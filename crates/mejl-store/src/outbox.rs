//! The outbox: durable user intent, applied optimistically.
//!
//! # Overlay technique
//!
//! Server truth stays in the cache tables (`srv_keywords_json` and
//! `srv_mailbox_ids_json` on each email row, and the masked email, contact
//! and mailbox tables). The effective state the UI reads (the flag columns,
//! `keywords_json` and `email_mailbox`) is server truth with every pending
//! patch applied, and is rewritten:
//!
//! - when an entry is enqueued, for its target emails;
//! - in every sync batch, for every email the batch touched, so a
//!   `/changes` result that arrives before the flush cannot make a row
//!   flicker back;
//! - when an entry fails or is cancelled, so the row shows server truth
//!   again.
//!
//! Materialising the overlay keeps page reads plain indexed range scans: a
//! pending move changes which rows belong to a mailbox, which read-time
//! patching could not do cheaply under offset paging.
//!
//! `state.db` is the source of truth for entries. `cache.db` keeps a mirror
//! of the entries whose overlay is active (the `overlay` table), so a cache
//! transaction never reads the other file. The two files cannot share a
//! transaction in WAL mode, so the mirror and every overlaid row are
//! rebuilt from `state.db` on each [`Store::open`].
//!
//! # Lifecycle
//!
//! `pending -> in_flight -> done | failed`, plus `cancelled`. A done entry
//! keeps its overlay until the next server data for its target arrives
//! (next email batch that touches the email, next mailbox, masked email or
//! contact refresh), or at once when cached server truth already reflects
//! it. A failed or cancelled entry drops its overlay immediately.
//!
//! The flusher must not apply server data that was requested before a flush
//! response it already reported with [`Store::finish_flush`]: such stale
//! data would confirm a done entry early and show the old state until the
//! next sync.
//!
//! Masked email and contact creates and updates show through read-time
//! overlays in the readers. Contact updates and destroys, and sends, have
//! no overlay.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::Timestamp;
use mejl_jmap::types::{ContactCard, Mailbox, MaskedEmail};
use mejl_jmap::{Id, Patch};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::codec::{from_json, json, ts_ms};
use crate::derive::{self, Dirty, EmailOp, kind as mirror};
use crate::error::{Error, Result};
use crate::overlay::{references_mailbox, rename_mailbox, untracked_paths};
use crate::store::Store;

/// How many actions [`Store::undo`] can step back through.
pub const UNDO_DEPTH: usize = 10;

/// An outbox sequence number. Later entries have larger numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Seq(pub i64);

/// Stored kind names.
mod kind {
    pub const EMAIL_PATCH: &str = "email_patch";
    pub const EMAIL_DESTROY: &str = "email_destroy";
    pub const MAILBOX_CREATE: &str = "mailbox_create";
    pub const MASKED_EMAIL_SET: &str = "masked_email_set";
    pub const CONTACT_SET: &str = "contact_set";
    pub const EMAIL_SUBMIT: &str = "email_create_and_submit";
}

/// A patch to one email.
#[derive(Debug, Clone, PartialEq)]
pub struct EmailPatchTarget {
    /// The email to change.
    pub email_id: Id,
    /// Keyword and mailbox changes, such as `keywords/$seen: true`.
    pub patch: Patch,
    /// The patch that undoes this one. `None` lets the store compute it
    /// from the email's current effective state.
    pub inverse: Option<Patch>,
}

impl EmailPatchTarget {
    /// A target whose inverse the store computes.
    pub fn new(email_id: impl Into<Id>, patch: Patch) -> Self {
        Self {
            email_id: email_id.into(),
            patch,
            inverse: None,
        }
    }
}

/// A change the user made, to be sent to the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Mutation {
    /// Keyword and mailbox patches, one per email. Archiving 20 messages is
    /// one mutation with 20 targets.
    EmailPatch(Vec<EmailPatchTarget>),
    /// Permanent deletion. Not undoable.
    EmailDestroy(Vec<Id>),
    /// A new mailbox. Email patches may move mail into it right away by
    /// using the returned [`Enqueued::temp_id`] as the mailbox id; they are
    /// held back until the create succeeds and then rewritten to the real id.
    MailboxCreate(Mailbox),
    /// A new masked address. The address is server-generated, so callers
    /// should only queue this while online.
    MaskedEmailCreate(MaskedEmail),
    /// A change to a masked address, such as its state or description.
    /// Only top-level properties are shown in the overlay.
    MaskedEmailUpdate {
        /// The masked email id.
        id: Id,
        /// Top-level property changes, such as `state: "disabled"`.
        patch: Patch,
        /// The patch that undoes this one. `None` lets the store compute
        /// it from the cached mask.
        inverse: Option<Patch>,
    },
    /// A new contact card, for example "allow this sender".
    ContactCreate(ContactCard),
    /// A change to a contact card. No overlay.
    ContactUpdate {
        /// The contact card id.
        id: Id,
        /// The JMAP patch.
        patch: Patch,
        /// The patch that undoes this one. `None` lets the store compute
        /// it for top-level properties of the cached card.
        inverse: Option<Patch>,
    },
    /// Deleting a contact card. No overlay, not undoable.
    ContactDestroy(Id),
    /// Creating an email and submitting it. The payload is whatever the
    /// sync engine needs to build the request; the store keeps it opaque.
    /// It should carry a client-made Message-ID, so the engine can find an
    /// earlier copy before resending an entry marked
    /// [`FlushUnit::interrupted`].
    EmailSubmit {
        /// The engine's description of the email and submission.
        payload: Value,
        /// Undo send: the entry is not handed to the flusher before this.
        hold_until: Option<Timestamp>,
    },
}

/// Options for [`Store::enqueue`].
#[derive(Debug, Clone, Copy, Default)]
pub struct EnqueueOptions<'a> {
    /// Shown for undo and in failure toasts, for example "Archive 3".
    pub label: Option<&'a str>,
    /// Put the action on the undo stack.
    pub undoable: bool,
}

/// What [`Store::enqueue`] created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueued {
    /// The new entry.
    pub seq: Seq,
    /// The local placeholder id for a created object, such as
    /// `local:12`. It never collides with a server id, because JMAP ids
    /// cannot contain `:`.
    pub temp_id: Option<String>,
}

/// One unit of work for the flusher.
#[derive(Debug, Clone, PartialEq)]
pub struct FlushUnit {
    /// The entries this unit completes. Pass the unit back to
    /// [`Store::finish_flush`].
    pub seqs: Vec<Seq>,
    /// The call to make.
    pub op: FlushOp,
    /// The highest attempt number among the entries, starting at 1.
    pub attempt: u32,
    /// An entry was in flight when mejl last exited. The request may have
    /// reached the server. For a send, check before sending again.
    pub interrupted: bool,
}

/// The JMAP call a [`FlushUnit`] stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum FlushOp {
    /// One `Email/set`. Repeated patches to an email are collapsed into
    /// one, and emails whose net change is nothing are left out. The
    /// engine may split it into several calls of `maxObjectsInSet`.
    EmailSet {
        /// Patches by email id.
        update: BTreeMap<Id, Patch>,
        /// Emails to destroy.
        destroy: Vec<Id>,
    },
    /// One `Mailbox/set` create.
    MailboxCreate {
        /// The creation id, which is also the local placeholder id.
        creation_id: String,
        /// The create object.
        mailbox: Mailbox,
    },
    /// One `MaskedEmail/set` create.
    MaskedEmailCreate {
        /// The creation id, which is also the local placeholder id.
        creation_id: String,
        /// The create object.
        masked: MaskedEmail,
    },
    /// One `MaskedEmail/set` update.
    MaskedEmailUpdate {
        /// The masked email id.
        id: Id,
        /// The patch.
        patch: Patch,
    },
    /// One `ContactCard/set` create.
    ContactCreate {
        /// The creation id, which is also the local placeholder id.
        creation_id: String,
        /// The create object.
        card: ContactCard,
    },
    /// One `ContactCard/set` update.
    ContactUpdate {
        /// The contact card id.
        id: Id,
        /// The patch.
        patch: Patch,
    },
    /// One `ContactCard/set` destroy.
    ContactDestroy {
        /// The contact card id.
        id: Id,
    },
    /// `Email/set` create plus `EmailSubmission/set`, built by the engine.
    EmailSubmit {
        /// The payload given to [`Mutation::EmailSubmit`].
        payload: Value,
    },
}

/// How a flush went.
#[derive(Debug, Clone, PartialEq)]
pub enum FlushOutcome {
    /// The server processed the call.
    Applied {
        /// The server id of a created object. Required for creates.
        created_id: Option<Id>,
        /// Objects the server rejected (`notUpdated`, `notDestroyed`), with
        /// a description. Report `notFound` on a destroy as success.
        rejected: BTreeMap<Id, String>,
    },
    /// Nothing was applied, and trying again later may work: offline, a
    /// timeout, HTTP 429 or 503, `serverUnavailable`.
    Retry {
        /// What went wrong, kept as the entry's last error.
        error: String,
        /// Do not try again before this.
        not_before: Option<Timestamp>,
    },
    /// Nothing was applied and retrying will not help.
    Failed {
        /// What went wrong, for the toast.
        error: String,
    },
}

impl FlushOutcome {
    /// Success with no rejections and no created id.
    pub fn ok() -> Self {
        FlushOutcome::Applied {
            created_id: None,
            rejected: BTreeMap::new(),
        }
    }

    /// Success for a create.
    pub fn created(id: impl Into<Id>) -> Self {
        FlushOutcome::Applied {
            created_id: Some(id.into()),
            rejected: BTreeMap::new(),
        }
    }
}

/// What [`Store::finish_flush`] concluded, for toasts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FinishReport {
    /// Entries that succeeded, at least in part.
    pub done: Vec<Seq>,
    /// Entries with failures, in whole or in part.
    pub failed: Vec<FailedEntry>,
}

/// An entry that failed in whole or in part.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedEntry {
    /// The entry.
    pub seq: Seq,
    /// The label it was queued with.
    pub label: Option<String>,
    /// The first error reported for it.
    pub error: String,
    /// Targets the server rejected.
    pub failed_targets: Vec<Id>,
    /// Every target failed, so the entry as a whole failed.
    pub entry_failed: bool,
}

/// What [`Store::undo`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoOutcome {
    /// The inverse was queued as a new entry. If the original was still
    /// pending, the two collapse to nothing at flush time.
    Reverted {
        /// The undone action's label.
        label: String,
        /// The undone entry.
        original: Seq,
        /// The new entry holding the inverse.
        seq: Seq,
    },
    /// The original had not been sent yet and was cancelled. For a send,
    /// `payload` is the submit payload so compose can reopen it.
    Cancelled {
        /// The undone action's label.
        label: String,
        /// The cancelled entry.
        seq: Seq,
        /// The create object or submit payload.
        payload: Option<Value>,
    },
    /// The original is already sent or has no inverse.
    TooLate {
        /// The action's label.
        label: String,
        /// Its entry.
        seq: Seq,
    },
}

/// What [`Store::redo`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum RedoOutcome {
    /// The undone action was queued again as a new entry, which is back on
    /// the undo stack.
    Reapplied {
        /// The action's label.
        label: String,
        /// The entry that held the undo.
        undone: Seq,
        /// The new entry that applies the action again.
        seq: Seq,
    },
    /// The undo failed or was cancelled, so there is nothing to apply again.
    TooLate {
        /// The action's label.
        label: String,
        /// The entry that held the undo.
        seq: Seq,
    },
}

/// What [`Store::cancel`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    /// Cancelled. The payload of a create or send is returned.
    Cancelled {
        /// The create object or submit payload, if the entry had one.
        payload: Option<Value>,
    },
    /// Already in flight or finished.
    TooLate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Payload {
    Patch,
    Destroy,
    MailboxCreate { mailbox: Value },
    MaskedCreate { masked: Value },
    MaskedUpdate,
    ContactCreate { card: Value },
    ContactUpdate,
    ContactDestroy,
    Submit { payload: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UndoKind {
    Inverse,
    Cancel,
    None,
}

impl Payload {
    fn undo_kind(&self) -> UndoKind {
        match self {
            Payload::Patch | Payload::MaskedUpdate | Payload::ContactUpdate => UndoKind::Inverse,
            Payload::MailboxCreate { .. }
            | Payload::MaskedCreate { .. }
            | Payload::ContactCreate { .. }
            | Payload::Submit { .. } => UndoKind::Cancel,
            Payload::Destroy | Payload::ContactDestroy => UndoKind::None,
        }
    }

    fn creates(&self) -> bool {
        matches!(
            self,
            Payload::MailboxCreate { .. }
                | Payload::MaskedCreate { .. }
                | Payload::ContactCreate { .. }
        )
    }

    fn returned_payload(&self) -> Option<Value> {
        match self {
            Payload::MailboxCreate { mailbox } => Some(mailbox.clone()),
            Payload::MaskedCreate { masked } => Some(masked.clone()),
            Payload::ContactCreate { card } => Some(card.clone()),
            Payload::Submit { payload } => Some(payload.clone()),
            _ => None,
        }
    }
}

/// The overlay mirror row for an outbox target: its mirror kind and
/// payload, or `None` if the entry kind has no overlay.
pub(crate) fn mirror_entry(
    outbox_kind: &str,
    payload: Option<&str>,
    patch: Option<&str>,
) -> Result<Option<(&'static str, Option<String>)>> {
    let payload: Payload = match payload {
        Some(p) => from_json(p)?,
        None => {
            return Err(Error::Corrupt(format!(
                "outbox {outbox_kind} entry without payload"
            )));
        }
    };
    Ok(match (outbox_kind, payload) {
        (kind::EMAIL_PATCH, _) => Some((mirror::EMAIL_PATCH, patch.map(str::to_owned))),
        (kind::EMAIL_DESTROY, _) => Some((mirror::EMAIL_DESTROY, None)),
        (kind::MAILBOX_CREATE, Payload::MailboxCreate { mailbox }) => {
            Some((mirror::MAILBOX_CREATE, Some(json(&mailbox)?)))
        }
        (kind::MASKED_EMAIL_SET, Payload::MaskedCreate { masked }) => {
            Some((mirror::MASKED_CREATE, Some(json(&masked)?)))
        }
        (kind::MASKED_EMAIL_SET, Payload::MaskedUpdate) => {
            Some((mirror::MASKED_UPDATE, patch.map(str::to_owned)))
        }
        (kind::CONTACT_SET, Payload::ContactCreate { card }) => {
            Some((mirror::CONTACT_CREATE, Some(json(&card)?)))
        }
        _ => None,
    })
}

fn is_email_kind(kind: &str) -> bool {
    kind == kind::EMAIL_PATCH || kind == kind::EMAIL_DESTROY
}

struct TargetRow {
    target_id: String,
    patch: Option<Patch>,
    inverse: Option<Patch>,
}

struct Plan {
    kind: &'static str,
    payload: Payload,
    targets: Vec<TargetRow>,
    hold_until: Option<i64>,
}

struct PendingEntry {
    seq: i64,
    kind: String,
    payload: Payload,
    hold_until: Option<i64>,
    not_before: Option<i64>,
    attempts: i64,
    interrupted: bool,
    temp_id: Option<String>,
    targets: Vec<TargetRow>,
}

/// Marks targets confirmed in `state.db` and turns off the overlay of
/// entries with no active target left.
pub(crate) fn confirm_in_state<'t>(
    state: &mut Connection,
    items: impl IntoIterator<Item = (i64, &'t str)>,
) -> Result<()> {
    let tx = state.transaction()?;
    for (seq, target) in items {
        tx.prepare_cached(
            "UPDATE outbox_target SET state = 'confirmed'
             WHERE seq = ?1 AND target_id = ?2 AND state = 'active'",
        )?
        .execute(params![seq, target])?;
        tx.prepare_cached(
            "UPDATE outbox SET overlay_active = 0 WHERE seq = ?1
             AND NOT EXISTS (SELECT 1 FROM outbox_target WHERE seq = ?1 AND state = 'active')",
        )?
        .execute([seq])?;
    }
    tx.commit()?;
    Ok(())
}

fn top_level_inverse(current: &Value, patch: &Patch) -> Patch {
    let mut out = Patch::new();
    for path in patch.as_map().keys() {
        if path.contains('/') {
            continue;
        }
        let value = current.get(path.as_str()).cloned().unwrap_or(Value::Null);
        out.insert_raw(path.clone(), value);
    }
    out
}

fn load_targets(state: &Connection, seq: i64) -> Result<Vec<TargetRow>> {
    let mut stmt = state.prepare_cached(
        "SELECT target_id, patch_json, inverse_json FROM outbox_target
         WHERE seq = ?1 AND state = 'active' ORDER BY target_id",
    )?;
    let rows = stmt.query_map([seq], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (target_id, patch, inverse) = row?;
        out.push(TargetRow {
            target_id,
            patch: patch.as_deref().map(from_json).transpose()?,
            inverse: inverse.as_deref().map(from_json).transpose()?,
        });
    }
    Ok(out)
}

impl Store {
    /// Queues a mutation and applies its overlay. Returns its sequence
    /// number.
    ///
    /// The entry is durable once this returns. If refreshing the cache
    /// afterwards fails, the error is returned but the entry stays queued,
    /// and its overlay is rebuilt on the next open.
    ///
    /// A new action starts a new history, so this clears the redo stack.
    pub fn enqueue(
        &mut self,
        mutation: Mutation,
        options: EnqueueOptions<'_>,
        now: Timestamp,
    ) -> Result<Enqueued> {
        self.enqueue_inner(mutation, options, None, now)
    }

    fn enqueue_inner(
        &mut self,
        mutation: Mutation,
        options: EnqueueOptions<'_>,
        undo_of: Option<i64>,
        now: Timestamp,
    ) -> Result<Enqueued> {
        let plan = self.plan(mutation)?;
        if options.undoable && plan.payload.undo_kind() == UndoKind::None {
            return Err(Error::NotUndoable);
        }
        let payload_json = json(&plan.payload)?;
        let tx = self.state.transaction()?;
        tx.execute(
            "INSERT INTO outbox (kind, created_at, status, hold_until, payload_json, label,
                 overlay_active, undo_of)
             VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6, ?7)",
            params![
                plan.kind,
                ts_ms(now),
                plan.hold_until,
                payload_json,
                options.label,
                false,
                undo_of
            ],
        )?;
        let seq = tx.last_insert_rowid();
        let temp_id = plan.payload.creates().then(|| format!("local:{seq}"));
        if let Some(temp) = &temp_id {
            tx.execute(
                "UPDATE outbox SET temp_id = ?2 WHERE seq = ?1",
                params![seq, temp],
            )?;
        }
        let mut mirror_rows: Vec<(String, &'static str, Option<String>)> = Vec::new();
        for target in &plan.targets {
            let target_id = if target.target_id.is_empty() {
                temp_id.clone().unwrap_or_default()
            } else {
                target.target_id.clone()
            };
            let patch_json = target.patch.as_ref().map(json).transpose()?;
            tx.execute(
                "INSERT INTO outbox_target (seq, target_id, patch_json, inverse_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    seq,
                    target_id,
                    patch_json,
                    target.inverse.as_ref().map(json).transpose()?
                ],
            )?;
            if let Some((mk, payload)) =
                mirror_entry(plan.kind, Some(&payload_json), patch_json.as_deref())?
            {
                mirror_rows.push((target_id, mk, payload));
            }
        }
        if !mirror_rows.is_empty() {
            tx.execute("UPDATE outbox SET overlay_active = 1 WHERE seq = ?1", [seq])?;
        }
        if options.undoable {
            tx.execute(
                "INSERT INTO undo (outbox_seq, label, created_at) VALUES (?1, ?2, ?3)",
                params![seq, options.label.unwrap_or(plan.kind), ts_ms(now)],
            )?;
            tx.execute(
                "DELETE FROM undo WHERE id NOT IN (SELECT id FROM undo ORDER BY id DESC LIMIT ?1)",
                [crate::codec::usize_i64(UNDO_DEPTH)],
            )?;
        }
        if undo_of.is_none() {
            // A new user action, not an undo or redo: it starts a new
            // history, so whatever was undone can no longer be redone.
            tx.execute("DELETE FROM redo", [])?;
        }
        tx.commit()?;

        if !mirror_rows.is_empty() {
            let tx = self.cache.transaction()?;
            let mut dirty = Dirty::default();
            for (target, mk, payload) in &mirror_rows {
                tx.execute(
                    "INSERT INTO overlay (target_id, seq, kind, payload_json) VALUES (?1, ?2, ?3, ?4)",
                    params![target, seq, mk, payload],
                )?;
                if *mk == mirror::EMAIL_PATCH || *mk == mirror::EMAIL_DESTROY {
                    derive::refresh_email(&tx, target, &mut dirty)?;
                }
            }
            derive::refresh_threads(&tx, &dirty.threads)?;
            tx.commit()?;
        }
        Ok(Enqueued {
            seq: Seq(seq),
            temp_id,
        })
    }

    fn plan(&self, mutation: Mutation) -> Result<Plan> {
        let plan = match mutation {
            Mutation::EmailPatch(targets) => {
                let mut seen = HashSet::new();
                let mut rows = Vec::new();
                for t in targets {
                    if t.email_id.is_empty() {
                        return Err(Error::Invalid("an email patch with an empty id".into()));
                    }
                    if !seen.insert(t.email_id.clone()) {
                        return Err(Error::Invalid(format!(
                            "email {} appears twice in one mutation",
                            t.email_id
                        )));
                    }
                    if t.patch.is_empty() {
                        continue;
                    }
                    let inverse = match t.inverse {
                        Some(inverse) => Some(inverse),
                        None => derive::effective_flags(&self.cache, t.email_id.as_str())?
                            .map(|f| f.inverse(&t.patch)),
                    };
                    rows.push(TargetRow {
                        target_id: t.email_id.into_inner(),
                        patch: Some(t.patch),
                        inverse,
                    });
                }
                if rows.is_empty() {
                    return Err(Error::Invalid("the mutation changes nothing".into()));
                }
                Plan {
                    kind: kind::EMAIL_PATCH,
                    payload: Payload::Patch,
                    targets: rows,
                    hold_until: None,
                }
            }
            Mutation::EmailDestroy(ids) => {
                let mut seen = HashSet::new();
                let mut rows = Vec::new();
                for id in ids {
                    if id.is_empty() {
                        return Err(Error::Invalid("an email destroy with an empty id".into()));
                    }
                    if seen.insert(id.clone()) {
                        rows.push(TargetRow {
                            target_id: id.into_inner(),
                            patch: None,
                            inverse: None,
                        });
                    }
                }
                if rows.is_empty() {
                    return Err(Error::Invalid("nothing to destroy".into()));
                }
                Plan {
                    kind: kind::EMAIL_DESTROY,
                    payload: Payload::Destroy,
                    targets: rows,
                    hold_until: None,
                }
            }
            Mutation::MailboxCreate(mailbox) => {
                if mailbox.name.as_deref().is_none_or(|n| n.trim().is_empty()) {
                    return Err(Error::Invalid("a new mailbox needs a name".into()));
                }
                Plan {
                    kind: kind::MAILBOX_CREATE,
                    payload: Payload::MailboxCreate {
                        mailbox: serde_json::to_value(&mailbox)?,
                    },
                    targets: vec![temp_target()],
                    hold_until: None,
                }
            }
            Mutation::MaskedEmailCreate(masked) => Plan {
                kind: kind::MASKED_EMAIL_SET,
                payload: Payload::MaskedCreate {
                    masked: serde_json::to_value(&masked)?,
                },
                targets: vec![temp_target()],
                hold_until: None,
            },
            Mutation::MaskedEmailUpdate { id, patch, inverse } => {
                check_update(&id, &patch)?;
                let inverse = match inverse {
                    Some(inverse) => Some(inverse),
                    None => crate::read::masked_effective_json(&self.cache, id.as_str())?
                        .map(|current| top_level_inverse(&current, &patch)),
                };
                Plan {
                    kind: kind::MASKED_EMAIL_SET,
                    payload: Payload::MaskedUpdate,
                    targets: vec![TargetRow {
                        target_id: id.into_inner(),
                        patch: Some(patch),
                        inverse,
                    }],
                    hold_until: None,
                }
            }
            Mutation::ContactCreate(card) => Plan {
                kind: kind::CONTACT_SET,
                payload: Payload::ContactCreate {
                    card: serde_json::to_value(&card)?,
                },
                targets: vec![temp_target()],
                hold_until: None,
            },
            Mutation::ContactUpdate { id, patch, inverse } => {
                check_update(&id, &patch)?;
                let inverse = match inverse {
                    Some(inverse) => Some(inverse),
                    None => {
                        let stored: Option<String> = self
                            .cache
                            .query_row(
                                "SELECT json FROM contact WHERE id = ?1",
                                [id.as_str()],
                                |r| r.get(0),
                            )
                            .optional()?;
                        stored
                            .as_deref()
                            .map(from_json::<Value>)
                            .transpose()?
                            .map(|current| top_level_inverse(&current, &patch))
                    }
                };
                Plan {
                    kind: kind::CONTACT_SET,
                    payload: Payload::ContactUpdate,
                    targets: vec![TargetRow {
                        target_id: id.into_inner(),
                        patch: Some(patch),
                        inverse,
                    }],
                    hold_until: None,
                }
            }
            Mutation::ContactDestroy(id) => {
                if id.is_empty() {
                    return Err(Error::Invalid("a contact destroy with an empty id".into()));
                }
                Plan {
                    kind: kind::CONTACT_SET,
                    payload: Payload::ContactDestroy,
                    targets: vec![TargetRow {
                        target_id: id.into_inner(),
                        patch: None,
                        inverse: None,
                    }],
                    hold_until: None,
                }
            }
            Mutation::EmailSubmit {
                payload,
                hold_until,
            } => Plan {
                kind: kind::EMAIL_SUBMIT,
                payload: Payload::Submit { payload },
                targets: Vec::new(),
                hold_until: hold_until.map(ts_ms),
            },
        };
        Ok(plan)
    }

    /// The next unit of work for the flusher, or `None` when nothing is
    /// ready. The unit's entries move to `in_flight`.
    ///
    /// Entries go out in sequence order with these rules:
    ///
    /// - held sends and entries in retry backoff wait for their time;
    /// - email patches and destroys are gathered into one `Email/set`, up
    ///   to about `max_objects` emails, collapsed per email against the
    ///   best known server state, with no-op emails dropped (entries that
    ///   collapse to nothing complete at once without a request);
    /// - an email entry waits while an earlier entry for the same email is
    ///   waiting or in flight, and while it moves mail into a mailbox whose
    ///   create has not succeeded yet;
    /// - every other kind goes out on its own.
    pub fn next_flush(&mut self, now: Timestamp, max_objects: usize) -> Result<Option<FlushUnit>> {
        let now_ms = ts_ms(now);
        let max_objects = max_objects.max(1);
        loop {
            let pending = self.load_pending()?;
            if pending.is_empty() {
                return Ok(None);
            }
            let mut blocked: HashSet<String> = HashSet::new();
            {
                let mut stmt = self.state.prepare_cached(
                    "SELECT t.target_id FROM outbox_target t JOIN outbox o ON o.seq = t.seq
                     WHERE o.status = 'in_flight' AND o.kind IN ('email_patch', 'email_destroy')",
                )?;
                for id in stmt.query_map([], |r| r.get::<_, String>(0))? {
                    blocked.insert(id?);
                }
            }
            let unresolved: Vec<String> = {
                let mut stmt = self.state.prepare_cached(
                    "SELECT temp_id FROM outbox WHERE kind = 'mailbox_create'
                     AND status IN ('pending', 'in_flight') AND temp_id IS NOT NULL",
                )?;
                stmt.query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };

            let mut chosen: Vec<&PendingEntry> = Vec::new();
            let mut objects: BTreeSet<&str> = BTreeSet::new();
            let mut single: Option<&PendingEntry> = None;
            for entry in &pending {
                let ready = entry.hold_until.is_none_or(|h| h <= now_ms)
                    && entry.not_before.is_none_or(|n| n <= now_ms);
                if !is_email_kind(&entry.kind) {
                    if ready && chosen.is_empty() {
                        single = Some(entry);
                        break;
                    }
                    continue;
                }
                let depends = entry.targets.iter().any(|t| {
                    t.patch
                        .as_ref()
                        .is_some_and(|p| unresolved.iter().any(|u| references_mailbox(p, u)))
                });
                let waits = entry.targets.iter().any(|t| blocked.contains(&t.target_id));
                if !ready || depends || waits {
                    blocked.extend(entry.targets.iter().map(|t| t.target_id.clone()));
                    continue;
                }
                if !chosen.is_empty() && objects.len() >= max_objects {
                    break;
                }
                objects.extend(entry.targets.iter().map(|t| t.target_id.as_str()));
                chosen.push(entry);
            }
            if let Some(entry) = single {
                return self.take_single(entry).map(Some);
            }
            if chosen.is_empty() {
                return Ok(None);
            }
            let (update, destroy) = self.collapse(&chosen)?;
            let seqs: Vec<i64> = chosen.iter().map(|e| e.seq).collect();
            if update.is_empty() && destroy.is_empty() {
                self.complete_without_request(&seqs, now)?;
                continue;
            }
            let attempt = chosen.iter().map(|e| e.attempts).max().unwrap_or(0) + 1;
            let interrupted = chosen.iter().any(|e| e.interrupted);
            self.mark_in_flight(&seqs)?;
            return Ok(Some(FlushUnit {
                seqs: seqs.into_iter().map(Seq).collect(),
                op: FlushOp::EmailSet { update, destroy },
                attempt: u32::try_from(attempt).unwrap_or(u32::MAX),
                interrupted,
            }));
        }
    }

    fn load_pending(&self) -> Result<Vec<PendingEntry>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT seq, kind, payload_json, hold_until, not_before, attempts, interrupted, temp_id
             FROM outbox WHERE status = 'pending' ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, kind, payload, hold_until, not_before, attempts, interrupted, temp_id) = row?;
            let payload: Payload = match payload {
                Some(p) => from_json(&p)?,
                None => return Err(Error::Corrupt(format!("outbox entry {seq} has no payload"))),
            };
            out.push(PendingEntry {
                seq,
                kind,
                payload,
                hold_until,
                not_before,
                attempts,
                interrupted,
                temp_id,
                targets: load_targets(&self.state, seq)?,
            });
        }
        Ok(out)
    }

    /// Collapses the chosen email entries into one update map and destroy
    /// list.
    fn collapse(&self, chosen: &[&PendingEntry]) -> Result<(BTreeMap<Id, Patch>, Vec<Id>)> {
        let mut per_email: BTreeMap<&str, Vec<EmailOp>> = BTreeMap::new();
        for entry in chosen {
            for t in &entry.targets {
                let op = match (&*entry.kind, &t.patch) {
                    (kind::EMAIL_DESTROY, _) => EmailOp::Destroy,
                    (_, Some(p)) => EmailOp::Patch(p.clone()),
                    (_, None) => continue,
                };
                per_email.entry(&t.target_id).or_default().push(op);
            }
        }
        let mut update = BTreeMap::new();
        let mut destroy = Vec::new();
        for (email, ops) in per_email {
            if ops.iter().any(|op| matches!(op, EmailOp::Destroy)) {
                destroy.push(Id::from(email));
                continue;
            }
            let patches = ops.iter().filter_map(|op| match op {
                EmailOp::Patch(p) => Some(p),
                EmailOp::Destroy => None,
            });
            let patch = match derive::load_core(&self.cache, email)? {
                Some(core) => {
                    // The best guess of what the server has now: its last
                    // reported state plus what was sent but not yet seen.
                    let mut base = core.srv;
                    for (_, done, op) in derive::email_ops(&self.cache, email)? {
                        if let (true, EmailOp::Patch(p)) = (done, op) {
                            base.apply(&p);
                        }
                    }
                    let mut target = base.clone();
                    let mut other = Map::new();
                    for p in patches {
                        target.apply(p);
                        other.extend(untracked_paths(p));
                    }
                    let mut patch = base.diff(&target);
                    for (path, value) in other {
                        patch.insert_raw(path, value);
                    }
                    patch
                }
                None => {
                    let mut merged = Patch::new();
                    for p in patches {
                        merged.merge(p.clone());
                    }
                    merged
                }
            };
            if !patch.is_empty() {
                update.insert(Id::from(email), patch);
            }
        }
        Ok((update, destroy))
    }

    fn mark_in_flight(&mut self, seqs: &[i64]) -> Result<()> {
        let tx = self.state.transaction()?;
        for seq in seqs {
            tx.execute(
                "UPDATE outbox SET status = 'in_flight', attempts = attempts + 1, not_before = NULL
                 WHERE seq = ?1",
                [seq],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn take_single(&mut self, entry: &PendingEntry) -> Result<FlushUnit> {
        let target = entry.targets.first();
        let target_id = || -> Result<Id> {
            target
                .map(|t| Id::from(t.target_id.as_str()))
                .ok_or_else(|| Error::Corrupt(format!("outbox entry {} has no target", entry.seq)))
        };
        let target_patch = || -> Result<Patch> {
            target
                .and_then(|t| t.patch.clone())
                .ok_or_else(|| Error::Corrupt(format!("outbox entry {} has no patch", entry.seq)))
        };
        let creation_id = || {
            entry
                .temp_id
                .clone()
                .unwrap_or_else(|| format!("local:{}", entry.seq))
        };
        let op = match &entry.payload {
            Payload::MailboxCreate { mailbox } => FlushOp::MailboxCreate {
                creation_id: creation_id(),
                mailbox: serde_json::from_value(mailbox.clone())?,
            },
            Payload::MaskedCreate { masked } => FlushOp::MaskedEmailCreate {
                creation_id: creation_id(),
                masked: serde_json::from_value(masked.clone())?,
            },
            Payload::MaskedUpdate => FlushOp::MaskedEmailUpdate {
                id: target_id()?,
                patch: target_patch()?,
            },
            Payload::ContactCreate { card } => FlushOp::ContactCreate {
                creation_id: creation_id(),
                card: serde_json::from_value(card.clone())?,
            },
            Payload::ContactUpdate => FlushOp::ContactUpdate {
                id: target_id()?,
                patch: target_patch()?,
            },
            Payload::ContactDestroy => FlushOp::ContactDestroy { id: target_id()? },
            Payload::Submit { payload } => FlushOp::EmailSubmit {
                payload: payload.clone(),
            },
            Payload::Patch | Payload::Destroy => {
                return Err(Error::Corrupt(format!(
                    "outbox entry {} has an email payload but kind {}",
                    entry.seq, entry.kind
                )));
            }
        };
        self.mark_in_flight(&[entry.seq])?;
        Ok(FlushUnit {
            seqs: vec![Seq(entry.seq)],
            op,
            attempt: u32::try_from(entry.attempts + 1).unwrap_or(u32::MAX),
            interrupted: entry.interrupted,
        })
    }

    /// Entries that collapsed to nothing: done and confirmed without a
    /// request, since the server already has the net result.
    fn complete_without_request(&mut self, seqs: &[i64], now: Timestamp) -> Result<()> {
        let mut targets: Vec<(i64, String)> = Vec::new();
        {
            let tx = self.state.transaction()?;
            for seq in seqs {
                for t in load_targets(&tx, *seq)? {
                    targets.push((*seq, t.target_id));
                }
                tx.execute(
                    "UPDATE outbox SET status = 'done', finished_at = ?2 WHERE seq = ?1",
                    params![seq, ts_ms(now)],
                )?;
            }
            tx.commit()?;
        }
        self.drop_mirror_rows(&targets)?;
        confirm_in_state(
            &mut self.state,
            targets.iter().map(|(s, t)| (*s, t.as_str())),
        )
    }

    /// Removes mirror rows and re-derives the affected emails.
    fn drop_mirror_rows(&mut self, rows: &[(i64, String)]) -> Result<()> {
        let tx = self.cache.transaction()?;
        let mut dirty = Dirty::default();
        for (seq, target) in rows {
            tx.prepare_cached("DELETE FROM overlay WHERE target_id = ?1 AND seq = ?2")?
                .execute(params![target, seq])?;
            derive::refresh_email(&tx, target, &mut dirty)?;
        }
        derive::refresh_threads(&tx, &dirty.threads)?;
        tx.commit()?;
        Ok(())
    }

    /// Records how a flush unit went. Every seq in the unit must still be
    /// in flight.
    pub fn finish_flush(
        &mut self,
        unit: &FlushUnit,
        outcome: FlushOutcome,
        now: Timestamp,
    ) -> Result<FinishReport> {
        for seq in &unit.seqs {
            let status: Option<String> = self
                .state
                .query_row("SELECT status FROM outbox WHERE seq = ?1", [seq.0], |r| {
                    r.get(0)
                })
                .optional()?;
            if status.as_deref() != Some("in_flight") {
                return Err(Error::UnknownEntry(seq.0));
            }
        }
        let seqs: Vec<i64> = unit.seqs.iter().map(|s| s.0).collect();
        let Some(&first) = seqs.first() else {
            return Err(Error::Invalid("a flush unit with no entries".into()));
        };
        if !matches!(unit.op, FlushOp::EmailSet { .. }) && seqs.len() != 1 {
            return Err(Error::Invalid(
                "a single-object flush unit must hold exactly one entry".into(),
            ));
        }
        match outcome {
            FlushOutcome::Retry { error, not_before } => {
                let tx = self.state.transaction()?;
                for seq in &seqs {
                    tx.execute(
                        "UPDATE outbox SET status = 'pending', last_error = ?2, not_before = ?3
                         WHERE seq = ?1",
                        params![seq, error, not_before.map(ts_ms)],
                    )?;
                }
                tx.commit()?;
                Ok(FinishReport::default())
            }
            FlushOutcome::Failed { error } => self.fail_entries(&seqs, &error, now),
            FlushOutcome::Applied {
                created_id,
                rejected,
            } => match &unit.op {
                FlushOp::EmailSet { update, destroy } => {
                    self.finish_email_set(&seqs, update, destroy, &rejected, now)
                }
                FlushOp::MailboxCreate { .. }
                | FlushOp::MaskedEmailCreate { .. }
                | FlushOp::ContactCreate { .. } => {
                    let Some(created) = created_id else {
                        return Err(Error::Invalid(
                            "a successful create needs the created id".into(),
                        ));
                    };
                    self.finish_create(first, &created, now)
                }
                FlushOp::MaskedEmailUpdate { .. }
                | FlushOp::ContactUpdate { .. }
                | FlushOp::ContactDestroy { .. }
                | FlushOp::EmailSubmit { .. } => {
                    if let Some(error) = rejected.values().next() {
                        return self.fail_entries(&seqs, error, now);
                    }
                    self.finish_simple(first, now)
                }
            },
        }
    }

    fn finish_email_set(
        &mut self,
        seqs: &[i64],
        update: &BTreeMap<Id, Patch>,
        destroy: &[Id],
        rejected: &BTreeMap<Id, String>,
        now: Timestamp,
    ) -> Result<FinishReport> {
        let sent: BTreeSet<&str> = update
            .keys()
            .map(Id::as_str)
            .chain(destroy.iter().map(Id::as_str))
            .collect();
        let mut report = FinishReport::default();
        let mut accepted: Vec<(i64, String)> = Vec::new();
        let mut dropped: Vec<(i64, String)> = Vec::new();
        {
            let tx = self.state.transaction()?;
            for &seq in seqs {
                let targets = load_targets(&tx, seq)?;
                let mut failed_targets = Vec::new();
                let mut first_error = None;
                for t in &targets {
                    if let Some(err) = rejected.get(t.target_id.as_str()) {
                        tx.execute(
                            "UPDATE outbox_target SET state = 'rejected', error = ?3
                             WHERE seq = ?1 AND target_id = ?2",
                            params![seq, t.target_id, err],
                        )?;
                        failed_targets.push(Id::from(t.target_id.as_str()));
                        first_error.get_or_insert_with(|| err.clone());
                        dropped.push((seq, t.target_id.clone()));
                    } else {
                        accepted.push((seq, t.target_id.clone()));
                    }
                }
                let entry_failed = !targets.is_empty() && failed_targets.len() == targets.len();
                tx.execute(
                    "UPDATE outbox SET status = ?2, last_error = ?3, finished_at = ?4 WHERE seq = ?1",
                    params![
                        seq,
                        if entry_failed { "failed" } else { "done" },
                        first_error,
                        ts_ms(now)
                    ],
                )?;
                if entry_failed {
                    tx.execute("UPDATE outbox SET overlay_active = 0 WHERE seq = ?1", [seq])?;
                } else {
                    report.done.push(Seq(seq));
                }
                if let Some(error) = first_error {
                    report.failed.push(FailedEntry {
                        seq: Seq(seq),
                        label: tx
                            .query_row("SELECT label FROM outbox WHERE seq = ?1", [seq], |r| {
                                r.get(0)
                            })
                            .optional()?
                            .flatten(),
                        error,
                        failed_targets,
                        entry_failed,
                    });
                }
            }
            tx.commit()?;
        }

        let mut confirmed: Vec<(i64, String)> = Vec::new();
        {
            let tx = self.cache.transaction()?;
            let mut dirty = Dirty::default();
            let mut emails: BTreeSet<String> = BTreeSet::new();
            for (seq, target) in &dropped {
                tx.prepare_cached("DELETE FROM overlay WHERE target_id = ?1 AND seq = ?2")?
                    .execute(params![target, seq])?;
                emails.insert(target.clone());
            }
            for (seq, target) in &accepted {
                if sent.contains(target.as_str()) {
                    tx.prepare_cached(
                        "UPDATE overlay SET done = 1 WHERE target_id = ?1 AND seq = ?2",
                    )?
                    .execute(params![target, seq])?;
                } else {
                    // Collapsed away: the server already has the net result.
                    tx.prepare_cached("DELETE FROM overlay WHERE target_id = ?1 AND seq = ?2")?
                        .execute(params![target, seq])?;
                    confirmed.push((*seq, target.clone()));
                }
                emails.insert(target.clone());
            }
            // A patch the cached server state already reflects needs no
            // later confirmation.
            for email in update.keys() {
                let Some(core) = derive::load_core(&tx, email.as_str())? else {
                    continue;
                };
                let ops = derive::email_ops(&tx, email.as_str())?;
                if ops.iter().any(|(_, done, _)| !done) {
                    continue;
                }
                let mut after = core.srv.clone();
                for (_, _, op) in &ops {
                    if let EmailOp::Patch(p) = op {
                        after.apply(p);
                    }
                }
                if after == core.srv {
                    for (seq, _, _) in &ops {
                        tx.prepare_cached("DELETE FROM overlay WHERE target_id = ?1 AND seq = ?2")?
                            .execute(params![email.as_str(), seq])?;
                        confirmed.push((*seq, email.as_str().to_owned()));
                    }
                }
            }
            for email in &emails {
                derive::refresh_email(&tx, email, &mut dirty)?;
            }
            derive::refresh_threads(&tx, &dirty.threads)?;
            tx.commit()?;
        }
        confirm_in_state(
            &mut self.state,
            confirmed.iter().map(|(s, t)| (*s, t.as_str())),
        )?;
        Ok(report)
    }

    fn finish_create(&mut self, seq: i64, created: &Id, now: Timestamp) -> Result<FinishReport> {
        if created.is_empty() {
            return Err(Error::Invalid("the created id is empty".into()));
        }
        let (kind, temp_id): (String, Option<String>) = self.state.query_row(
            "SELECT kind, temp_id FROM outbox WHERE seq = ?1",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut rewritten_emails: BTreeSet<String> = BTreeSet::new();
        {
            let tx = self.state.transaction()?;
            tx.execute(
                "UPDATE outbox SET status = 'done', result_id = ?2, finished_at = ?3 WHERE seq = ?1",
                params![seq, created.as_str(), ts_ms(now)],
            )?;
            if let (kind::MAILBOX_CREATE, Some(temp)) = (kind.as_str(), &temp_id) {
                let like = format!("%{}%", crate::codec::like_escape(temp));
                let mut rows: Vec<(i64, String, Option<String>, Option<String>)> = Vec::new();
                {
                    let mut stmt = tx.prepare(
                        "SELECT seq, target_id, patch_json, inverse_json FROM outbox_target
                         WHERE patch_json LIKE ?1 ESCAPE '\\' OR inverse_json LIKE ?1 ESCAPE '\\'",
                    )?;
                    for row in stmt.query_map([&like], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })? {
                        rows.push(row?);
                    }
                }
                for (target_seq, target, patch, inverse) in rows {
                    let rewrite = |text: Option<String>| -> Result<Option<String>> {
                        match text {
                            Some(t) => {
                                let p: Patch = from_json(&t)?;
                                Ok(Some(json(&rename_mailbox(&p, temp, created.as_str()))?))
                            }
                            None => Ok(None),
                        }
                    };
                    tx.execute(
                        "UPDATE outbox_target SET patch_json = ?3, inverse_json = ?4
                         WHERE seq = ?1 AND target_id = ?2",
                        params![target_seq, target, rewrite(patch)?, rewrite(inverse)?],
                    )?;
                    rewritten_emails.insert(target);
                }
            }
            tx.commit()?;
        }
        let tx = self.cache.transaction()?;
        tx.execute(
            "UPDATE overlay SET done = 1, result_id = ?2 WHERE seq = ?1",
            params![seq, created.as_str()],
        )?;
        let mut dirty = Dirty::default();
        if let (kind::MAILBOX_CREATE, Some(temp)) = (kind.as_str(), &temp_id) {
            let like = format!("%{}%", crate::codec::like_escape(temp));
            let mut rows: Vec<(String, i64, String)> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT target_id, seq, payload_json FROM overlay
                     WHERE kind = 'email_patch' AND payload_json LIKE ?1 ESCAPE '\\'",
                )?;
                for row in stmt.query_map([&like], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
                    rows.push(row?);
                }
            }
            for (target, target_seq, payload) in rows {
                let p: Patch = from_json(&payload)?;
                tx.execute(
                    "UPDATE overlay SET payload_json = ?3 WHERE target_id = ?1 AND seq = ?2",
                    params![
                        target,
                        target_seq,
                        json(&rename_mailbox(&p, temp, created.as_str()))?
                    ],
                )?;
                rewritten_emails.insert(target);
            }
        }
        for email in &rewritten_emails {
            derive::refresh_email(&tx, email, &mut dirty)?;
        }
        derive::refresh_threads(&tx, &dirty.threads)?;
        tx.commit()?;
        Ok(FinishReport {
            done: vec![Seq(seq)],
            failed: Vec::new(),
        })
    }

    fn finish_simple(&mut self, seq: i64, now: Timestamp) -> Result<FinishReport> {
        self.state.execute(
            "UPDATE outbox SET status = 'done', finished_at = ?2 WHERE seq = ?1",
            params![seq, ts_ms(now)],
        )?;
        self.cache
            .execute("UPDATE overlay SET done = 1 WHERE seq = ?1", [seq])?;
        Ok(FinishReport {
            done: vec![Seq(seq)],
            failed: Vec::new(),
        })
    }

    /// Fails entries, drops their overlays and fails email entries that
    /// depend on a mailbox one of them was creating.
    fn fail_entries(&mut self, seqs: &[i64], error: &str, now: Timestamp) -> Result<FinishReport> {
        self.end_entries(seqs, "failed", error, now)
    }

    fn end_entries(
        &mut self,
        seqs: &[i64],
        status: &str,
        error: &str,
        now: Timestamp,
    ) -> Result<FinishReport> {
        let mut report = FinishReport::default();
        let mut all: Vec<i64> = seqs.to_vec();
        let mut ended: Vec<(i64, String)> = Vec::new();
        let tx = self.state.transaction()?;
        let mut i = 0;
        while i < all.len() {
            let seq = all[i];
            i += 1;
            let (kind, temp_id): (String, Option<String>) = tx.query_row(
                "SELECT kind, temp_id FROM outbox WHERE seq = ?1",
                [seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let targets = load_targets(&tx, seq)?;
            let entry_status = if seqs.contains(&seq) {
                status
            } else {
                "failed"
            };
            let entry_error = if seqs.contains(&seq) {
                error.to_owned()
            } else {
                "the mailbox it moves mail into could not be created".to_owned()
            };
            tx.execute(
                "UPDATE outbox SET status = ?2, last_error = ?3, finished_at = ?4, overlay_active = 0
                 WHERE seq = ?1",
                params![seq, entry_status, entry_error, ts_ms(now)],
            )?;
            tx.execute(
                "UPDATE outbox_target SET state = 'rejected', error = ?2 WHERE seq = ?1 AND state = 'active'",
                params![seq, entry_error],
            )?;
            if entry_status == "failed" {
                report.failed.push(FailedEntry {
                    seq: Seq(seq),
                    label: tx
                        .query_row("SELECT label FROM outbox WHERE seq = ?1", [seq], |r| {
                            r.get(0)
                        })
                        .optional()?
                        .flatten(),
                    error: entry_error,
                    failed_targets: targets
                        .iter()
                        .map(|t| Id::from(t.target_id.as_str()))
                        .collect(),
                    entry_failed: true,
                });
            }
            for t in targets {
                ended.push((seq, t.target_id));
            }
            if let (kind::MAILBOX_CREATE, Some(temp)) = (kind.as_str(), temp_id) {
                let mut stmt = tx.prepare(
                    "SELECT o.seq FROM outbox o WHERE o.kind = 'email_patch' AND o.status = 'pending'",
                )?;
                let candidates: Vec<i64> = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for candidate in candidates {
                    if all.contains(&candidate) {
                        continue;
                    }
                    let depends = load_targets(&tx, candidate)?.iter().any(|t| {
                        t.patch
                            .as_ref()
                            .is_some_and(|p| references_mailbox(p, &temp))
                    });
                    if depends {
                        all.push(candidate);
                    }
                }
            }
        }
        tx.commit()?;
        self.drop_mirror_rows(&ended)?;
        Ok(report)
    }

    /// Cancels an entry that has not been handed to the flusher yet: a
    /// held send, or any other pending entry. Its overlay disappears.
    pub fn cancel(&mut self, seq: Seq, now: Timestamp) -> Result<CancelOutcome> {
        let row: Option<(String, Option<String>)> = self
            .state
            .query_row(
                "SELECT status, payload_json FROM outbox WHERE seq = ?1",
                [seq.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((status, payload)) = row else {
            return Err(Error::UnknownEntry(seq.0));
        };
        if status != "pending" {
            return Ok(CancelOutcome::TooLate);
        }
        let payload: Option<Payload> = payload.as_deref().map(from_json).transpose()?;
        self.end_entries(&[seq.0], "cancelled", "cancelled", now)?;
        Ok(CancelOutcome::Cancelled {
            payload: payload.and_then(|p| p.returned_payload()),
        })
    }

    /// Ends the hold on a send so the flusher takes it at once. Returns
    /// false if the entry is not pending.
    pub fn send_now(&mut self, seq: Seq) -> Result<bool> {
        let n = self.state.execute(
            "UPDATE outbox SET hold_until = NULL WHERE seq = ?1 AND status = 'pending'",
            [seq.0],
        )?;
        Ok(n > 0)
    }

    /// Undoes the newest action on the undo stack, or returns `None` when
    /// the stack is empty.
    pub fn undo(&mut self, now: Timestamp) -> Result<Option<UndoOutcome>> {
        let top: Option<(i64, i64, String)> = self
            .state
            .query_row(
                "SELECT id, outbox_seq, label FROM undo ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((undo_id, seq, label)) = top else {
            return Ok(None);
        };
        self.state
            .execute("DELETE FROM undo WHERE id = ?1", [undo_id])?;
        let (status, payload): (String, Option<String>) = self.state.query_row(
            "SELECT status, payload_json FROM outbox WHERE seq = ?1",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let payload: Payload = match payload {
            Some(p) => from_json(&p)?,
            None => return Err(Error::Corrupt(format!("outbox entry {seq} has no payload"))),
        };
        match payload.undo_kind() {
            UndoKind::Inverse => {
                let Some(mutation) = self.inverse_mutation(seq, &status, &payload)? else {
                    return Ok(Some(UndoOutcome::TooLate {
                        label,
                        seq: Seq(seq),
                    }));
                };
                let undo_label = format!("Undo {label}");
                let enqueued = self.enqueue_inner(
                    mutation,
                    EnqueueOptions {
                        label: Some(&undo_label),
                        undoable: false,
                    },
                    Some(seq),
                    now,
                )?;
                self.push_redo(enqueued.seq.0, &label, now)?;
                Ok(Some(UndoOutcome::Reverted {
                    label,
                    original: Seq(seq),
                    seq: enqueued.seq,
                }))
            }
            UndoKind::Cancel => match self.cancel(Seq(seq), now)? {
                CancelOutcome::Cancelled { payload } => Ok(Some(UndoOutcome::Cancelled {
                    label,
                    seq: Seq(seq),
                    payload,
                })),
                CancelOutcome::TooLate => Ok(Some(UndoOutcome::TooLate {
                    label,
                    seq: Seq(seq),
                })),
            },
            UndoKind::None => Ok(Some(UndoOutcome::TooLate {
                label,
                seq: Seq(seq),
            })),
        }
    }

    /// Applies the newest undone action again, or returns `None` when there
    /// is nothing to redo.
    ///
    /// Redo mirrors [`Store::undo`]: each undo that queued an inverse can be
    /// redone, newest first, up to [`UNDO_DEPTH`] steps. The redone action
    /// goes back on the undo stack under its original label. Any new action
    /// queued with [`Store::enqueue`] clears the redo stack. Undoing a
    /// create or a send cancels it and hands back its payload, so those are
    /// not redoable.
    pub fn redo(&mut self, now: Timestamp) -> Result<Option<RedoOutcome>> {
        let top: Option<(i64, i64, String)> = self
            .state
            .query_row(
                "SELECT id, outbox_seq, label FROM redo ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((redo_id, seq, label)) = top else {
            return Ok(None);
        };
        self.state
            .execute("DELETE FROM redo WHERE id = ?1", [redo_id])?;
        let (status, payload): (String, Option<String>) = self.state.query_row(
            "SELECT status, payload_json FROM outbox WHERE seq = ?1",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let payload: Payload = match payload {
            Some(p) => from_json(&p)?,
            None => return Err(Error::Corrupt(format!("outbox entry {seq} has no payload"))),
        };
        if payload.undo_kind() != UndoKind::Inverse {
            return Err(Error::Corrupt(format!(
                "redo entry {seq} does not hold an inverse"
            )));
        }
        let Some(mutation) = self.inverse_mutation(seq, &status, &payload)? else {
            return Ok(Some(RedoOutcome::TooLate {
                label,
                seq: Seq(seq),
            }));
        };
        let enqueued = self.enqueue_inner(
            mutation,
            EnqueueOptions {
                label: Some(&label),
                undoable: true,
            },
            Some(seq),
            now,
        )?;
        Ok(Some(RedoOutcome::Reapplied {
            label,
            undone: Seq(seq),
            seq: enqueued.seq,
        }))
    }

    /// The label of the action [`Store::redo`] would apply next.
    pub fn redo_peek(&self) -> Result<Option<String>> {
        crate::read::redo_peek(&self.state)
    }

    /// The objects the action [`Store::undo`] would revert next touches:
    /// email ids for mail actions, the object id otherwise. Lets the caller
    /// see where those emails were before the undo moves them.
    pub fn undo_targets(&self) -> Result<Vec<Id>> {
        self.stack_targets("undo")
    }

    /// The objects the action [`Store::redo`] would apply next touches.
    pub fn redo_targets(&self) -> Result<Vec<Id>> {
        self.stack_targets("redo")
    }

    fn stack_targets(&self, stack: &str) -> Result<Vec<Id>> {
        // `stack` is one of two fixed table names, never user input.
        let sql = format!(
            "SELECT t.target_id FROM outbox_target t
             WHERE t.seq = (SELECT outbox_seq FROM {stack} ORDER BY id DESC LIMIT 1)
             ORDER BY t.target_id"
        );
        let mut stmt = self.state.prepare(&sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows
            .map(|r| r.map(Id::from))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The mutation that reverts entry `seq`: every target's stored
    /// inverse, with the original patch as the new inverse. `None` when the
    /// entry failed, was cancelled, or has nothing left to revert.
    fn inverse_mutation(
        &self,
        seq: i64,
        status: &str,
        payload: &Payload,
    ) -> Result<Option<Mutation>> {
        if status == "failed" || status == "cancelled" {
            return Ok(None);
        }
        let mut stmt = self.state.prepare(
            "SELECT target_id, patch_json, inverse_json FROM outbox_target
             WHERE seq = ?1 AND state != 'rejected' AND inverse_json IS NOT NULL",
        )?;
        let rows: Vec<(String, Option<String>, String)> = stmt
            .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let mut targets = Vec::new();
        for (target, patch, inverse) in rows {
            let inverse: Patch = from_json(&inverse)?;
            if inverse.is_empty() {
                continue;
            }
            let patch: Option<Patch> = patch.as_deref().map(from_json).transpose()?;
            targets.push((target, inverse, patch));
        }
        if targets.is_empty() {
            return Ok(None);
        }
        Ok(Some(match payload {
            Payload::Patch => Mutation::EmailPatch(
                targets
                    .into_iter()
                    .map(|(id, inverse, patch)| EmailPatchTarget {
                        email_id: Id::from(id),
                        patch: inverse,
                        inverse: patch,
                    })
                    .collect(),
            ),
            Payload::MaskedUpdate => {
                let (id, inverse, patch) = targets.remove(0);
                Mutation::MaskedEmailUpdate {
                    id: Id::from(id),
                    patch: inverse,
                    inverse: patch,
                }
            }
            _ => {
                let (id, inverse, patch) = targets.remove(0);
                Mutation::ContactUpdate {
                    id: Id::from(id),
                    patch: inverse,
                    inverse: patch,
                }
            }
        }))
    }

    fn push_redo(&mut self, seq: i64, label: &str, now: Timestamp) -> Result<()> {
        let tx = self.state.transaction()?;
        tx.execute(
            "INSERT INTO redo (outbox_seq, label, created_at) VALUES (?1, ?2, ?3)",
            params![seq, label, ts_ms(now)],
        )?;
        tx.execute(
            "DELETE FROM redo WHERE id NOT IN (SELECT id FROM redo ORDER BY id DESC LIMIT ?1)",
            [crate::codec::usize_i64(UNDO_DEPTH)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// When [`Store::next_flush`] will next have work that is waiting for
    /// its time: the earliest end of a send hold or a retry backoff among
    /// pending entries. `None` when nothing is pending. An entry with no
    /// hold and no backoff is due at once and gives the Unix epoch, which is
    /// before any `now`.
    pub fn next_flush_time(&self) -> Result<Option<Timestamp>> {
        let at: Option<i64> = self.state.query_row(
            "SELECT MIN(MAX(COALESCE(hold_until, 0), COALESCE(not_before, 0)))
             FROM outbox WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(at.map(crate::codec::ms_ts))
    }

    /// Deletes finished entries older than `before` that the undo and redo
    /// stacks no longer refer to. Returns how many were deleted.
    pub fn prune_outbox(&mut self, before: Timestamp) -> Result<usize> {
        Ok(self.state.execute(
            "DELETE FROM outbox WHERE status IN ('done', 'failed', 'cancelled')
             AND overlay_active = 0 AND finished_at < ?1
             AND seq NOT IN (SELECT outbox_seq FROM undo)
             AND seq NOT IN (SELECT outbox_seq FROM redo)",
            [ts_ms(before)],
        )?)
    }

    /// The label of the action [`Store::undo`] would undo next.
    pub fn undo_peek(&self) -> Result<Option<String>> {
        crate::read::undo_peek(&self.state)
    }
}

fn temp_target() -> TargetRow {
    TargetRow {
        target_id: String::new(),
        patch: None,
        inverse: None,
    }
}

fn check_update(id: &Id, patch: &Patch) -> Result<()> {
    if id.is_empty() {
        return Err(Error::Invalid("an update with an empty id".into()));
    }
    if patch.is_empty() {
        return Err(Error::Invalid("the update changes nothing".into()));
    }
    Ok(())
}
