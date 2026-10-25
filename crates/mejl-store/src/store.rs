//! The writer: opening an account directory, and the small `state.db`
//! records (drafts, rules, render history, UI state).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use mejl_jmap::{Id, Session, State};
use rusqlite::{Connection, OptionalExtension, params};

use crate::batch::SyncBatch;
use crate::codec::{json, ts_ms};
use crate::derive::{self, Dirty};
use crate::error::{Error, Result};
use crate::model::{Draft, RuleRecord};
use crate::read::{self, ReaderFactory};
use crate::schema::{self, CACHE_FILE, STATE_FILE};

/// Tuning for a [`Store`].
#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// The most body text the cache keeps, in bytes. When a sync batch
    /// pushes the total over this, the least recently opened bodies are
    /// dropped. Bodies stored in that same batch are kept, so a message the
    /// user just opened never vanishes.
    pub body_cache_bytes: u64,
    /// How long a connection waits for a lock before failing.
    pub busy_timeout: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            body_cache_bytes: 256 * 1024 * 1024,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

/// The local store for one account: the writer connections to `cache.db`
/// and `state.db`.
///
/// Exactly one `Store` should be open per account directory, owned by the
/// store thread. The UI reads through [`Reader`](crate::Reader)s made by
/// [`Store::reader_factory`].
#[derive(Debug)]
pub struct Store {
    pub(crate) dir: PathBuf,
    pub(crate) cache: Connection,
    pub(crate) state: Connection,
    pub(crate) options: StoreOptions,
}

impl Store {
    /// Opens or creates the store in `dir`, creating the directory if needed.
    ///
    /// This migrates `state.db`, rebuilds `cache.db` if its schema is stale,
    /// returns entries left `in_flight` by a crash to `pending` (marking them
    /// interrupted), and re-derives every pending overlay from `state.db`.
    pub fn open(dir: impl AsRef<Path>, options: StoreOptions) -> Result<Store> {
        let dir = dir.as_ref().to_owned();
        create_private_dir(&dir)?;
        let state = schema::open_state(&dir.join(STATE_FILE), options.busy_timeout)?;
        let cache = schema::open_cache(&dir.join(CACHE_FILE), options.busy_timeout)?;
        let mut store = Store {
            dir,
            cache,
            state,
            options,
        };
        store.recover()?;
        Ok(store)
    }

    /// The account directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A cheap, cloneable handle that opens read-only connections.
    pub fn reader_factory(&self) -> ReaderFactory {
        ReaderFactory::new(&self.dir, self.options.busy_timeout)
    }

    /// A new read-only connection pair.
    pub fn reader(&self) -> Result<crate::Reader> {
        self.reader_factory().open()
    }

    /// Starts a sync batch: one cache transaction that commits all of its
    /// changes, or none, and re-applies pending overlays before commit.
    pub fn batch(&mut self) -> Result<SyncBatch<'_>> {
        SyncBatch::begin(self)
    }

    fn recover(&mut self) -> Result<()> {
        let interrupted = self.state.execute(
            "UPDATE outbox SET status = 'pending', interrupted = 1 WHERE status = 'in_flight'",
            [],
        )?;
        if interrupted > 0 {
            tracing::warn!(
                interrupted,
                "outbox entries were in flight at the last exit"
            );
        }
        self.rebuild_overlays()
    }

    /// Rebuilds the overlay mirror in `cache.db` from `state.db`, then
    /// re-derives every email that had or now has an overlay.
    pub(crate) fn rebuild_overlays(&mut self) -> Result<()> {
        let mut rows = Vec::new();
        {
            let mut stmt = self.state.prepare(
                "SELECT t.target_id, t.seq, o.kind, o.payload_json, t.patch_json, o.status, o.result_id
                 FROM outbox_target t JOIN outbox o ON o.seq = t.seq
                 WHERE o.overlay_active = 1 AND t.state = 'active'
                   AND o.status IN ('pending', 'in_flight', 'done')
                 ORDER BY t.seq",
            )?;
            let mapped = stmt.query_map([], |r| {
                Ok(MirrorSource {
                    target_id: r.get(0)?,
                    seq: r.get(1)?,
                    kind: r.get(2)?,
                    payload: r.get(3)?,
                    patch: r.get(4)?,
                    done: r.get::<_, String>(5)? == "done",
                    result_id: r.get(6)?,
                })
            })?;
            for row in mapped {
                rows.push(row?);
            }
        }
        let tx = self.cache.transaction()?;
        let mut emails: BTreeSet<String> = BTreeSet::new();
        {
            let mut stmt = tx.prepare("SELECT id FROM email WHERE has_overlay = 1")?;
            for id in stmt.query_map([], |r| r.get::<_, String>(0))? {
                emails.insert(id?);
            }
        }
        tx.execute("DELETE FROM overlay", [])?;
        for row in &rows {
            let Some((mirror_kind, payload)) = crate::outbox::mirror_entry(
                &row.kind,
                row.payload.as_deref(),
                row.patch.as_deref(),
            )?
            else {
                continue;
            };
            tx.execute(
                "INSERT INTO overlay (target_id, seq, kind, payload_json, done, result_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.target_id,
                    row.seq,
                    mirror_kind,
                    payload,
                    row.done,
                    row.result_id
                ],
            )?;
            if mirror_kind == derive::kind::EMAIL_PATCH
                || mirror_kind == derive::kind::EMAIL_DESTROY
            {
                emails.insert(row.target_id.clone());
            }
        }
        let mut dirty = Dirty::default();
        for id in &emails {
            derive::refresh_email(&tx, id, &mut dirty)?;
        }
        derive::refresh_threads(&tx, &dirty.threads)?;
        tx.commit()?;
        Ok(())
    }

    /// The stored state string for a JMAP type, such as `Email`.
    pub fn type_state(&self, type_name: &str) -> Result<Option<State>> {
        read::type_state(&self.cache, type_name)
    }

    /// The cached session.
    pub fn session(&self) -> Result<Option<Session>> {
        read::session(&self.cache)
    }

    /// The cached query window for `key`, if any.
    pub fn query_page(
        &self,
        key: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<crate::QueryPage>> {
        read::query_page(&self.cache, key, offset, limit)
    }

    // Engine support: reads the sync engine makes on the store thread.

    /// The cached emails a mailbox holds now, newest first, with pending
    /// local changes applied. Emails of threads in `except_threads` are left
    /// out. This expands "every conversation in this mailbox" on the store
    /// thread, so the UI never collects the ids. Only cached mail is listed.
    pub fn mailbox_email_ids(&self, mailbox_id: &str, except_threads: &[Id]) -> Result<Vec<Id>> {
        let except: BTreeSet<&str> = except_threads.iter().map(Id::as_str).collect();
        let mut stmt = self.cache.prepare_cached(
            "SELECT e.id, e.thread_id FROM email_mailbox m JOIN email e ON e.rid = m.email_rid
             WHERE m.mailbox_id = ?1 ORDER BY m.received_at DESC, m.email_rid DESC",
        )?;
        let rows = stmt.query_map([mailbox_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, thread) = row?;
            if !except.contains(thread.as_str()) {
                out.push(Id::from(id));
            }
        }
        Ok(out)
    }

    /// The mailboxes each cached email in `ids` is in now, with pending
    /// local changes applied. Ids that are not cached are left out.
    pub fn email_mailboxes(&self, ids: &[Id]) -> Result<BTreeMap<Id, Vec<Id>>> {
        let mut stmt = self.cache.prepare_cached(
            "SELECT m.mailbox_id FROM email e JOIN email_mailbox m ON m.email_rid = e.rid
             WHERE e.id = ?1",
        )?;
        let mut exists = self
            .cache
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM email WHERE id = ?1)")?;
        let mut out = BTreeMap::new();
        for id in ids {
            if !exists.query_row([id.as_str()], |r| r.get::<_, bool>(0))? {
                continue;
            }
            let mailboxes: Vec<Id> = stmt
                .query_map([id.as_str()], |r| r.get::<_, String>(0))?
                .map(|m| m.map(Id::from))
                .collect::<rusqlite::Result<_>>()?;
            out.insert(id.clone(), mailboxes);
        }
        Ok(out)
    }

    /// The ids in `ids` that are not cached, in the given order.
    pub fn missing_emails(&self, ids: &[Id]) -> Result<Vec<Id>> {
        let mut stmt = self
            .cache
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM email WHERE id = ?1)")?;
        let mut out = Vec::new();
        for id in ids {
            if !stmt.query_row([id.as_str()], |r| r.get::<_, bool>(0))? {
                out.push(id.clone());
            }
        }
        Ok(out)
    }

    /// Every cached email id, for a full resync after the server can no
    /// longer list changes.
    pub fn cached_email_ids(&self) -> Result<Vec<Id>> {
        let mut stmt = self.cache.prepare("SELECT id FROM email ORDER BY rid")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows
            .map(|r| r.map(Id::from))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Every thread id the cache knows, from stored threads and from cached
    /// emails, for a full thread resync.
    pub fn cached_thread_ids(&self) -> Result<Vec<Id>> {
        let mut stmt = self
            .cache
            .prepare("SELECT id FROM thread UNION SELECT thread_id FROM email ORDER BY 1")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows
            .map(|r| r.map(Id::from))
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Records that the user opened a message, so the body cache evicts it
    /// last. The UI sends this to the store thread when a body is shown.
    pub fn touch_body(&mut self, email_id: &str, now: Timestamp) -> Result<bool> {
        let n = self.cache.execute(
            "UPDATE body_meta SET opened_at = ?2
             WHERE email_rid = (SELECT rid FROM email WHERE id = ?1)",
            params![email_id, ts_ms(now)],
        )?;
        Ok(n > 0)
    }

    // Drafts.

    /// Saves a draft, replacing any earlier version with the same local id.
    /// The write is one transaction on the fully synced `state.db`, so the
    /// previous version survives a crash in the middle of it.
    ///
    /// A server copy already recorded for the draft is kept: the engine
    /// owns that id, and a caller that read the row earlier may hold an
    /// older one. The given `server_email_id` only fills an empty slot, for
    /// a draft opened from the server.
    pub fn save_draft(&mut self, draft: &Draft) -> Result<()> {
        if draft.local_id.is_empty() {
            return Err(Error::Invalid("a draft needs a local id".into()));
        }
        self.state.execute(
            "INSERT INTO draft (local_id, server_email_id, identity_id, headers_json, body, format,
                 attachments_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (local_id) DO UPDATE SET
                 server_email_id = coalesce(draft.server_email_id, excluded.server_email_id),
                 identity_id = excluded.identity_id,
                 headers_json = excluded.headers_json,
                 body = excluded.body,
                 format = excluded.format,
                 attachments_json = excluded.attachments_json,
                 updated_at = excluded.updated_at",
            params![
                draft.local_id,
                draft.server_email_id.as_ref().map(Id::as_str),
                draft.identity_id.as_ref().map(Id::as_str),
                json(&draft.headers)?,
                draft.body,
                draft.format,
                json(&draft.attachments)?,
                ts_ms(draft.created_at),
                ts_ms(draft.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Records the server copy of a draft. Returns false if the draft is gone.
    pub fn set_draft_server_id(&mut self, local_id: &str, email_id: Option<&Id>) -> Result<bool> {
        let n = self.state.execute(
            "UPDATE draft SET server_email_id = ?2 WHERE local_id = ?1",
            params![local_id, email_id.map(Id::as_str)],
        )?;
        Ok(n > 0)
    }

    /// Deletes a local draft. Returns false if it did not exist.
    ///
    /// Its server copies go to the draft trash in the same transaction, so
    /// they are destroyed even if the deletion happens offline or mejl
    /// exits first. See [`Store::draft_trash`].
    pub fn delete_draft(&mut self, local_id: &str, now: Timestamp) -> Result<bool> {
        let tx = self.state.transaction()?;
        tx.execute(
            "INSERT INTO draft_trash (email_id, message_id, created_at)
             SELECT server_email_id, server_message_id, ?2 FROM draft
             WHERE local_id = ?1 AND (server_email_id IS NOT NULL OR server_message_id IS NOT NULL)",
            params![local_id, ts_ms(now)],
        )?;
        let n = tx.execute("DELETE FROM draft WHERE local_id = ?1", [local_id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    // Rules.

    /// Inserts or replaces a rule.
    pub fn save_rule(&mut self, rule: &RuleRecord) -> Result<()> {
        if rule.id.is_empty() {
            return Err(Error::Invalid("a rule needs an id".into()));
        }
        self.state.execute(
            "INSERT INTO rule (id, name, enabled, sort_order, rule_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled,
                 sort_order = excluded.sort_order, rule_json = excluded.rule_json,
                 updated_at = excluded.updated_at",
            params![
                rule.id,
                rule.name,
                rule.enabled,
                rule.sort_order,
                rule.rule_json,
                ts_ms(rule.updated_at)
            ],
        )?;
        Ok(())
    }

    /// Turns a rule on or off. Returns false if it does not exist.
    pub fn set_rule_enabled(&mut self, id: &str, enabled: bool, now: Timestamp) -> Result<bool> {
        let n = self.state.execute(
            "UPDATE rule SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, enabled, ts_ms(now)],
        )?;
        Ok(n > 0)
    }

    /// Deletes a rule. Returns false if it did not exist.
    pub fn delete_rule(&mut self, id: &str) -> Result<bool> {
        let n = self.state.execute("DELETE FROM rule WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    /// Sets the order of rules to the order of `ids`. Rules not listed keep
    /// their order but sort after the listed ones.
    pub fn reorder_rules(&mut self, ids: &[&str], now: Timestamp) -> Result<()> {
        let tx = self.state.transaction()?;
        let listed = i64::try_from(ids.len()).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE rule SET sort_order = sort_order + ?1 + 1000000",
            [listed],
        )?;
        for (pos, id) in ids.iter().enumerate() {
            let n = tx.execute(
                "UPDATE rule SET sort_order = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, i64::try_from(pos).unwrap_or(i64::MAX), ts_ms(now)],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no rule with id {id}")));
            }
        }
        // Close the gap the offset above opened, keeping relative order.
        let mut rest: Vec<String> = Vec::new();
        {
            let mut stmt =
                tx.prepare("SELECT id FROM rule WHERE sort_order >= ?1 ORDER BY sort_order, id")?;
            for id in stmt.query_map([listed], |r| r.get(0))? {
                rest.push(id?);
            }
        }
        for (i, id) in rest.iter().enumerate() {
            tx.execute(
                "UPDATE rule SET sort_order = ?2 WHERE id = ?1",
                params![id, listed + i64::try_from(i).unwrap_or(i64::MAX)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Adds a rendered Sieve block to the render history and returns its id.
    pub fn record_rule_render(
        &mut self,
        sieve: &str,
        body_sha256: &str,
        rendered_at: Timestamp,
    ) -> Result<i64> {
        self.state.execute(
            "INSERT INTO rule_render (rendered_at, sieve, body_sha256) VALUES (?1, ?2, ?3)",
            params![ts_ms(rendered_at), sieve, body_sha256],
        )?;
        Ok(self.state.last_insert_rowid())
    }

    /// Records that the user pasted the block with this id into Fastmail.
    pub fn confirm_rule_paste(&mut self, render_id: i64, at: Timestamp) -> Result<bool> {
        let n = self.state.execute(
            "UPDATE rule_render SET user_confirmed_paste_at = ?2 WHERE id = ?1",
            params![render_id, ts_ms(at)],
        )?;
        Ok(n > 0)
    }

    // UI state.

    /// Stores a UI value such as the last mailbox or pane ratios.
    pub fn set_ui_value(&mut self, key: &str, value: &str, now: Timestamp) -> Result<()> {
        self.state.execute(
            "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, ts_ms(now)],
        )?;
        Ok(())
    }

    /// Deletes a UI value. Returns false if it was not set.
    pub fn delete_ui_value(&mut self, key: &str) -> Result<bool> {
        let n = self
            .state
            .execute("DELETE FROM ui_state WHERE key = ?1", [key])?;
        Ok(n > 0)
    }

    /// A UI value.
    pub fn ui_value(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .state
            .query_row("SELECT value FROM ui_state WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

struct MirrorSource {
    target_id: String,
    seq: i64,
    kind: String,
    payload: Option<String>,
    patch: Option<String>,
    done: bool,
    result_id: Option<String>,
}

fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        // Mail and tokens live under here; keep other local users out.
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|source| Error::Io {
        path: dir.to_owned(),
        source,
    })
}
