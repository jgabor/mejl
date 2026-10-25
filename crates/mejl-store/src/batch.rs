//! Applying server data. One [`SyncBatch`] is one cache transaction.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::Timestamp;
use mejl_jmap::method::{ChangesResponse, GetResponse, QueryChangesResponse, QueryResponse};
use mejl_jmap::types::{
    ContactCard, Email, EmailAddress, EmailBodyPart, Identity, Mailbox, MaskedEmail, Thread,
};
use mejl_jmap::{Id, Session, State};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::codec::{
    addresses_text, clean_text, decode_ids, encode_ids, from_json, html_to_index_text, json,
    name_words, normalise_address, opt_json, to_i64, ts_ms, usize_i64,
};
use crate::derive::{self, Dirty, kind};
use crate::error::{Error, Result};
use crate::overlay::EmailFlags;
use crate::store::{Store, StoreOptions};

/// The header Fastmail puts on mail delivered to a masked address. mejl-fake
/// uses this name.
pub const MASKED_EMAIL_HEADER: &str = "X-FM-Masked-Email";
/// Another name for the masked header seen in Fastmail documentation. Which
/// one stored mail carries is unverified, so both are read.
pub const MASKED_EMAIL_HEADER_ALT: &str = "Fastmail-MaskedEmail";

/// Type names for [`SyncBatch::set_type_state`].
pub mod type_name {
    /// `Mailbox` state.
    pub const MAILBOX: &str = "Mailbox";
    /// `Email` state.
    pub const EMAIL: &str = "Email";
    /// `Thread` state.
    pub const THREAD: &str = "Thread";
    /// `Identity` state.
    pub const IDENTITY: &str = "Identity";
    /// `ContactCard` state.
    pub const CONTACT_CARD: &str = "ContactCard";
    /// `MaskedEmail` state. Fastmail reportedly sends an empty string.
    pub const MASKED_EMAIL: &str = "MaskedEmail";
}

/// Most body text indexed for search per message. Long bodies are mostly
/// quoted history and would bloat the index.
const MAX_INDEXED_BODY: usize = 64 * 1024;

/// Row ids per second of received time. See `allocate_rid`.
const RID_SPREAD: i64 = 1024;

/// What an email upsert did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpsertReport {
    /// Emails that were not cached before.
    pub inserted: usize,
    /// Cached emails whose fetched properties were merged in.
    pub updated: usize,
    /// Ids that are not cached and came without `receivedAt`, typically from
    /// a flags-only `Email/get` of `#updated`. Fetch them with the list
    /// properties.
    pub needs_fetch: Vec<Id>,
}

/// What a full masked email refresh changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaskedDiff {
    /// Masks the cache did not have.
    pub created: Vec<Id>,
    /// Masks whose stored object changed.
    pub updated: Vec<Id>,
    /// Masks the server no longer lists.
    pub destroyed: Vec<Id>,
}

/// The result of applying `Email/queryChanges` to a cached query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryChangesOutcome {
    /// The cached ids, state and total were updated.
    Applied,
    /// No query is cached under this key.
    NotCached,
    /// The response's `oldQueryState` is not the cached state. Refetch the
    /// query.
    StateMismatch,
}

/// One sync batch: a single cache transaction. Dropping it without
/// [`commit`](SyncBatch::commit) rolls everything back.
///
/// Before commit, the batch re-applies every pending overlay to the emails
/// it touched, rebuilds the conversation rows of their threads, confirms
/// done outbox entries that this server data now covers, and trims the body
/// cache.
pub struct SyncBatch<'a> {
    tx: Transaction<'a>,
    state: &'a mut Connection,
    options: &'a StoreOptions,
    dirty: Dirty,
    touched_emails: BTreeSet<String>,
    applied_mailboxes: bool,
    applied_masked: bool,
    applied_contacts: bool,
    fresh_bodies: HashSet<i64>,
    sent_mailboxes: Option<HashSet<String>>,
}

impl<'a> SyncBatch<'a> {
    pub(crate) fn begin(store: &'a mut Store) -> Result<Self> {
        let tx = store.cache.transaction()?;
        Ok(Self {
            tx,
            state: &mut store.state,
            options: &store.options,
            dirty: Dirty::default(),
            touched_emails: BTreeSet::new(),
            applied_mailboxes: false,
            applied_masked: false,
            applied_contacts: false,
            fresh_bodies: HashSet::new(),
            sent_mailboxes: None,
        })
    }

    fn conn(&self) -> &Connection {
        &self.tx
    }

    // Session and states.

    /// Stores the session object and its state string.
    pub fn save_session(&mut self, session: &Session) -> Result<()> {
        self.set_meta("session_json", &json(session)?)?;
        self.set_meta("session_state", session.state.as_str())?;
        Ok(())
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn()
            .prepare_cached(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?
            .execute(params![key, value])?;
        Ok(())
    }

    /// Stores the state string for a JMAP type. See [`type_name`].
    pub fn set_type_state(&mut self, type_name: &str, state: &State) -> Result<()> {
        self.conn()
            .prepare_cached(
                "INSERT INTO sync_state (type, state) VALUES (?1, ?2)
                 ON CONFLICT (type) DO UPDATE SET state = excluded.state",
            )?
            .execute(params![type_name, state.as_str()])?;
        Ok(())
    }

    /// The state string stored for a JMAP type, including changes made
    /// earlier in this batch.
    pub fn type_state(&self, type_name: &str) -> Result<Option<State>> {
        crate::read::type_state(self.conn(), type_name)
    }

    /// Forgets a type's state, for example after `cannotCalculateChanges`.
    pub fn clear_type_state(&mut self, type_name: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM sync_state WHERE type = ?1", [type_name])?;
        Ok(())
    }

    // Mailboxes.

    /// Replaces every mailbox with the result of `Mailbox/get` for all ids,
    /// and stores its state.
    pub fn replace_mailboxes(&mut self, get: &GetResponse<Mailbox>) -> Result<()> {
        let keep: HashSet<&str> = get.list.iter().map(|m| m.id.as_str()).collect();
        let existing: Vec<String> = self
            .conn()
            .prepare("SELECT id FROM mailbox")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in existing.iter().filter(|id| !keep.contains(id.as_str())) {
            self.conn()
                .execute("DELETE FROM mailbox WHERE id = ?1", [id])?;
        }
        for mailbox in &get.list {
            self.upsert_mailbox(mailbox)?;
        }
        self.set_type_state(type_name::MAILBOX, &get.state)?;
        self.applied_mailboxes = true;
        Ok(())
    }

    /// Applies `Mailbox/changes` with the `Mailbox/get` of its created and
    /// updated ids, and stores the new state. Fetched mailboxes may be
    /// partial (for example only counts, after `updatedProperties`); they
    /// are merged into what is stored.
    pub fn apply_mailbox_changes(
        &mut self,
        changes: &ChangesResponse<Mailbox>,
        fetched: &[Mailbox],
    ) -> Result<()> {
        for id in &changes.destroyed {
            self.conn()
                .execute("DELETE FROM mailbox WHERE id = ?1", [id.as_str()])?;
        }
        for mailbox in fetched {
            self.upsert_mailbox(mailbox)?;
        }
        self.set_type_state(type_name::MAILBOX, &changes.new_state)?;
        self.applied_mailboxes = true;
        Ok(())
    }

    /// Inserts or merges mailboxes without touching the stored state.
    pub fn upsert_mailboxes(&mut self, mailboxes: &[Mailbox]) -> Result<()> {
        for mailbox in mailboxes {
            self.upsert_mailbox(mailbox)?;
        }
        self.applied_mailboxes = true;
        Ok(())
    }

    fn upsert_mailbox(&mut self, mailbox: &Mailbox) -> Result<()> {
        if mailbox.id.is_empty() {
            return Err(Error::Invalid("a mailbox without an id".into()));
        }
        let stored: Option<String> = self
            .conn()
            .prepare_cached("SELECT json FROM mailbox WHERE id = ?1")?
            .query_row([mailbox.id.as_str()], |r| r.get(0))
            .optional()?;
        let merged: Mailbox = match stored {
            Some(stored) => {
                let mut base: Value = from_json(&stored)?;
                let update = serde_json::to_value(mailbox)?;
                if let (Some(base), Value::Object(update)) = (base.as_object_mut(), update) {
                    base.extend(update);
                }
                serde_json::from_value(base)?
            }
            None => mailbox.clone(),
        };
        self.conn()
            .prepare_cached(
                "INSERT INTO mailbox (id, parent_id, name, role, sort_order, total_emails,
                     unread_emails, total_threads, unread_threads, is_subscribed, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (id) DO UPDATE SET parent_id = excluded.parent_id,
                     name = excluded.name, role = excluded.role, sort_order = excluded.sort_order,
                     total_emails = excluded.total_emails, unread_emails = excluded.unread_emails,
                     total_threads = excluded.total_threads, unread_threads = excluded.unread_threads,
                     is_subscribed = excluded.is_subscribed, json = excluded.json",
            )?
            .execute(params![
                merged.id.as_str(),
                merged.parent_id.as_ref().map(Id::as_str),
                merged.name.as_deref().unwrap_or(""),
                merged.role.as_ref().map(|r| r.as_str()),
                merged.sort_order.unwrap_or(0),
                to_i64(merged.total_emails.unwrap_or(0)),
                to_i64(merged.unread_emails.unwrap_or(0)),
                to_i64(merged.total_threads.unwrap_or(0)),
                to_i64(merged.unread_threads.unwrap_or(0)),
                merged.is_subscribed.unwrap_or(false),
                json(&merged)?,
            ])?;
        self.sent_mailboxes = None;
        Ok(())
    }

    // Emails.

    /// Applies `Email/changes` with the `Email/get` of its created and
    /// updated ids, and stores the new state. Destroyed ids are removed
    /// with their bodies, index rows and membership.
    pub fn apply_email_changes(
        &mut self,
        changes: &ChangesResponse<Email>,
        fetched: &[Email],
    ) -> Result<UpsertReport> {
        self.destroy_emails(&changes.destroyed)?;
        let mut report = self.upsert_emails(fetched)?;
        let got: HashSet<&str> = fetched.iter().map(|e| e.id.as_str()).collect();
        for id in changes.created.iter().chain(&changes.updated) {
            if got.contains(id.as_str()) || report.needs_fetch.contains(id) {
                continue;
            }
            if derive::load_core(self.conn(), id.as_str())?.is_none() {
                report.needs_fetch.push(id.clone());
            }
        }
        self.set_type_state(type_name::EMAIL, &changes.new_state)?;
        Ok(report)
    }

    /// Inserts or merges emails from any `Email/get`. Properties that were
    /// not fetched keep their stored values, so a flags-only fetch of
    /// `mailboxIds` and `keywords` is safe. An email that is not cached is
    /// only inserted when it has `receivedAt`; otherwise its id is returned
    /// in [`UpsertReport::needs_fetch`].
    pub fn upsert_emails(&mut self, emails: &[Email]) -> Result<UpsertReport> {
        let mut report = UpsertReport::default();
        for email in emails {
            self.upsert_email(email, &mut report)?;
        }
        Ok(report)
    }

    fn sent_mailboxes(&mut self) -> Result<&HashSet<String>> {
        if self.sent_mailboxes.is_none() {
            let ids: HashSet<String> = self
                .conn()
                .prepare_cached("SELECT id FROM mailbox WHERE role = 'sent'")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            self.sent_mailboxes = Some(ids);
        }
        Ok(self.sent_mailboxes.get_or_insert_with(HashSet::new))
    }

    fn upsert_email(&mut self, e: &Email, report: &mut UpsertReport) -> Result<()> {
        if e.id.is_empty() {
            return Err(Error::Invalid("an email without an id".into()));
        }
        let id = e.id.as_str();
        let keywords = e
            .keywords
            .as_ref()
            .map(|k| derive::set_json(&EmailFlags::from_maps(Some(k), None).keywords))
            .transpose()?;
        let mailboxes = e
            .mailbox_ids
            .as_ref()
            .map(|m| {
                let set: BTreeSet<String> = m
                    .iter()
                    .filter(|(_, on)| **on)
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect();
                derive::set_json(&set)
            })
            .transpose()?;
        let masked = masked_header_of(e);
        let received = e.received_at.map(ts_ms);
        let existing = derive::load_core(self.conn(), id)?;

        match &existing {
            None => {
                let Some(received) = received else {
                    report.needs_fetch.push(e.id.clone());
                    return Ok(());
                };
                let thread = e.thread_id.as_ref().unwrap_or(&e.id).as_str();
                let rid = self.allocate_rid(received)?;
                self.conn()
                    .prepare_cached(
                        "INSERT INTO email (rid, id, thread_id, received_at, size, subject, preview,
                             from_json, to_json, cc_json, bcc_json, reply_to_json, sender_json,
                             sent_at, message_id_json, in_reply_to_json, references_json,
                             has_attachment, srv_keywords_json, srv_mailbox_ids_json,
                             keywords_json, masked_header)
                         VALUES (?21, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                             ?14, ?15, ?16, ?17, ?18, ?19, ?18, ?20)",
                    )?
                    .execute(params![
                        id,
                        thread,
                        received,
                        to_i64(e.size.unwrap_or(0)),
                        e.subject.as_deref().unwrap_or(""),
                        e.preview.as_deref().unwrap_or(""),
                        opt_json(e.from.as_ref())?,
                        opt_json(e.to.as_ref())?,
                        opt_json(e.cc.as_ref())?,
                        opt_json(e.bcc.as_ref())?,
                        opt_json(e.reply_to.as_ref())?,
                        opt_json(e.sender.as_ref())?,
                        e.sent_at,
                        opt_json(e.message_id.as_ref())?,
                        opt_json(e.in_reply_to.as_ref())?,
                        opt_json(e.references.as_ref())?,
                        e.has_attachment.unwrap_or(false),
                        keywords.as_deref().unwrap_or("{}"),
                        mailboxes.as_deref().unwrap_or("{}"),
                        masked.clone().flatten(),
                        rid,
                    ])?;
                self.conn()
                    .prepare_cached(
                        "INSERT INTO search_doc (rowid, subject, from_text, to_text, preview)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    )?
                    .execute(params![
                        rid,
                        clean_text(e.subject.as_deref().unwrap_or("")),
                        from_text(e),
                        to_text(e),
                        clean_text(e.preview.as_deref().unwrap_or("")),
                    ])?;
                if masked.as_ref().is_some_and(Option::is_some) {
                    self.resolve_masked(rid)?;
                }
                self.note_correspondents(e, received)?;
                report.inserted += 1;
            }
            Some(core) => {
                let masked_fetched = masked.is_some();
                self.conn()
                    .prepare_cached(
                        "UPDATE email SET
                             thread_id = COALESCE(?2, thread_id),
                             received_at = COALESCE(?3, received_at),
                             size = COALESCE(?4, size),
                             subject = COALESCE(?5, subject),
                             preview = COALESCE(?6, preview),
                             from_json = COALESCE(?7, from_json),
                             to_json = COALESCE(?8, to_json),
                             cc_json = COALESCE(?9, cc_json),
                             bcc_json = COALESCE(?10, bcc_json),
                             reply_to_json = COALESCE(?11, reply_to_json),
                             sender_json = COALESCE(?12, sender_json),
                             sent_at = COALESCE(?13, sent_at),
                             message_id_json = COALESCE(?14, message_id_json),
                             in_reply_to_json = COALESCE(?15, in_reply_to_json),
                             references_json = COALESCE(?16, references_json),
                             has_attachment = COALESCE(?17, has_attachment),
                             srv_keywords_json = COALESCE(?18, srv_keywords_json),
                             srv_mailbox_ids_json = COALESCE(?19, srv_mailbox_ids_json),
                             masked_header = CASE WHEN ?20 THEN ?21 ELSE masked_header END
                         WHERE rid = ?1",
                    )?
                    .execute(params![
                        core.rid,
                        e.thread_id.as_ref().map(Id::as_str),
                        received,
                        e.size.map(to_i64),
                        e.subject,
                        e.preview,
                        opt_json(e.from.as_ref())?,
                        opt_json(e.to.as_ref())?,
                        opt_json(e.cc.as_ref())?,
                        opt_json(e.bcc.as_ref())?,
                        opt_json(e.reply_to.as_ref())?,
                        opt_json(e.sender.as_ref())?,
                        e.sent_at,
                        opt_json(e.message_id.as_ref())?,
                        opt_json(e.in_reply_to.as_ref())?,
                        opt_json(e.references.as_ref())?,
                        e.has_attachment,
                        keywords,
                        mailboxes,
                        masked_fetched,
                        masked.clone().flatten(),
                    ])?;
                if e.subject.is_some()
                    || e.preview.is_some()
                    || e.from.is_some()
                    || e.to.is_some()
                    || e.cc.is_some()
                    || e.bcc.is_some()
                {
                    let has_recipients = e.to.is_some() || e.cc.is_some() || e.bcc.is_some();
                    self.conn()
                        .prepare_cached(
                            "UPDATE search_doc SET subject = COALESCE(?2, subject),
                                 from_text = COALESCE(?3, from_text),
                                 to_text = COALESCE(?4, to_text),
                                 preview = COALESCE(?5, preview)
                             WHERE rowid = ?1",
                        )?
                        .execute(params![
                            core.rid,
                            e.subject.as_deref().map(clean_text),
                            e.from.as_ref().map(|_| from_text(e)),
                            has_recipients.then(|| to_text(e)),
                            e.preview.as_deref().map(clean_text),
                        ])?;
                }
                if masked_fetched {
                    self.resolve_masked(core.rid)?;
                }
                if let Some(thread) = &e.thread_id
                    && thread.as_str() != core.thread_id
                {
                    self.dirty.threads.insert(core.thread_id.clone());
                }
                report.updated += 1;
            }
        }
        derive::refresh_email(&self.tx, id, &mut self.dirty)?;
        self.touched_emails.insert(id.to_owned());
        Ok(())
    }

    /// Picks the row id for a new email from its received time, so rowid
    /// order is date order. The full-text index keeps documents in rowid
    /// order, which lets search walk the newest matches first without
    /// touching the email table. Ties within a second take the next free
    /// id.
    fn allocate_rid(&self, received_ms: i64) -> Result<i64> {
        // Far beyond any real date, and small enough that the product fits.
        const LIMIT: i64 = 1 << 52;
        // Seconds, not milliseconds: JMAP dates have second precision, and
        // smaller gaps between neighbouring ids keep the index compact.
        let mut rid = received_ms.div_euclid(1000).clamp(-LIMIT, LIMIT) * RID_SPREAD;
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM email WHERE rid = ?1)")?;
        while stmt.query_row([rid], |r| r.get::<_, bool>(0))? {
            rid += 1;
        }
        Ok(rid)
    }

    fn resolve_masked(&self, rid: i64) -> Result<()> {
        self.conn()
            .prepare_cached(
                "UPDATE email SET masked_email_id = (
                     SELECT m.id FROM masked_email m
                     WHERE m.email_lc = email.masked_header OR m.id = email.masked_header LIMIT 1)
                 WHERE rid = ?1",
            )?
            .execute([rid])?;
        Ok(())
    }

    fn note_correspondents(&mut self, e: &Email, received: i64) -> Result<()> {
        let Some(mailboxes) = &e.mailbox_ids else {
            return Ok(());
        };
        let sent = self.sent_mailboxes()?;
        if !mailboxes
            .iter()
            .any(|(id, on)| *on && sent.contains(id.as_str()))
        {
            return Ok(());
        }
        let recipients: Vec<&EmailAddress> = [&e.to, &e.cc, &e.bcc]
            .into_iter()
            .flatten()
            .flatten()
            .filter(|a| !a.email.is_empty())
            .collect();
        for addr in recipients {
            let name = addr
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty());
            self.conn()
                .prepare_cached(
                    "INSERT INTO correspondent (address_lc, address, name, name_words, last_used_at, use_count)
                     VALUES (?1, ?2, ?3, ?4, ?5, 1)
                     ON CONFLICT (address_lc) DO UPDATE SET
                         use_count = use_count + 1,
                         last_used_at = max(last_used_at, excluded.last_used_at),
                         name = COALESCE(excluded.name, name),
                         name_words = CASE WHEN excluded.name IS NULL THEN name_words
                                           ELSE excluded.name_words END",
                )?
                .execute(params![
                    addr.email.to_lowercase(),
                    addr.email,
                    name,
                    name.map(name_words).unwrap_or_default(),
                    received,
                ])?;
        }
        Ok(())
    }

    /// Removes emails the server destroyed, with their bodies, index rows
    /// and membership. Returns how many were cached.
    pub fn destroy_emails(&mut self, ids: &[Id]) -> Result<usize> {
        let mut n = 0;
        for id in ids {
            let Some(core) = derive::load_core(self.conn(), id.as_str())? else {
                self.touched_emails.insert(id.as_str().to_owned());
                continue;
            };
            derive::set_membership(self.conn(), core.rid, core.received_at, &BTreeSet::new())?;
            self.conn()
                .prepare_cached("DELETE FROM search_doc WHERE rowid = ?1")?
                .execute([core.rid])?;
            self.conn()
                .prepare_cached("DELETE FROM email WHERE rid = ?1")?
                .execute([core.rid])?;
            self.dirty.threads.insert(core.thread_id);
            self.touched_emails.insert(id.as_str().to_owned());
            n += 1;
        }
        Ok(n)
    }

    // Threads.

    /// Applies `Thread/changes` with the `Thread/get` of changed ids, and
    /// stores the new state.
    pub fn apply_thread_changes(
        &mut self,
        changes: &ChangesResponse<Thread>,
        fetched: &[Thread],
    ) -> Result<()> {
        for id in &changes.destroyed {
            self.conn()
                .prepare_cached("DELETE FROM thread WHERE id = ?1")?
                .execute([id.as_str()])?;
        }
        self.upsert_threads(fetched)?;
        self.set_type_state(type_name::THREAD, &changes.new_state)
    }

    /// Stores thread email lists from `Thread/get`.
    pub fn upsert_threads(&mut self, threads: &[Thread]) -> Result<()> {
        for thread in threads {
            self.conn()
                .prepare_cached(
                    "INSERT INTO thread (id, email_ids_json) VALUES (?1, ?2)
                     ON CONFLICT (id) DO UPDATE SET email_ids_json = excluded.email_ids_json",
                )?
                .execute(params![thread.id.as_str(), json(&thread.email_ids)?])?;
        }
        Ok(())
    }

    // Identities.

    /// Replaces every identity with the result of `Identity/get` for all
    /// ids, and stores its state.
    pub fn replace_identities(&mut self, get: &GetResponse<Identity>) -> Result<()> {
        self.conn().execute("DELETE FROM identity", [])?;
        for (pos, identity) in get.list.iter().enumerate() {
            self.write_identity(identity, usize_i64(pos))?;
        }
        self.set_type_state(type_name::IDENTITY, &get.state)
    }

    /// Applies `Identity/changes` with the `Identity/get` of changed ids.
    pub fn apply_identity_changes(
        &mut self,
        changes: &ChangesResponse<Identity>,
        fetched: &[Identity],
    ) -> Result<()> {
        for id in &changes.destroyed {
            self.conn()
                .execute("DELETE FROM identity WHERE id = ?1", [id.as_str()])?;
        }
        for identity in fetched {
            let pos: Option<i64> = self
                .conn()
                .query_row(
                    "SELECT pos FROM identity WHERE id = ?1",
                    [identity.id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let pos = match pos {
                Some(pos) => pos,
                None => self.conn().query_row(
                    "SELECT COALESCE(MAX(pos) + 1, 0) FROM identity",
                    [],
                    |r| r.get(0),
                )?,
            };
            self.write_identity(identity, pos)?;
        }
        self.set_type_state(type_name::IDENTITY, &changes.new_state)
    }

    fn write_identity(&self, identity: &Identity, pos: i64) -> Result<()> {
        if identity.id.is_empty() {
            return Err(Error::Invalid("an identity without an id".into()));
        }
        self.conn().execute(
            "INSERT INTO identity (id, pos, json) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET pos = excluded.pos, json = excluded.json",
            params![identity.id.as_str(), pos, json(identity)?],
        )?;
        Ok(())
    }

    // Contacts.

    /// Replaces every contact card with a full `ContactCard/get`, and
    /// stores its state.
    pub fn replace_contacts(&mut self, get: &GetResponse<ContactCard>) -> Result<()> {
        self.conn().execute("DELETE FROM contact_address", [])?;
        self.conn().execute("DELETE FROM contact", [])?;
        for card in &get.list {
            self.write_contact(card)?;
        }
        self.applied_contacts = true;
        self.set_type_state(type_name::CONTACT_CARD, &get.state)
    }

    /// Applies `ContactCard/changes` with the `ContactCard/get` of changed
    /// ids, and stores the new state.
    pub fn apply_contact_changes(
        &mut self,
        changes: &ChangesResponse<ContactCard>,
        fetched: &[ContactCard],
    ) -> Result<()> {
        for id in &changes.destroyed {
            self.conn()
                .execute("DELETE FROM contact WHERE id = ?1", [id.as_str()])?;
        }
        for card in fetched {
            self.write_contact(card)?;
        }
        self.applied_contacts = true;
        self.set_type_state(type_name::CONTACT_CARD, &changes.new_state)
    }

    fn write_contact(&self, card: &ContactCard) -> Result<()> {
        if card.id.is_empty() {
            return Err(Error::Invalid("a contact without an id".into()));
        }
        let name = card.display_name();
        self.conn().execute(
            "INSERT INTO contact (id, uid, kind, name, name_words, json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET uid = excluded.uid, kind = excluded.kind,
                 name = excluded.name, name_words = excluded.name_words, json = excluded.json",
            params![
                card.id.as_str(),
                card.uid,
                card.kind,
                name,
                name.as_deref().map(name_words).unwrap_or_default(),
                json(card)?,
            ],
        )?;
        self.conn().execute(
            "DELETE FROM contact_address WHERE contact_id = ?1",
            [card.id.as_str()],
        )?;
        for (pref, address) in card.email_addresses().into_iter().enumerate() {
            self.conn().execute(
                "INSERT OR IGNORE INTO contact_address (contact_id, address_lc, address, pref)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    card.id.as_str(),
                    address.to_lowercase(),
                    address,
                    usize_i64(pref)
                ],
            )?;
        }
        Ok(())
    }

    // Masked email.

    /// Replaces every masked address with a full `MaskedEmail/get` and
    /// reports what changed. Fastmail reportedly cannot calculate changes
    /// for this type, so a full refresh with a local diff is the sync
    /// strategy. Also re-links cached messages to their masks by address.
    pub fn replace_masked_emails(&mut self, get: &GetResponse<MaskedEmail>) -> Result<MaskedDiff> {
        let mut stored: BTreeMap<String, String> = BTreeMap::new();
        {
            let mut stmt = self.conn().prepare("SELECT id, json FROM masked_email")?;
            for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
                let (id, json): (String, String) = row?;
                stored.insert(id, json);
            }
        }
        let mut diff = MaskedDiff::default();
        let mut seen: HashSet<&str> = HashSet::new();
        for masked in &get.list {
            if masked.id.is_empty() {
                return Err(Error::Invalid("a masked email without an id".into()));
            }
            seen.insert(masked.id.as_str());
            let new_json = json(masked)?;
            match stored.get(masked.id.as_str()) {
                None => diff.created.push(masked.id.clone()),
                Some(old) if *old != new_json => diff.updated.push(masked.id.clone()),
                Some(_) => continue,
            }
            self.conn().execute(
                "INSERT INTO masked_email (id, email_lc, state, last_message_at, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET email_lc = excluded.email_lc,
                     state = excluded.state, last_message_at = excluded.last_message_at,
                     json = excluded.json",
                params![
                    masked.id.as_str(),
                    masked.email.as_deref().map(normalise_address),
                    masked.state.as_ref().map(|s| s.as_str()),
                    masked.last_message_at.map(ts_ms),
                    new_json,
                ],
            )?;
        }
        for id in stored.keys().filter(|id| !seen.contains(id.as_str())) {
            self.conn()
                .execute("DELETE FROM masked_email WHERE id = ?1", [id])?;
            diff.destroyed.push(Id::from(id.as_str()));
        }
        self.conn().execute(
            "UPDATE email SET masked_email_id = (
                 SELECT m.id FROM masked_email m
                 WHERE m.email_lc = email.masked_header OR m.id = email.masked_header LIMIT 1)
             WHERE masked_header IS NOT NULL",
            [],
        )?;
        self.set_type_state(type_name::MASKED_EMAIL, &get.state)?;
        self.applied_masked = true;
        Ok(diff)
    }

    // Bodies.

    /// Stores the body of a cached email from an `Email/get` with
    /// `bodyValues`, `textBody`, `htmlBody`, `attachments` and optionally
    /// `headers`, and adds its text to the search index. Returns false
    /// when the email is not cached, in which case nothing is stored.
    pub fn upsert_body(&mut self, email: &Email, fetched_at: Timestamp) -> Result<bool> {
        let Some(core) = derive::load_core(self.conn(), email.id.as_str())? else {
            return Ok(false);
        };
        let rid = core.rid;
        let empty = BTreeMap::new();
        let values = email.body_values.as_ref().unwrap_or(&empty);
        let mut bytes: usize = 0;

        self.conn()
            .prepare_cached("DELETE FROM body_meta WHERE email_rid = ?1")?
            .execute([rid])?;
        let headers_json = opt_json(email.headers.as_ref())?;
        bytes += headers_json.as_ref().map_or(0, String::len);
        self.conn()
            .prepare_cached(
                "INSERT INTO body_meta (email_rid, headers_json, fetched_at, opened_at, bytes)
                 VALUES (?1, ?2, ?3, NULL, 0)",
            )?
            .execute(params![rid, headers_json, ts_ms(fetched_at)])?;

        let mut index_text = String::new();
        for (kind, parts) in [(0_i64, &email.text_body), (1_i64, &email.html_body)] {
            for (idx, part) in parts.iter().flatten().enumerate() {
                let value = part.part_id.as_ref().and_then(|p| values.get(p));
                let text = value.map_or("", |v| v.value.as_str());
                bytes += text.len();
                self.conn()
                    .prepare_cached(
                        "INSERT INTO body_part (email_rid, kind, idx, part_id, blob_id, mime_type,
                             charset, value, is_truncated, is_encoding_problem)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    )?
                    .execute(params![
                        rid,
                        kind,
                        usize_i64(idx),
                        part.part_id,
                        part.blob_id.as_ref().map(Id::as_str),
                        part.content_type,
                        part.charset,
                        text,
                        value.is_some_and(|v| v.is_truncated),
                        value.is_some_and(|v| v.is_encoding_problem),
                    ])?;
                if kind == 0 && !text.is_empty() {
                    index_text.push_str(text);
                    index_text.push('\n');
                }
            }
        }
        if index_text.trim().is_empty() {
            for part in email.html_body.iter().flatten() {
                if let Some(v) = part.part_id.as_ref().and_then(|p| values.get(p)) {
                    index_text.push_str(&html_to_index_text(&v.value));
                    index_text.push('\n');
                }
            }
        }
        for (idx, part) in email.attachments.iter().flatten().enumerate() {
            self.write_attachment(rid, idx, part)?;
        }
        self.conn()
            .prepare_cached("UPDATE body_meta SET bytes = ?2 WHERE email_rid = ?1")?
            .execute(params![rid, usize_i64(bytes)])?;

        let mut index_text = clean_text(&index_text).into_owned();
        if index_text.len() > MAX_INDEXED_BODY {
            let mut cut = MAX_INDEXED_BODY;
            while !index_text.is_char_boundary(cut) {
                cut -= 1;
            }
            index_text.truncate(cut);
        }
        self.conn()
            .prepare_cached("UPDATE search_doc SET body_text = ?2 WHERE rowid = ?1")?
            .execute(params![rid, index_text])?;
        self.fresh_bodies.insert(rid);
        Ok(true)
    }

    fn write_attachment(&self, rid: i64, idx: usize, part: &EmailBodyPart) -> Result<()> {
        self.conn()
            .prepare_cached(
                "INSERT INTO attachment (email_rid, idx, part_id, blob_id, name, mime_type, size,
                     disposition, cid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute(params![
                rid,
                usize_i64(idx),
                part.part_id,
                part.blob_id.as_ref().map(Id::as_str),
                part.name,
                part.content_type,
                to_i64(part.size.unwrap_or(0)),
                part.disposition,
                part.cid,
            ])?;
        Ok(())
    }

    // Query cache.

    /// Stores a query result window. A response at position 0 replaces the
    /// cached ids; a later position appends to or overwrites the cached
    /// window from that position. A position past the end of the window is
    /// an error, because the cache only holds contiguous windows.
    pub fn save_query(
        &mut self,
        key: &str,
        response: &QueryResponse,
        collapse_threads: bool,
        now: Timestamp,
    ) -> Result<()> {
        let position = usize::try_from(response.position).unwrap_or(usize::MAX);
        let mut ids = if position == 0 {
            Vec::new()
        } else {
            let cached: Option<(Vec<u8>, bool)> = self
                .conn()
                .query_row(
                    "SELECT ids_blob, collapse_threads FROM query_cache WHERE query_key = ?1",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match cached {
                Some((blob, collapse)) if collapse == collapse_threads => decode_ids(&blob)?,
                _ => {
                    return Err(Error::Invalid(format!(
                        "query {key} has no cached window to extend at position {position}"
                    )));
                }
            }
        };
        if position > ids.len() {
            return Err(Error::Invalid(format!(
                "query {key} window ends at {}, cannot add at {position}",
                ids.len()
            )));
        }
        ids.truncate(position);
        ids.extend(response.ids.iter().cloned());
        self.write_query(
            key,
            &ids,
            &response.query_state,
            collapse_threads,
            response.can_calculate_changes,
            response.total,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn write_query(
        &self,
        key: &str,
        ids: &[Id],
        state: &State,
        collapse: bool,
        can_calculate: bool,
        total: Option<u64>,
        now: Timestamp,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO query_cache (query_key, query_state, collapse_threads, can_calculate_changes,
                 total, ids_blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (query_key) DO UPDATE SET query_state = excluded.query_state,
                 collapse_threads = excluded.collapse_threads,
                 can_calculate_changes = excluded.can_calculate_changes,
                 total = excluded.total, ids_blob = excluded.ids_blob,
                 updated_at = excluded.updated_at",
            params![
                key,
                state.as_str(),
                collapse,
                can_calculate,
                total.map(to_i64),
                encode_ids(ids)?,
                ts_ms(now)
            ],
        )?;
        Ok(())
    }

    /// Applies `Email/queryChanges` to a cached query: removes the removed
    /// ids, then inserts the added ones in index order. Added indexes past
    /// the cached window are dropped, since the window only covers the
    /// start of the result.
    pub fn apply_query_changes(
        &mut self,
        key: &str,
        changes: &QueryChangesResponse,
        now: Timestamp,
    ) -> Result<QueryChangesOutcome> {
        type Cached = (String, bool, bool, Vec<u8>, Option<i64>);
        let cached: Option<Cached> = self
            .conn()
            .query_row(
                "SELECT query_state, collapse_threads, can_calculate_changes, ids_blob, total
                 FROM query_cache WHERE query_key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((state, collapse, can_calculate, blob, old_total)) = cached else {
            return Ok(QueryChangesOutcome::NotCached);
        };
        if state != changes.old_query_state.as_str() {
            return Ok(QueryChangesOutcome::StateMismatch);
        }
        let mut ids = decode_ids(&blob)?;
        let removed: HashSet<&str> = changes.removed.iter().map(Id::as_str).collect();
        let added_ids: HashSet<&str> = changes.added.iter().map(|a| a.id.as_str()).collect();
        // An added id that is also in the window moved; RFC 8620 lists it in
        // removed too, but be safe if a server does not.
        ids.retain(|id| !removed.contains(id.as_str()) && !added_ids.contains(id.as_str()));
        let mut added = changes.added.clone();
        added.sort_by_key(|a| a.index);
        for item in added {
            let index = usize::try_from(item.index).unwrap_or(usize::MAX);
            if index > ids.len() {
                continue;
            }
            ids.insert(index, item.id);
        }
        // Without a fresh total from the server, adjust the old one by the
        // net change.
        let total = changes.total.or_else(|| {
            old_total.and_then(|t| {
                let delta = usize_i64(changes.added.len()) - usize_i64(changes.removed.len());
                u64::try_from(t + delta).ok()
            })
        });
        self.write_query(
            key,
            &ids,
            &changes.new_query_state,
            collapse,
            can_calculate,
            total,
            now,
        )?;
        Ok(QueryChangesOutcome::Applied)
    }

    /// Forgets a cached query.
    pub fn drop_query(&mut self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM query_cache WHERE query_key = ?1", [key])?;
        Ok(())
    }

    // Commit.

    /// Confirms covered outbox entries, re-applies overlays, rebuilds
    /// conversation rows, trims the body cache and commits.
    pub fn commit(mut self) -> Result<()> {
        let confirmed = self.collect_confirmations()?;
        if !confirmed.is_empty() {
            let mut refresh: BTreeSet<String> = BTreeSet::new();
            for c in &confirmed {
                self.conn()
                    .prepare_cached("DELETE FROM overlay WHERE target_id = ?1 AND seq = ?2")?
                    .execute(params![c.target_id, c.seq])?;
                if c.kind == kind::EMAIL_PATCH || c.kind == kind::EMAIL_DESTROY {
                    refresh.insert(c.target_id.clone());
                }
            }
            for id in &refresh {
                derive::refresh_email(&self.tx, id, &mut self.dirty)?;
            }
            crate::outbox::confirm_in_state(
                &mut *self.state,
                confirmed.iter().map(|c| (c.seq, c.target_id.as_str())),
            )?;
        }
        if !self.fresh_bodies.is_empty() {
            self.evict_bodies()?;
        }
        derive::refresh_threads(&self.tx, &self.dirty.threads)?;
        self.tx.commit()?;
        Ok(())
    }

    fn collect_confirmations(&self) -> Result<Vec<Confirmed>> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn()
            .prepare("SELECT target_id, seq, kind FROM overlay WHERE done = 1 ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            Ok(Confirmed {
                target_id: r.get(0)?,
                seq: r.get(1)?,
                kind: r.get(2)?,
            })
        })?;
        for row in rows {
            let c = row?;
            let covered = match c.kind.as_str() {
                kind::EMAIL_PATCH | kind::EMAIL_DESTROY => {
                    self.touched_emails.contains(&c.target_id)
                }
                kind::MAILBOX_CREATE => self.applied_mailboxes,
                kind::MASKED_CREATE | kind::MASKED_UPDATE => self.applied_masked,
                kind::CONTACT_CREATE => self.applied_contacts,
                _ => false,
            };
            if covered {
                out.push(c);
            }
        }
        Ok(out)
    }

    fn evict_bodies(&self) -> Result<()> {
        let cap = to_i64(self.options.body_cache_bytes);
        let mut total: i64 =
            self.conn()
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM body_meta", [], |r| {
                    r.get(0)
                })?;
        if total <= cap {
            return Ok(());
        }
        let mut victims: Vec<(i64, i64)> = Vec::new();
        {
            let mut stmt = self.conn().prepare(
                "SELECT email_rid, bytes FROM body_meta
                 ORDER BY coalesce(opened_at, fetched_at), email_rid",
            )?;
            for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
                let (rid, bytes): (i64, i64) = row?;
                if total <= cap {
                    break;
                }
                if self.fresh_bodies.contains(&rid) {
                    continue;
                }
                victims.push((rid, bytes));
                total -= bytes;
            }
        }
        for (rid, _) in &victims {
            self.conn()
                .prepare_cached("DELETE FROM body_meta WHERE email_rid = ?1")?
                .execute([rid])?;
            self.conn()
                .prepare_cached("UPDATE search_doc SET body_text = NULL WHERE rowid = ?1")?
                .execute([rid])?;
        }
        if !victims.is_empty() {
            tracing::debug!(evicted = victims.len(), "trimmed the body cache");
        }
        Ok(())
    }
}

struct Confirmed {
    target_id: String,
    seq: i64,
    kind: String,
}

/// The masked header if it was fetched: `Some(Some(address))` when present,
/// `Some(None)` when fetched and absent, `None` when not fetched.
fn masked_header_of(e: &Email) -> Option<Option<String>> {
    let mut fetched = false;
    for name in [MASKED_EMAIL_HEADER, MASKED_EMAIL_HEADER_ALT] {
        let key = format!("header:{name}:asText");
        if !e.extra.keys().any(|k| k.eq_ignore_ascii_case(&key)) {
            continue;
        }
        fetched = true;
        if let Some(value) = e.header_text(name) {
            let address = normalise_address(value);
            if !address.is_empty() {
                return Some(Some(address));
            }
        }
    }
    fetched.then_some(None)
}

fn from_text(e: &Email) -> String {
    addresses_text(e.from.as_deref().into_iter().chain(e.sender.as_deref()))
}

fn to_text(e: &Email) -> String {
    addresses_text(
        [e.to.as_deref(), e.cc.as_deref(), e.bcc.as_deref()]
            .into_iter()
            .flatten(),
    )
}
