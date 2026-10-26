//! Read-only access for the UI thread.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use mejl_jmap::types::{EmailHeader, Identity, MailboxRights, MaskedEmail, Role};
use mejl_jmap::{Id, Session, State};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::codec::{
    decode_ids, from_json, from_opt_json, like_escape, ms_ts, to_u64, ts_ms, usize_i64,
};
use crate::error::Result;
use crate::model::{
    AddressSuggestion, Attachment, Body, BodyPart, ConversationRow, Draft, DraftHeaders,
    EmailDetail, MailboxRow, MaskedEmailRow, MessageHeaders, MessageRow, OutboxSummary, Page,
    QueryPage, QueuedSend, RuleRecord, RuleRender,
};
use crate::outbox::Seq;
use crate::schema::{self, CACHE_FILE, CACHE_VERSION, STATE_FILE};
use crate::search::{self, SearchOptions, SearchResults};

/// Opens [`Reader`]s for one account directory. Cheap to clone and send to
/// other threads.
#[derive(Debug, Clone)]
pub struct ReaderFactory {
    cache: PathBuf,
    state: PathBuf,
    busy_timeout: Duration,
}

impl ReaderFactory {
    /// A factory for the store in `dir`. The store must have been opened
    /// with [`Store::open`](crate::Store::open) at least once.
    pub fn new(dir: impl AsRef<Path>, busy_timeout: Duration) -> Self {
        let dir = dir.as_ref();
        Self {
            cache: dir.join(CACHE_FILE),
            state: dir.join(STATE_FILE),
            busy_timeout,
        }
    }

    /// A new pair of read-only connections.
    pub fn open(&self) -> Result<Reader> {
        Ok(Reader {
            cache: schema::open_reader(&self.cache, CACHE_VERSION, self.busy_timeout)?,
            state: schema::open_reader(&self.state, schema::state_version(), self.busy_timeout)?,
        })
    }
}

/// Read-only connections to `cache.db` and `state.db`. Not `Sync`: make one
/// per thread with a [`ReaderFactory`].
///
/// Reads see committed data only. Pending outbox entries are already
/// applied to emails and mailbox counts by the writer; masked email and
/// contact overlays are applied here at read time.
#[derive(Debug)]
pub struct Reader {
    pub(crate) cache: Connection,
    pub(crate) state: Connection,
}

const ROW_COLUMNS: &str = "e.id, e.thread_id, e.received_at, e.subject, e.preview, e.from_json,
    e.to_json, e.cc_json, e.size, e.is_seen, e.is_flagged, e.is_answered, e.is_draft,
    e.has_attachment, e.masked_header IS NOT NULL, e.masked_email_id";

/// Columns after [`ROW_COLUMNS`] start at this index.
const ROW_LEN: usize = 16;

pub(crate) fn message_row(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        email_id: r.get(0)?,
        thread_id: r.get(1)?,
        received_at: r.get(2)?,
        subject: r.get(3)?,
        preview: r.get(4)?,
        from: r.get(5)?,
        to: r.get(6)?,
        cc: r.get(7)?,
        size: r.get(8)?,
        seen: r.get(9)?,
        flagged: r.get(10)?,
        answered: r.get(11)?,
        draft: r.get(12)?,
        has_attachment: r.get(13)?,
        masked: r.get(14)?,
        masked_email_id: r.get(15)?,
    })
}

/// A message row before its JSON columns are decoded. Decoding happens
/// outside the SQLite callback so errors keep their type.
pub(crate) struct RawRow {
    email_id: String,
    thread_id: String,
    received_at: i64,
    subject: String,
    preview: String,
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    size: i64,
    seen: bool,
    flagged: bool,
    answered: bool,
    draft: bool,
    has_attachment: bool,
    masked: bool,
    masked_email_id: Option<String>,
}

impl RawRow {
    pub(crate) fn decode(self) -> Result<MessageRow> {
        Ok(MessageRow {
            email_id: Id::from(self.email_id),
            thread_id: Id::from(self.thread_id),
            received_at: ms_ts(self.received_at),
            subject: self.subject,
            preview: self.preview,
            from: from_opt_json(self.from.as_deref())?,
            to: from_opt_json(self.to.as_deref())?,
            cc: from_opt_json(self.cc.as_deref())?,
            size: to_u64(self.size),
            seen: self.seen,
            flagged: self.flagged,
            answered: self.answered,
            draft: self.draft,
            has_attachment: self.has_attachment,
            masked: self.masked,
            masked_email_id: self.masked_email_id.map(Id::from),
        })
    }
}

pub(crate) fn row_columns() -> &'static str {
    ROW_COLUMNS
}

pub(crate) fn type_state(conn: &Connection, type_name: &str) -> Result<Option<State>> {
    Ok(conn
        .prepare_cached("SELECT state FROM sync_state WHERE type = ?1")?
        .query_row([type_name], |r| r.get::<_, String>(0))
        .optional()?
        .map(State::from))
}

pub(crate) fn session(conn: &Connection) -> Result<Option<Session>> {
    let text: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'session_json'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    text.as_deref().map(from_json).transpose()
}

pub(crate) fn query_page(
    conn: &Connection,
    key: &str,
    offset: usize,
    limit: usize,
) -> Result<Option<QueryPage>> {
    type Cached = (String, bool, bool, Option<i64>, Vec<u8>);
    let row: Option<Cached> = conn
        .prepare_cached(
            "SELECT query_state, collapse_threads, can_calculate_changes, total, ids_blob
             FROM query_cache WHERE query_key = ?1",
        )?
        .query_row([key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((state, collapse, can_calculate, total, blob)) = row else {
        return Ok(None);
    };
    let ids = decode_ids(&blob)?;
    let cached = ids.len();
    let ids = ids.into_iter().skip(offset).take(limit).collect();
    Ok(Some(QueryPage {
        ids,
        total: total.map(to_u64),
        query_state: State::from(state),
        collapse_threads: collapse,
        can_calculate_changes: can_calculate,
        cached,
    }))
}

pub(crate) fn undo_peek(state: &Connection) -> Result<Option<String>> {
    Ok(state
        .query_row("SELECT label FROM undo ORDER BY id DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .optional()?)
}

pub(crate) fn redo_peek(state: &Connection) -> Result<Option<String>> {
    Ok(state
        .query_row("SELECT label FROM redo ORDER BY id DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .optional()?)
}

/// A masked email's JSON with done and pending update overlays applied.
pub(crate) fn masked_effective_json(conn: &Connection, id: &str) -> Result<Option<Value>> {
    let stored: Option<String> = conn
        .query_row("SELECT json FROM masked_email WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let mut value: Value = from_json(&stored)?;
    let mut stmt = conn.prepare_cached(
        "SELECT payload_json FROM overlay WHERE target_id = ?1 AND kind = 'masked_update' ORDER BY seq",
    )?;
    for patch in stmt.query_map([id], |r| r.get::<_, Option<String>>(0))? {
        if let Some(patch) = patch? {
            apply_top_level(&mut value, &from_json(&patch)?);
        }
    }
    Ok(Some(value))
}

fn apply_top_level(value: &mut Value, patch: &mejl_jmap::Patch) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    for (path, v) in patch.as_map() {
        if path.contains('/') {
            continue;
        }
        if v.is_null() {
            obj.remove(path);
        } else {
            obj.insert(path.clone(), v.clone());
        }
    }
}

impl Reader {
    /// Every mailbox, ordered by `sortOrder` and then name, with counts
    /// adjusted for pending changes. Mailboxes created locally and not yet
    /// confirmed are included with `pending` set.
    pub fn mailboxes(&self) -> Result<Vec<MailboxRow>> {
        mailboxes(&self.cache)
    }

    /// A page of conversations in a mailbox, newest first.
    pub fn conversations(
        &self,
        mailbox_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Page<ConversationRow>> {
        conversations(&self.cache, mailbox_id, offset, limit)
    }

    /// A page of single messages in a mailbox, newest first.
    pub fn messages(
        &self,
        mailbox_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Page<MessageRow>> {
        messages(&self.cache, mailbox_id, offset, limit)
    }

    /// Every cached message of a thread, oldest first, with headers.
    /// Messages pending local deletion are left out.
    pub fn conversation(&self, thread_id: &str) -> Result<Vec<MessageHeaders>> {
        conversation(&self.cache, thread_id)
    }

    /// One message with its body, if cached.
    pub fn email(&self, email_id: &str) -> Result<Option<EmailDetail>> {
        email_detail(&self.cache, email_id)
    }

    /// Where a thread's row sits in a mailbox's conversation list, counted
    /// from the newest, or `None` when the thread has no row there. The UI
    /// uses it to keep the cursor on a conversation when new mail arrives
    /// above it. The count runs over the narrow order index only, so it
    /// costs about what skipping that many rows in a page read costs.
    pub fn conversation_position(
        &self,
        mailbox_id: &str,
        thread_id: &str,
    ) -> Result<Option<usize>> {
        let latest: Option<i64> = self
            .cache
            .prepare_cached(
                "SELECT latest_at FROM conversation INDEXED BY conversation_by_thread
                 WHERE thread_id = ?1 AND mailbox_id = ?2",
            )?
            .query_row([thread_id, mailbox_id], |r| r.get(0))
            .optional()?;
        let Some(latest) = latest else {
            return Ok(None);
        };
        let before: i64 = self
            .cache
            .prepare_cached(
                "SELECT COUNT(*) FROM conversation INDEXED BY conversation_order
                 WHERE mailbox_id = ?1
                   AND (latest_at > ?2 OR (latest_at = ?2 AND thread_id > ?3))",
            )?
            .query_row(params![mailbox_id, latest, thread_id], |r| r.get(0))?;
        Ok(Some(usize::try_from(before).unwrap_or(usize::MAX)))
    }

    /// The newest cached messages in any mailbox, newest first, with their
    /// headers. Messages pending local deletion are left out. For the rule
    /// dry run, which checks rules against recent mail.
    pub fn recent_messages(&self, limit: usize) -> Result<Vec<MessageHeaders>> {
        // Row ids are recency keys (received_at scaled), so this walks the
        // primary key backwards and stops after `limit` rows.
        let sql = headers_query("e.is_hidden = 0 ORDER BY e.rid DESC LIMIT ?1");
        let mut stmt = self.cache.prepare_cached(&sql)?;
        let raw: Vec<RawHeaders> = stmt
            .query_map([usize_i64(limit)], raw_headers)?
            .collect::<Result<_, _>>()?;
        raw.into_iter()
            .map(|r| decode_headers(&self.cache, r))
            .collect()
    }

    /// The email ids of a thread as the server last listed them.
    pub fn thread_email_ids(&self, thread_id: &str) -> Result<Option<Vec<Id>>> {
        let text: Option<String> = self
            .cache
            .prepare_cached("SELECT email_ids_json FROM thread WHERE id = ?1")?
            .query_row([thread_id], |r| r.get(0))
            .optional()?;
        text.as_deref().map(from_json).transpose()
    }

    /// Sending identities in server order.
    pub fn identities(&self) -> Result<Vec<Identity>> {
        let mut stmt = self
            .cache
            .prepare_cached("SELECT json FROM identity ORDER BY pos, id")?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        rows.iter().map(|j| from_json(j)).collect()
    }

    /// Addresses for compose autocomplete whose address or any name word
    /// starts with `prefix`, ignoring case. Contacts and people mail was
    /// sent to are merged; the most recently used come first.
    pub fn address_suggestions(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<AddressSuggestion>> {
        address_suggestions(&self.cache, prefix, limit)
    }

    /// Every masked address with local message counts, pending local
    /// changes applied, newest activity first.
    pub fn masked_emails(&self) -> Result<Vec<MaskedEmailRow>> {
        masked_emails(&self.cache)
    }

    /// The masked address with this address, ignoring case.
    pub fn masked_email_by_address(&self, address: &str) -> Result<Option<MaskedEmailRow>> {
        let wanted = crate::codec::normalise_address(address);
        Ok(masked_emails(&self.cache)?.into_iter().find(|m| {
            m.masked
                .email
                .as_deref()
                .is_some_and(|e| crate::codec::normalise_address(e) == wanted)
        }))
    }

    /// Searches the local index. See [`crate::search`] for the syntax.
    pub fn search(&self, query: &str, options: &SearchOptions) -> Result<SearchResults> {
        search::run(&self.cache, query, options)
    }

    /// The stored state string for a JMAP type.
    pub fn type_state(&self, type_name: &str) -> Result<Option<State>> {
        type_state(&self.cache, type_name)
    }

    /// The cached session.
    pub fn session(&self) -> Result<Option<Session>> {
        session(&self.cache)
    }

    /// A slice of a cached query result.
    pub fn query_page(&self, key: &str, offset: usize, limit: usize) -> Result<Option<QueryPage>> {
        query_page(&self.cache, key, offset, limit)
    }

    /// Whether a query result is cached under `key`, without reading it.
    pub fn has_query(&self, key: &str) -> Result<bool> {
        Ok(self
            .cache
            .prepare_cached("SELECT 1 FROM query_cache WHERE query_key = ?1")?
            .query_row([key], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Rules in render order.
    pub fn rules(&self) -> Result<Vec<RuleRecord>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT id, name, enabled, sort_order, rule_json, updated_at FROM rule
             ORDER BY sort_order, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RuleRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                enabled: r.get(2)?,
                sort_order: r.get(3)?,
                rule_json: r.get(4)?,
                updated_at: ms_ts(r.get(5)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The newest rendered Sieve blocks first.
    pub fn rule_renders(&self, limit: usize) -> Result<Vec<RuleRender>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT id, rendered_at, sieve, body_sha256, user_confirmed_paste_at FROM rule_render
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([usize_i64(limit)], |r| {
            Ok(RuleRender {
                id: r.get(0)?,
                rendered_at: ms_ts(r.get(1)?),
                sieve: r.get(2)?,
                body_sha256: r.get(3)?,
                user_confirmed_paste_at: r.get::<_, Option<i64>>(4)?.map(ms_ts),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Local drafts, most recently changed first.
    pub fn drafts(&self) -> Result<Vec<Draft>> {
        self.read_drafts(None)
    }

    /// One local draft.
    pub fn draft(&self, local_id: &str) -> Result<Option<Draft>> {
        Ok(self.read_drafts(Some(local_id))?.into_iter().next())
    }

    fn read_drafts(&self, only: Option<&str>) -> Result<Vec<Draft>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT local_id, server_email_id, identity_id, headers_json, body, format,
                 attachments_json, created_at, updated_at
             FROM draft WHERE ?1 IS NULL OR local_id = ?1 ORDER BY updated_at DESC, local_id",
        )?;
        type RawDraft = (
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            i64,
            i64,
        );
        let rows: Vec<RawDraft> = stmt
            .query_map([only], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(
                |(
                    local_id,
                    server,
                    identity,
                    headers,
                    body,
                    format,
                    attachments,
                    created,
                    updated,
                )| {
                    Ok(Draft {
                        local_id,
                        server_email_id: server.map(Id::from),
                        identity_id: identity.map(Id::from),
                        headers: from_json::<DraftHeaders>(&headers)?,
                        body,
                        format,
                        attachments: from_json(&attachments)?,
                        created_at: ms_ts(created),
                        updated_at: ms_ts(updated),
                    })
                },
            )
            .collect()
    }

    /// Counts for the status line, such as "offline, 3 changes pending".
    pub fn outbox_summary(&self, now: Timestamp) -> Result<OutboxSummary> {
        outbox_summary(&self.state, now)
    }

    /// Sends still in the outbox, oldest first.
    pub fn queued_sends(&self, now: Timestamp) -> Result<Vec<QueuedSend>> {
        queued_sends(&self.state, now)
    }

    /// The label of the action undo would revert next.
    pub fn undo_peek(&self) -> Result<Option<String>> {
        undo_peek(&self.state)
    }

    /// The label of the action redo would apply again next.
    pub fn redo_peek(&self) -> Result<Option<String>> {
        redo_peek(&self.state)
    }

    /// A UI value.
    pub fn ui_value(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .state
            .prepare_cached("SELECT value FROM ui_state WHERE key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()?)
    }
}

pub(crate) fn outbox_summary(state: &Connection, now: Timestamp) -> Result<OutboxSummary> {
    let now = ts_ms(now);
    let (pending, held, failed): (i64, i64, i64) = state
        .prepare_cached(
            "SELECT
                 COALESCE(SUM(status IN ('pending', 'in_flight')
                     AND NOT (status = 'pending' AND hold_until IS NOT NULL AND hold_until > ?1)), 0),
                 COALESCE(SUM(status = 'pending' AND hold_until IS NOT NULL AND hold_until > ?1), 0),
                 COALESCE(SUM(status = 'failed'), 0)
             FROM outbox WHERE status IN ('pending', 'in_flight', 'failed')",
        )?
        .query_row([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(OutboxSummary {
        pending: to_u64(pending),
        held: to_u64(held),
        failed: to_u64(failed),
    })
}

pub(crate) fn queued_sends(state: &Connection, now: Timestamp) -> Result<Vec<QueuedSend>> {
    let now = ts_ms(now);
    let mut stmt = state.prepare_cached(
        "SELECT seq, label, hold_until, status, attempts, last_error FROM outbox
         WHERE kind = 'email_create_and_submit' AND status IN ('pending', 'in_flight') ORDER BY seq",
    )?;
    let rows = stmt.query_map([], |r| {
        let hold: Option<i64> = r.get(2)?;
        Ok(QueuedSend {
            seq: Seq(r.get(0)?),
            label: r.get(1)?,
            hold_until: hold.filter(|h| *h > now).map(ms_ts),
            in_flight: r.get::<_, String>(3)? == "in_flight",
            attempts: u32::try_from(r.get::<_, i64>(4)?).unwrap_or(u32::MAX),
            last_error: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn mailboxes(conn: &Connection) -> Result<Vec<MailboxRow>> {
    let mut deltas: HashMap<String, [i64; 4]> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT mailbox_id, SUM(d_total_emails), SUM(d_unread_emails), SUM(d_total_threads),
                 SUM(d_unread_threads) FROM count_delta GROUP BY mailbox_id",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
            ))
        })? {
            let (id, d) = row?;
            deltas.insert(id, d);
        }
    }
    let adjust = |base: i64, delta: i64| to_u64(base.saturating_add(delta));
    let mut out = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT m.id, m.parent_id, m.name, m.role, m.sort_order, m.total_emails, m.unread_emails,
                 m.total_threads, m.unread_threads, m.is_subscribed, m.json,
                 COALESCE(l.messages, 0), COALESCE(l.conversations, 0)
             FROM mailbox m LEFT JOIN mailbox_local l ON l.mailbox_id = m.id
             ORDER BY m.sort_order, m.name, m.id",
        )?;
        type RawMailbox = (
            String,
            Option<String>,
            String,
            Option<String>,
            i64,
            [i64; 4],
            bool,
            String,
            i64,
            i64,
        );
        let rows: Vec<RawMailbox> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, parent, name, role, sort, counts, subscribed, json, local_m, local_c) in rows {
            let d = deltas.get(&id).copied().unwrap_or_default();
            let rights: Option<MailboxRights> = from_json::<Value>(&json)?
                .get("myRights")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?;
            out.push(MailboxRow {
                id: Id::from(id),
                parent_id: parent.map(Id::from),
                name,
                role: role.as_deref().map(Role::from),
                sort_order: u32::try_from(sort).unwrap_or(0),
                total_emails: adjust(counts[0], d[0]),
                unread_emails: adjust(counts[1], d[1]),
                total_threads: adjust(counts[2], d[2]),
                unread_threads: adjust(counts[3], d[3]),
                local_messages: to_u64(local_m),
                local_conversations: to_u64(local_c),
                is_subscribed: subscribed,
                my_rights: rights,
                pending: false,
            });
        }
    }
    let known: BTreeSet<String> = out.iter().map(|m| m.id.as_str().to_owned()).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT target_id, payload_json, result_id FROM overlay WHERE kind = 'mailbox_create' ORDER BY seq",
    )?;
    let pending: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let local: HashMap<String, (i64, i64)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT mailbox_id, messages, conversations FROM mailbox_local WHERE mailbox_id LIKE 'local:%'",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?
    };
    for (temp, payload, result) in pending {
        let id = result.unwrap_or(temp);
        if known.contains(&id) {
            continue;
        }
        let mailbox: mejl_jmap::types::Mailbox = from_opt_json(payload.as_deref())?;
        let (local_m, local_c) = local.get(&id).copied().unwrap_or_default();
        out.push(MailboxRow {
            id: Id::from(id),
            parent_id: mailbox.parent_id,
            name: mailbox.name.unwrap_or_default(),
            role: mailbox.role,
            sort_order: mailbox.sort_order.unwrap_or(0),
            total_emails: to_u64(local_m),
            unread_emails: 0,
            total_threads: to_u64(local_c),
            unread_threads: 0,
            local_messages: to_u64(local_m),
            local_conversations: to_u64(local_c),
            is_subscribed: mailbox.is_subscribed.unwrap_or(true),
            my_rights: None,
            pending: true,
        });
    }
    Ok(out)
}

fn local_count(conn: &Connection, mailbox_id: &str, column: &str) -> Result<u64> {
    let sql = match column {
        "messages" => "SELECT messages FROM mailbox_local WHERE mailbox_id = ?1",
        _ => "SELECT conversations FROM mailbox_local WHERE mailbox_id = ?1",
    };
    let n: Option<i64> = conn
        .prepare_cached(sql)?
        .query_row([mailbox_id], |r| r.get(0))
        .optional()?;
    Ok(n.map_or(0, to_u64))
}

// Both page queries skip the offset in a subquery over key columns only,
// then join the page's rows. Joining first would read every skipped row,
// which made a 40k offset cost about 6 ms instead of 0.5 ms.
fn conversations(
    conn: &Connection,
    mailbox_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Page<ConversationRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.thread_id, c.latest_at, c.count_in_mailbox, c.thread_count, c.unread_count,
             c.any_flagged, c.any_attachment, c.any_masked, c.participants_json,
             e.id, e.subject, e.preview, e.from_json
         FROM (SELECT latest_at, thread_id FROM conversation INDEXED BY conversation_order
               WHERE mailbox_id = ?1
               ORDER BY latest_at DESC, thread_id DESC
               LIMIT ?2 OFFSET ?3) k
         JOIN conversation c
             ON c.mailbox_id = ?1 AND c.latest_at = k.latest_at AND c.thread_id = k.thread_id
         JOIN email e ON e.rid = c.latest_rid
         ORDER BY k.latest_at DESC, k.thread_id DESC",
    )?;
    type RawConv = (
        String,
        i64,
        i64,
        i64,
        i64,
        bool,
        bool,
        bool,
        String,
        String,
        String,
        String,
        Option<String>,
    );
    let rows: Vec<RawConv> = stmt
        .query_map(
            params![mailbox_id, usize_i64(limit), usize_i64(offset)],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )?
        .collect::<Result<_, _>>()?;
    let to_u32 = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(rows.len());
    for (
        thread,
        at,
        count,
        thread_count,
        unread,
        flagged,
        attach,
        masked,
        participants,
        id,
        subject,
        preview,
        from,
    ) in rows
    {
        out.push(ConversationRow {
            thread_id: Id::from(thread),
            email_id: Id::from(id),
            received_at: ms_ts(at),
            subject,
            preview,
            from: from_opt_json(from.as_deref())?,
            participants: from_json(&participants)?,
            count_in_mailbox: to_u32(count),
            thread_count: to_u32(thread_count),
            unread_count: to_u32(unread),
            flagged,
            has_attachment: attach,
            masked,
        });
    }
    Ok(Page {
        rows: out,
        offset,
        total: local_count(conn, mailbox_id, "conversations")?,
    })
}

fn messages(
    conn: &Connection,
    mailbox_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Page<MessageRow>> {
    let sql = format!(
        "SELECT {ROW_COLUMNS}
         FROM (SELECT received_at, email_rid FROM email_mailbox
               WHERE mailbox_id = ?1
               ORDER BY received_at DESC, email_rid DESC
               LIMIT ?2 OFFSET ?3) k
         JOIN email e ON e.rid = k.email_rid
         ORDER BY k.received_at DESC, k.email_rid DESC"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let raw: Vec<RawRow> = stmt
        .query_map(
            params![mailbox_id, usize_i64(limit), usize_i64(offset)],
            message_row,
        )?
        .collect::<Result<_, _>>()?;
    Ok(Page {
        rows: raw.into_iter().map(RawRow::decode).collect::<Result<_>>()?,
        offset,
        total: local_count(conn, mailbox_id, "messages")?,
    })
}

struct RawHeaders {
    row: RawRow,
    rid: i64,
    bcc: Option<String>,
    reply_to: Option<String>,
    sender: Option<String>,
    sent_at: Option<String>,
    message_id: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    masked_address: Option<String>,
    keywords: String,
    body_cached: bool,
}

fn headers_query(where_clause: &str) -> String {
    format!(
        "SELECT {ROW_COLUMNS}, e.rid, e.bcc_json, e.reply_to_json, e.sender_json, e.sent_at,
             e.message_id_json, e.in_reply_to_json, e.references_json, e.masked_header,
             e.keywords_json, EXISTS (SELECT 1 FROM body_meta b WHERE b.email_rid = e.rid)
         FROM email e WHERE {where_clause}"
    )
}

fn raw_headers(r: &Row<'_>) -> rusqlite::Result<RawHeaders> {
    let n = ROW_LEN;
    Ok(RawHeaders {
        row: message_row(r)?,
        rid: r.get(n)?,
        bcc: r.get(n + 1)?,
        reply_to: r.get(n + 2)?,
        sender: r.get(n + 3)?,
        sent_at: r.get(n + 4)?,
        message_id: r.get(n + 5)?,
        in_reply_to: r.get(n + 6)?,
        references: r.get(n + 7)?,
        masked_address: r.get(n + 8)?,
        keywords: r.get(n + 9)?,
        body_cached: r.get(n + 10)?,
    })
}

fn decode_headers(conn: &Connection, raw: RawHeaders) -> Result<MessageHeaders> {
    let keywords: BTreeMap<String, bool> = from_json(&raw.keywords)?;
    let mut mailbox_ids = BTreeSet::new();
    let mut stmt =
        conn.prepare_cached("SELECT mailbox_id FROM email_mailbox WHERE email_rid = ?1")?;
    for m in stmt.query_map([raw.rid], |r| r.get::<_, String>(0))? {
        mailbox_ids.insert(Id::from(m?));
    }
    Ok(MessageHeaders {
        row: raw.row.decode()?,
        bcc: from_opt_json(raw.bcc.as_deref())?,
        reply_to: from_opt_json(raw.reply_to.as_deref())?,
        sender: from_opt_json(raw.sender.as_deref())?,
        sent_at: raw.sent_at,
        message_id: from_opt_json(raw.message_id.as_deref())?,
        in_reply_to: from_opt_json(raw.in_reply_to.as_deref())?,
        references: from_opt_json(raw.references.as_deref())?,
        masked_address: raw.masked_address,
        keywords: keywords
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(k, _)| k)
            .collect(),
        mailbox_ids,
        body_cached: raw.body_cached,
    })
}

fn conversation(conn: &Connection, thread_id: &str) -> Result<Vec<MessageHeaders>> {
    let sql = headers_query("e.thread_id = ?1 AND e.is_hidden = 0 ORDER BY e.received_at, e.rid");
    let mut stmt = conn.prepare_cached(&sql)?;
    let raw: Vec<RawHeaders> = stmt
        .query_map([thread_id], raw_headers)?
        .collect::<Result<_, _>>()?;
    raw.into_iter().map(|r| decode_headers(conn, r)).collect()
}

fn email_detail(conn: &Connection, email_id: &str) -> Result<Option<EmailDetail>> {
    let sql = headers_query("e.id = ?1");
    let raw: Option<RawHeaders> = conn
        .prepare_cached(&sql)?
        .query_row([email_id], raw_headers)
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let rid = raw.rid;
    let body_cached = raw.body_cached;
    let headers = decode_headers(conn, raw)?;
    let body = if body_cached {
        Some(body(conn, rid)?)
    } else {
        None
    };
    Ok(Some(EmailDetail { headers, body }))
}

fn body(conn: &Connection, rid: i64) -> Result<Body> {
    let (headers_json, fetched, opened): (Option<String>, i64, Option<i64>) = conn
        .prepare_cached(
            "SELECT headers_json, fetched_at, opened_at FROM body_meta WHERE email_rid = ?1",
        )?
        .query_row([rid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT kind, part_id, blob_id, mime_type, charset, value, is_truncated, is_encoding_problem
             FROM body_part WHERE email_rid = ?1 ORDER BY kind, idx",
        )?;
        let rows = stmt.query_map([rid], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                BodyPart {
                    part_id: r.get(1)?,
                    blob_id: r.get::<_, Option<String>>(2)?.map(Id::from),
                    mime_type: r.get(3)?,
                    charset: r.get(4)?,
                    value: r.get(5)?,
                    is_truncated: r.get(6)?,
                    is_encoding_problem: r.get(7)?,
                },
            ))
        })?;
        for row in rows {
            let (kind, part) = row?;
            if kind == 0 {
                text_parts.push(part);
            } else {
                html_parts.push(part);
            }
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT part_id, blob_id, name, mime_type, size, disposition, cid FROM attachment
         WHERE email_rid = ?1 ORDER BY idx",
    )?;
    let attachments = stmt
        .query_map([rid], |r| {
            Ok(Attachment {
                part_id: r.get(0)?,
                blob_id: r.get::<_, Option<String>>(1)?.map(Id::from),
                name: r.get(2)?,
                mime_type: r.get(3)?,
                size: to_u64(r.get(4)?),
                disposition: r.get(5)?,
                cid: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Body {
        text_parts,
        html_parts,
        attachments,
        headers: from_opt_json::<Vec<EmailHeader>>(headers_json.as_deref())?,
        fetched_at: ms_ts(fetched),
        opened_at: opened.map(ms_ts),
    })
}

fn address_suggestions(
    conn: &Connection,
    prefix: &str,
    limit: usize,
) -> Result<Vec<AddressSuggestion>> {
    let prefix = prefix.trim().to_lowercase();
    if prefix.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let starts = format!("{}%", like_escape(&prefix));
    let word = format!("% {}%", like_escape(&prefix));
    let mut found: BTreeMap<String, AddressSuggestion> = BTreeMap::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT ca.address_lc, ca.address, c.name, c.id, r.last_used_at, r.use_count
             FROM contact_address ca JOIN contact c ON c.id = ca.contact_id
             LEFT JOIN correspondent r ON r.address_lc = ca.address_lc
             WHERE ca.address_lc LIKE ?1 ESCAPE '\\' OR c.name_words LIKE ?2 ESCAPE '\\'",
        )?;
        let rows = stmt.query_map(params![starts, word], |r| {
            Ok((
                r.get::<_, String>(0)?,
                AddressSuggestion {
                    email: r.get(1)?,
                    name: r.get(2)?,
                    contact_id: r.get::<_, Option<String>>(3)?.map(Id::from),
                    last_used: r.get::<_, Option<i64>>(4)?.map(ms_ts),
                    use_count: r.get::<_, Option<i64>>(5)?.map_or(0, to_u64),
                },
            ))
        })?;
        for row in rows {
            let (key, s) = row?;
            found.entry(key).or_insert(s);
        }
    }
    {
        let mut stmt = conn.prepare_cached(
            "SELECT address_lc, address, name, last_used_at, use_count FROM correspondent
             WHERE address_lc LIKE ?1 ESCAPE '\\' OR name_words LIKE ?2 ESCAPE '\\'",
        )?;
        let rows = stmt.query_map(params![starts, word], |r| {
            Ok((
                r.get::<_, String>(0)?,
                AddressSuggestion {
                    email: r.get(1)?,
                    name: r.get(2)?,
                    contact_id: None,
                    last_used: Some(ms_ts(r.get(3)?)),
                    use_count: to_u64(r.get(4)?),
                },
            ))
        })?;
        for row in rows {
            let (key, s) = row?;
            found.entry(key).or_insert(s);
        }
    }
    // Contacts created locally and not yet confirmed.
    {
        let mut stmt = conn.prepare_cached(
            "SELECT target_id, payload_json, result_id FROM overlay WHERE kind = 'contact_create'",
        )?;
        let rows: Vec<(String, Option<String>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (temp, payload, result) in rows {
            let card: mejl_jmap::types::ContactCard = from_opt_json(payload.as_deref())?;
            let name = card.display_name();
            let words = name
                .as_deref()
                .map(crate::codec::name_words)
                .unwrap_or_default();
            for address in card.email_addresses() {
                let key = address.to_lowercase();
                if key.starts_with(&prefix) || words.contains(&format!(" {prefix}")) {
                    found.entry(key).or_insert_with(|| AddressSuggestion {
                        name: name.clone(),
                        email: address.to_owned(),
                        contact_id: Some(Id::from(result.clone().unwrap_or_else(|| temp.clone()))),
                        last_used: None,
                        use_count: 0,
                    });
                }
            }
        }
    }
    let mut out: Vec<AddressSuggestion> = found.into_values().collect();
    out.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| b.use_count.cmp(&a.use_count))
            .then_with(|| {
                let a_addr = a.email.to_lowercase().starts_with(&prefix);
                let b_addr = b.email.to_lowercase().starts_with(&prefix);
                b_addr.cmp(&a_addr)
            })
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.email.cmp(&b.email))
    });
    out.truncate(limit);
    Ok(out)
}

fn masked_emails(conn: &Connection) -> Result<Vec<MaskedEmailRow>> {
    let mut stats: HashMap<String, (i64, i64, Option<i64>)> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT e.masked_email_id, COUNT(*),
                 SUM(EXISTS (SELECT 1 FROM email_mailbox em JOIN mailbox m ON m.id = em.mailbox_id
                             WHERE em.email_rid = e.rid AND m.role = 'junk')),
                 MAX(e.received_at)
             FROM email e WHERE e.masked_email_id IS NOT NULL AND e.is_hidden = 0
             GROUP BY e.masked_email_id",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })? {
            let (id, s) = row?;
            stats.insert(id, s);
        }
    }
    let mut overlays: HashMap<String, Vec<mejl_jmap::Patch>> = HashMap::new();
    let mut creates: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT target_id, kind, payload_json, result_id FROM overlay
             WHERE kind IN ('masked_update', 'masked_create') ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (target, kind, payload, result) = row?;
            if kind == "masked_update" {
                overlays
                    .entry(target)
                    .or_default()
                    .push(from_opt_json(payload.as_deref())?);
            } else {
                creates.push((target, payload, result));
            }
        }
    }
    let mut out = Vec::new();
    {
        let mut stmt = conn.prepare_cached("SELECT id, json FROM masked_email")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (id, json) in rows {
            let mut value: Value = from_json(&json)?;
            let pending = overlays.get(&id);
            for patch in pending.into_iter().flatten() {
                apply_top_level(&mut value, patch);
            }
            let (n, spam, last) = stats.get(&id).copied().unwrap_or((0, 0, None));
            out.push(MaskedEmailRow {
                masked: serde_json::from_value(value)?,
                local_messages: to_u64(n),
                local_in_spam: to_u64(spam),
                last_seen: last.map(ms_ts),
                pending: pending.is_some(),
            });
        }
    }
    let known: BTreeSet<String> = out
        .iter()
        .map(|m| m.masked.id.as_str().to_owned())
        .collect();
    for (temp, payload, result) in creates {
        let id = result.unwrap_or(temp);
        if known.contains(&id) {
            continue;
        }
        let mut masked: MaskedEmail = from_opt_json(payload.as_deref())?;
        masked.id = Id::from(id);
        out.push(MaskedEmailRow {
            masked,
            local_messages: 0,
            local_in_spam: 0,
            last_seen: None,
            pending: true,
        });
    }
    out.sort_by(|a, b| {
        let key = |m: &MaskedEmailRow| m.last_seen.max(m.masked.last_message_at);
        b.pending
            .cmp(&a.pending)
            .then_with(|| key(b).cmp(&key(a)))
            .then_with(|| a.masked.email.cmp(&b.masked.email))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::{Store, StoreOptions};

    #[test]
    fn reader_connections_refuse_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let reader = store.reader().unwrap();
        assert!(
            reader
                .cache
                .execute("INSERT INTO meta (key, value) VALUES ('a', 'b')", [])
                .is_err()
        );
        assert!(
            reader
                .state
                .execute(
                    "INSERT INTO ui_state (key, value, updated_at) VALUES ('a', 'b', 0)",
                    []
                )
                .is_err()
        );
        assert!(reader.mailboxes().unwrap().is_empty());
    }

    #[test]
    fn reader_needs_matching_state_version() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path(), StoreOptions::default()).unwrap());
        let state = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
        state.pragma_update(None, "user_version", 0).unwrap();
        drop(state);
        let err = crate::ReaderFactory::new(dir.path(), std::time::Duration::from_secs(1))
            .open()
            .unwrap_err();
        assert!(matches!(err, crate::Error::NotInitialised(_)));
    }
}
