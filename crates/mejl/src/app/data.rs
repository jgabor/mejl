//! [`MailData`] over mejl-store's [`Reader`], with the pending overlay.
//!
//! Every method is one or a few indexed reads on the UI thread's own read
//! connections, as mejl-store's budgets assume. Reads cannot fail in the
//! UI's API, so an error is logged, kept for one toast and answered with an
//! empty value; the next read tries again.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::tz::TimeZone;
use mejl_jmap::Id;
use mejl_store::{EmailFlags, Reader, SearchOptions};
use mejl_sync::RoleMailboxes;
use mejl_ui::{self as ui, MailData, Page};
use serde::{Deserialize, Serialize};

use super::convert;
use super::overlay::{CountDelta, Pending};
use crate::render_pool::BodyText;

/// The ui value key saved searches are stored under in `state.db`.
pub const SAVED_SEARCHES_KEY: &str = "saved_searches";
/// The ui value key pane sizes are stored under.
pub const PANE_SIZES_KEY: &str = "pane_sizes";

/// How many store hits a local search asks for per conversation wanted.
/// Hits are per message and several can share a conversation.
const HITS_PER_ROW: usize = 3;

/// A saved search as stored in `state.db`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    /// Local id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The query.
    pub query: String,
}

/// A row the overlay adds to a mailbox's list.
struct Added {
    /// Where it goes among the Reader's visible rows: before the row at
    /// this index once hidden rows are left out.
    base: usize,
    /// Its index in the list as the UI sees it.
    place: usize,
    /// The row.
    row: ui::ConversationRow,
}

/// The UI's read model over the local store.
pub struct StoreData {
    reader: Reader,
    pending: Pending,
    account_fallback: String,
    time_zone: TimeZone,
    saved: Vec<SavedSearch>,
    load_more: RefCell<Option<String>>,
    error: RefCell<Option<String>>,
}

impl std::fmt::Debug for StoreData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreData")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl StoreData {
    /// The read model over `reader`. `account` names the account until
    /// the session is cached. Search dates use `time_zone`.
    pub fn new(reader: Reader, account: &str, time_zone: TimeZone) -> StoreData {
        let mut data = StoreData {
            reader,
            pending: Pending::default(),
            account_fallback: account.to_owned(),
            time_zone,
            saved: Vec::new(),
            load_more: RefCell::new(None),
            error: RefCell::new(None),
        };
        let stored = data.ok("saved searches", data.reader.ui_value(SAVED_SEARCHES_KEY));
        data.saved = stored
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        data
    }

    /// The Reader, for reads the UI's trait does not cover.
    pub fn reader(&self) -> &Reader {
        &self.reader
    }

    /// The pending overlay.
    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    /// The pending overlay, to add or drop entries.
    pub fn pending_mut(&mut self) -> &mut Pending {
        &mut self.pending
    }

    /// A mailbox whose cached window the user scrolled past, once.
    pub fn take_load_more(&self) -> Option<String> {
        self.load_more.borrow_mut().take()
    }

    /// The first read error since the last call, for a toast.
    pub fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }

    /// Unwraps a read, logging and keeping the error.
    pub fn ok<T: Default>(&self, what: &str, result: mejl_store::Result<T>) -> T {
        match result {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read {what} from the local store");
                let mut slot = self.error.borrow_mut();
                if slot.is_none() {
                    *slot = Some(format!("Cannot read {what} from the local cache: {e}"));
                }
                T::default()
            }
        }
    }

    /// The role mailboxes.
    pub fn roles(&self) -> RoleMailboxes {
        RoleMailboxes::from_rows(&self.ok("mailboxes", self.reader.mailboxes()))
    }

    /// Saved searches, as stored.
    pub fn saved(&self) -> &[SavedSearch] {
        &self.saved
    }

    /// Adds a saved search and returns the whole list as JSON to store.
    pub fn add_saved(&mut self, name: &str, query: &str) -> String {
        let n = self.saved.len() + 1;
        let mut id = format!("s{n}");
        let mut k = n;
        while self.saved.iter().any(|s| s.id == id) {
            k += 1;
            id = format!("s{k}");
        }
        self.saved.push(SavedSearch {
            id,
            name: name.to_owned(),
            query: query.to_owned(),
        });
        serde_json::to_string(&self.saved).unwrap_or_else(|_| "[]".to_owned())
    }

    /// Every cached message of a thread with its effective flags: the
    /// Reader's, with pending overlay flags on top.
    pub fn thread_flags(&self, thread: &str) -> Vec<(String, EmailFlags)> {
        self.ok("a conversation", self.reader.conversation(thread))
            .iter()
            .map(|h| {
                let id = h.row.email_id.as_str().to_owned();
                let flags = self
                    .pending
                    .flags(&id)
                    .cloned()
                    .unwrap_or_else(|| convert::flags_of(h));
                (id, flags)
            })
            .collect()
    }

    /// How whole-mailbox actions move the counts. What an action takes is
    /// the mailbox's count now less what the conversations left out of it
    /// hold, so once the store has written it there is nothing left to
    /// take and nothing moves twice. A move adds what it takes to the
    /// target, which can count a conversation twice there for a moment if
    /// part of it was already in that mailbox.
    fn bulk_deltas(&self, views: &[ui::MailboxView], deltas: &mut HashMap<String, CountDelta>) {
        for b in self.pending.bulks() {
            let Some(source) = views.iter().find(|v| v.id.as_str() == b.mailbox) else {
                continue;
            };
            let mut kept = [0i64; 4];
            for thread in &b.except {
                let inside: Vec<EmailFlags> = self
                    .reader_flags(thread)
                    .into_iter()
                    .map(|(_, f)| f)
                    .filter(|f| f.in_mailbox(&b.mailbox))
                    .collect();
                let unread = inside.iter().filter(|f| !f.has_keyword("$seen")).count();
                kept[0] += inside.len() as i64;
                kept[1] += unread as i64;
                kept[2] += i64::from(!inside.is_empty());
                kept[3] += i64::from(unread > 0);
            }
            let now = [
                source.total_emails,
                source.unread_emails,
                source.total_threads,
                source.unread_threads,
            ];
            let taken: CountDelta = std::array::from_fn(|i| (i64::from(now[i]) - kept[i]).max(0));
            let mut add = |mailbox: &str, d: CountDelta| {
                let e = deltas.entry(mailbox.to_owned()).or_default();
                for i in 0..4 {
                    e[i] += d[i];
                }
            };
            let read = b.seen == Some(true);
            if b.hide {
                add(&b.mailbox, taken.map(|n| -n));
                if let Some(target) = &b.target {
                    let unread = if read { 0 } else { 1 };
                    add(
                        target,
                        [taken[0], taken[1] * unread, taken[2], taken[3] * unread],
                    );
                }
            } else if read {
                add(&b.mailbox, [0, -taken[1], 0, -taken[3]]);
            } else if b.seen == Some(false) {
                add(&b.mailbox, [0, taken[0] - taken[1], 0, taken[2] - taken[3]]);
            }
        }
    }

    /// Every cached message of a thread with the Reader's flags, pending
    /// changes left out.
    fn reader_flags(&self, thread: &str) -> Vec<(String, EmailFlags)> {
        self.ok("a conversation", self.reader.conversation(thread))
            .iter()
            .map(|h| (h.row.email_id.as_str().to_owned(), convert::flags_of(h)))
            .collect()
    }

    /// The thread of a message.
    pub fn thread_of(&self, email: &str) -> Option<String> {
        self.ok("a message", self.reader.email(email))
            .map(|d| d.headers.row.thread_id.as_str().to_owned())
    }

    /// The text parts of a cached body, for a render worker.
    pub fn body_text(&self, email: &str) -> Option<BodyText> {
        let view = self.message_body(&ui::EmailId::new(email))?;
        Some(BodyText {
            plain: view.text.map(Into::into),
            html: view.html.map(Into::into),
        })
    }

    /// Conversation rows for server search results, one per thread in the
    /// server's order. The engine caches the rows before it answers.
    pub fn rows_for_emails(&self, ids: &[Id]) -> Vec<ui::ConversationRow> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in ids {
            let Some(detail) = self.ok("a search result", self.reader.email(id.as_str())) else {
                continue;
            };
            let thread = detail.headers.row.thread_id.as_str().to_owned();
            if !seen.insert(thread.clone()) {
                continue;
            }
            let messages = self.ok("a conversation", self.reader.conversation(&thread));
            out.push(self.thread_row(&thread, &detail.headers.row, &messages));
        }
        out
    }

    /// A conversation row for a thread, built from its cached messages,
    /// with `fallback` when the thread has none (it is then a single row).
    fn thread_row(
        &self,
        thread: &str,
        fallback: &mejl_store::MessageRow,
        messages: &[mejl_store::MessageHeaders],
    ) -> ui::ConversationRow {
        let newest = messages.last().map_or(fallback, |m| &m.row);
        let mut people: Vec<ui::Participant> = Vec::new();
        for m in messages.iter().rev() {
            if let Some(p) = m.row.from.first().map(convert::participant)
                && !people
                    .iter()
                    .any(|q| q.email.eq_ignore_ascii_case(&p.email))
            {
                people.push(p);
            }
        }
        let mut row = ui::ConversationRow {
            thread_id: ui::ThreadId::new(thread),
            latest_email_id: ui::EmailId::new(newest.email_id.as_str()),
            from: newest
                .from
                .first()
                .map(convert::participant)
                .unwrap_or_default(),
            participants: people,
            subject: newest.subject.clone(),
            preview: newest.preview.clone(),
            date: newest.received_at,
            message_count: u32::try_from(messages.len().max(1)).unwrap_or(u32::MAX),
            unread: messages.iter().any(|m| !m.row.seen) || (messages.is_empty() && !fallback.seen),
            flagged: messages.iter().any(|m| m.row.flagged),
            has_attachment: messages.iter().any(|m| m.row.has_attachment),
            masked: messages.iter().any(|m| m.row.masked),
        };
        if let Some(s) = self.pending.thread(thread) {
            row.unread = s.after.iter().any(|(_, f)| !f.has_keyword("$seen"));
            row.flagged = s.after.iter().any(|(_, f)| f.has_keyword("$flagged"));
        }
        row
    }

    /// A Reader page, or an empty one when the read fails.
    fn page(
        &self,
        mailbox: &str,
        offset: usize,
        limit: usize,
    ) -> mejl_store::Page<mejl_store::ConversationRow> {
        match self.reader.conversations(mailbox, offset, limit) {
            Ok(p) => p,
            Err(e) => {
                self.ok::<()>("conversations", Err(e));
                mejl_store::Page {
                    rows: Vec::new(),
                    offset,
                    total: 0,
                }
            }
        }
    }

    /// Applies pending flags to a list row in `mailbox`.
    fn overlay_row(&self, mailbox: &str, row: &mut ui::ConversationRow) {
        if let Some((unread, flagged)) = self
            .pending
            .thread(row.thread_id.as_str())
            .and_then(|s| s.row_flags(mailbox))
        {
            row.unread = unread;
            row.flagged = flagged;
        }
        if let Some(b) = self.pending.bulk(mailbox)
            && !b.except.contains(row.thread_id.as_str())
        {
            if let Some(seen) = b.seen {
                row.unread = !seen;
            }
            if let Some(flagged) = b.flagged {
                row.flagged = flagged;
            }
        }
    }

    /// The Reader positions of rows the overlay hides in `mailbox`,
    /// ascending: rows a pending change takes out, and the Reader's own
    /// rows for conversations it brings in, which [`StoreData::added`]
    /// shows instead.
    fn hidden(&self, mailbox: &str) -> Vec<(usize, String)> {
        let entered = self.pending.entered(mailbox);
        let mut out: Vec<(usize, String)> = self
            .pending
            .left(mailbox)
            .into_iter()
            .chain(entered.iter().map(|s| s.thread.as_str()))
            .filter_map(|t| {
                let pos = self.ok(
                    "a conversation position",
                    self.reader.conversation_position(mailbox, t),
                )?;
                Some((pos, t.to_owned()))
            })
            .collect();
        out.sort();
        out
    }

    /// A page of the Reader's rows with the hidden ones left out and
    /// pending flags applied.
    fn base_page(
        &self,
        mb: &str,
        offset: usize,
        limit: usize,
        hidden: &[(usize, String)],
    ) -> Page<ui::ConversationRow> {
        // The Reader position of the first visible row at `offset`.
        let mut start = offset;
        for (p, _) in hidden {
            if *p <= start {
                start += 1;
            } else {
                break;
            }
        }
        let skip: HashSet<&str> = hidden.iter().map(|(_, t)| t.as_str()).collect();
        let page = self.page(mb, start, limit.saturating_add(hidden.len()));
        let raw_total = usize::try_from(page.total).unwrap_or(usize::MAX);
        let items: Vec<ui::ConversationRow> = page
            .rows
            .iter()
            .filter(|r| !skip.contains(r.thread_id.as_str()))
            .take(limit)
            .map(|r| {
                let mut row = convert::conversation_row(r);
                self.overlay_row(mb, &mut row);
                row
            })
            .collect();
        self.check_window(mb, start.saturating_add(limit), raw_total);
        Page {
            offset,
            items,
            total: raw_total.saturating_sub(hidden.len()),
        }
    }

    /// Rows the overlay adds to `mailbox`: conversations a pending change
    /// brings into it (an undo of an archive, a move into the mailbox on
    /// screen). Sorted by place. They come from the overlay whether or not
    /// the store has written the change yet, as the store can write it
    /// between two reads of one page; the Reader's own row, if it has one,
    /// is hidden. Usually empty.
    fn added(&self, mailbox: &str, hidden: &[(usize, String)]) -> Vec<Added> {
        let mut out: Vec<Added> = Vec::new();
        for s in self.pending.entered(mailbox) {
            let messages = self.ok("a conversation", self.reader.conversation(&s.thread));
            let newest = messages
                .iter()
                .filter(|h| {
                    s.after
                        .iter()
                        .any(|(id, f)| id == h.row.email_id.as_str() && f.in_mailbox(mailbox))
                })
                .max_by_key(|h| h.row.received_at);
            let Some(newest) = newest else {
                continue;
            };
            let mut row = self.thread_row(&s.thread, &newest.row, &messages);
            // The list shows a conversation by its newest message in the
            // mailbox, as the Reader would.
            row.latest_email_id = ui::EmailId::new(newest.row.email_id.as_str());
            row.subject = newest.row.subject.clone();
            row.preview = newest.row.preview.clone();
            row.date = newest.row.received_at;
            if let Some(from) = newest.row.from.first() {
                row.from = convert::participant(from);
            }
            self.overlay_row(mailbox, &mut row);
            let at = self.rank(mailbox, row.date, &s.thread);
            let base = at - hidden.iter().filter(|(p, _)| *p < at).count();
            out.push(Added {
                base,
                place: 0,
                row,
            });
        }
        out.sort_by(|a, b| {
            a.base
                .cmp(&b.base)
                .then(b.row.date.cmp(&a.row.date))
                .then(b.row.thread_id.as_str().cmp(a.row.thread_id.as_str()))
        });
        for (k, a) in out.iter_mut().enumerate() {
            a.place = a.base + k;
        }
        out
    }

    /// How many of the Reader's rows in `mailbox` sort before a
    /// conversation whose newest message there arrived `at`: the Reader
    /// lists newest first, then by thread id, descending. Gallops from the
    /// top, as added rows are nearly always recent, then bisects; each probe
    /// is a one-row page read.
    fn rank(&self, mailbox: &str, at: jiff::Timestamp, thread: &str) -> usize {
        let sorts_before = |i: usize| {
            self.page(mailbox, i, 1)
                .rows
                .first()
                .is_some_and(|r| (r.received_at, r.thread_id.as_str()) > (at, thread))
        };
        let mut lo = 0;
        let mut hi = 1;
        while sorts_before(hi - 1) {
            lo = hi;
            hi = hi.saturating_mul(2);
        }
        // Every row before `lo` sorts before; row `hi - 1` does not.
        let mut end = hi - 1;
        while lo < end {
            let mid = lo + (end - lo) / 2;
            if sorts_before(mid) {
                lo = mid + 1;
            } else {
                end = mid;
            }
        }
        lo
    }

    /// Rows still visible after a whole-mailbox move: the excepted ones,
    /// by Reader position.
    fn bulk_rows(&self, mailbox: &str, except: &[&str]) -> Vec<usize> {
        let mut out: Vec<usize> = except
            .iter()
            .filter_map(|t| {
                self.ok(
                    "a conversation position",
                    self.reader.conversation_position(mailbox, t),
                )
            })
            .collect();
        out.sort_unstable();
        out
    }

    /// Asks for the mailbox's window to grow when the UI reads past what is
    /// cached locally and the server has more, or for its first window when
    /// it has none. The first sync only fetches windows for role mailboxes,
    /// so a folder's local rows are only what came in with other mailboxes
    /// or later changes.
    fn check_window(&self, mailbox: &str, end: usize, local_total: usize) {
        let key = mejl_sync::mailbox_query_key(&Id::from(mailbox));
        let more = if end < local_total {
            // The cheap check: a page inside the local rows only needs a
            // window to exist.
            match self.reader.has_query(&key) {
                Ok(has) => !has,
                Err(e) => {
                    self.ok::<()>("a query window", Err(e));
                    false
                }
            }
        } else {
            match self.reader.query_page(&key, 0, 0) {
                Ok(page) => page.is_none_or(|q| q.total.is_some_and(|t| t > q.cached as u64)),
                Err(e) => {
                    self.ok::<()>("a query window", Err(e));
                    false
                }
            }
        };
        if more {
            *self.load_more.borrow_mut() = Some(mailbox.to_owned());
        }
    }
}

impl MailData for StoreData {
    fn account(&self) -> ui::AccountView {
        let session = self.ok("the session", self.reader.session());
        let identities = self.ok("identities", self.reader.identities());
        let email = session
            .as_ref()
            .map(|s| s.username.clone())
            .filter(|u| !u.is_empty())
            .or_else(|| identities.first().and_then(|i| i.email.clone()))
            .unwrap_or_else(|| self.account_fallback.clone());
        let name = identities
            .iter()
            .find(|i| i.email.as_deref() == Some(email.as_str()))
            .or(identities.first())
            .and_then(|i| i.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.account_fallback.clone());
        ui::AccountView { name, email }
    }

    fn mailboxes(&self) -> Vec<ui::MailboxView> {
        if self.pending.is_empty() {
            return convert::mailbox_views(&self.ok("mailboxes", self.reader.mailboxes()));
        }
        // The counts are measured against the Reader's flags, so both must
        // come from the same moment. The store can write between two reads;
        // read the flags on both sides of the counts and keep a round in
        // which they agree.
        let threads = self.pending.threads();
        let flags = || -> HashMap<&str, Vec<(String, EmailFlags)>> {
            threads.iter().map(|t| (*t, self.reader_flags(t))).collect()
        };
        let mut seen = flags();
        let mut rows = self.ok("mailboxes", self.reader.mailboxes());
        for _ in 0..3 {
            let after = flags();
            if after == seen {
                break;
            }
            seen = after;
            rows = self.ok("mailboxes", self.reader.mailboxes());
        }
        let mut views = convert::mailbox_views(&rows);
        {
            let mut deltas = self
                .pending
                .count_deltas(|thread| seen.get(thread).cloned().unwrap_or_default());
            self.bulk_deltas(&views, &mut deltas);
            let adjust = |n: u32, d: i64| -> u32 {
                u32::try_from((i64::from(n) + d).max(0)).unwrap_or(u32::MAX)
            };
            for v in &mut views {
                if let Some(d) = deltas.get(v.id.as_str()) {
                    v.total_emails = adjust(v.total_emails, d[0]);
                    v.unread_emails = adjust(v.unread_emails, d[1]);
                    v.total_threads = adjust(v.total_threads, d[2]);
                    v.unread_threads = adjust(v.unread_threads, d[3]);
                }
            }
        }
        views
    }

    fn conversation_page(
        &self,
        mailbox: &ui::MailboxId,
        offset: usize,
        limit: usize,
    ) -> Page<ui::ConversationRow> {
        let mb = mailbox.as_str();
        if let Some(bulk) = self.pending.bulk(mb).filter(|b| b.hide) {
            let except: Vec<&str> = bulk.except.iter().map(String::as_str).collect();
            let positions = self.bulk_rows(mb, &except);
            let items = positions
                .iter()
                .skip(offset)
                .take(limit)
                .filter_map(|p| {
                    self.page(mb, *p, 1)
                        .rows
                        .first()
                        .map(convert::conversation_row)
                })
                .collect();
            return Page {
                offset,
                items,
                total: positions.len(),
            };
        }
        let hidden = self.hidden(mb);
        let added = self.added(mb, &hidden);
        if added.is_empty() {
            return self.base_page(mb, offset, limit, &hidden);
        }
        // Added rows sit at their places; the Reader's rows fill the rest.
        let before = added.iter().filter(|a| a.place < offset).count();
        let base = self.base_page(mb, offset - before, limit, &hidden);
        let total = base.total + added.len();
        // The Reader's own rows for these are hidden, but the store may
        // have written one between the reads above; it still shows once.
        let mut rows = base
            .items
            .into_iter()
            .filter(|r| added.iter().all(|a| a.row.thread_id != r.thread_id));
        let mut items = Vec::with_capacity(limit);
        for i in offset..total.min(offset.saturating_add(limit)) {
            match added.iter().find(|a| a.place == i) {
                Some(a) => items.push(a.row.clone()),
                None => match rows.next() {
                    Some(r) => items.push(r),
                    None => break,
                },
            }
        }
        Page {
            offset,
            items,
            total,
        }
    }

    fn conversation_index(&self, mailbox: &ui::MailboxId, thread: &ui::ThreadId) -> Option<usize> {
        let mb = mailbox.as_str();
        if let Some(bulk) = self.pending.bulk(mb).filter(|b| b.hide) {
            if !bulk.except.contains(thread.as_str()) {
                return None;
            }
            let except: Vec<&str> = bulk.except.iter().map(String::as_str).collect();
            let pos = self.ok(
                "a conversation position",
                self.reader.conversation_position(mb, thread.as_str()),
            )?;
            return self.bulk_rows(mb, &except).iter().position(|p| *p == pos);
        }
        let hidden = self.hidden(mb);
        let added = self.added(mb, &hidden);
        if let Some(a) = added.iter().find(|a| &a.row.thread_id == thread) {
            return Some(a.place);
        }
        if hidden.iter().any(|(_, t)| t == thread.as_str()) {
            return None;
        }
        let pos = self.ok(
            "a conversation position",
            self.reader.conversation_position(mb, thread.as_str()),
        )?;
        let base = pos - hidden.iter().filter(|(p, _)| *p < pos).count();
        Some(base + added.iter().filter(|a| a.base <= base).count())
    }

    fn conversation(&self, thread: &ui::ThreadId) -> Option<ui::ConversationView> {
        let messages = self.ok("a conversation", self.reader.conversation(thread.as_str()));
        if messages.is_empty() {
            return None;
        }
        // The newest subject, as the list shows it ("Re: ..." once
        // there are replies).
        let subject = messages
            .last()
            .map(|m| m.row.subject.clone())
            .unwrap_or_default();
        let messages = messages
            .iter()
            .map(|h| {
                let mut m = convert::message_summary(h);
                if let Some(f) = self.pending.flags(h.row.email_id.as_str()) {
                    convert::apply_flags(&mut m, f);
                }
                m
            })
            .collect();
        Some(ui::ConversationView {
            thread_id: thread.clone(),
            subject,
            messages,
        })
    }

    fn message_body(&self, email: &ui::EmailId) -> Option<ui::BodyView> {
        let detail = self.ok("a message body", self.reader.email(email.as_str()))?;
        convert::body_view(email.as_str(), &detail)
    }

    fn identities(&self) -> Vec<ui::IdentityView> {
        self.ok("identities", self.reader.identities())
            .iter()
            .map(convert::identity_view)
            .collect()
    }

    fn masked_emails(&self) -> Vec<ui::MaskedEmailView> {
        self.ok("Masked Email addresses", self.reader.masked_emails())
            .iter()
            .map(|m| {
                let mut v = convert::masked_view(m);
                let (state, description) = self.pending.masked(&v.id);
                if let Some(s) = state {
                    v.state = s;
                }
                if let Some(d) = description {
                    v.description = d.to_owned();
                }
                v
            })
            .collect()
    }

    fn masked_for_address(&self, address: &str) -> Option<ui::MaskedEmailView> {
        self.ok(
            "a Masked Email address",
            self.reader.masked_email_by_address(address),
        )
        .map(|m| convert::masked_view(&m))
    }

    fn contacts_prefix(&self, prefix: &str, limit: usize) -> Vec<ui::ContactView> {
        self.ok(
            "address suggestions",
            self.reader.address_suggestions(prefix, limit),
        )
        .into_iter()
        .map(|s| ui::ContactView {
            id: s
                .contact_id
                .map(|c| c.as_str().to_owned())
                .unwrap_or_else(|| s.email.clone()),
            name: s.name.unwrap_or_default(),
            emails: vec![s.email],
        })
        .collect()
    }

    fn search_local(&self, query: &str, limit: usize) -> ui::SearchResults {
        let options = SearchOptions {
            limit: limit.saturating_mul(HITS_PER_ROW).max(1),
            time_zone: self.time_zone.clone(),
            ..SearchOptions::default()
        };
        let results = self.ok("search results", self.reader.search(query, &options));
        let roles = self.roles();
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for h in &results.hits {
            if hits.len() >= limit {
                break;
            }
            let thread = h.row.thread_id.as_str();
            if !seen.insert(thread.to_owned()) {
                continue;
            }
            let messages = self.ok("a conversation", self.reader.conversation(thread));
            let row = self.thread_row(thread, &h.row, &messages);
            // The matching message's mailbox, preferring the Inbox, so an
            // archive from the results moves it out of where it shows.
            let mailbox = messages
                .iter()
                .find(|m| m.row.email_id == h.row.email_id)
                .and_then(|m| {
                    let inbox = roles.inbox.as_ref().filter(|i| m.mailbox_ids.contains(*i));
                    inbox.or_else(|| m.mailbox_ids.iter().next()).cloned()
                })
                .map(|m| ui::MailboxId::new(m.as_str()));
            hits.push(ui::SearchHit {
                row,
                mailbox,
                subject: convert::highlight(&h.subject),
                snippet: convert::highlight(&h.snippet),
            });
        }
        ui::SearchResults {
            hits,
            issues: results
                .issues
                .iter()
                .map(|i| ui::SearchIssue {
                    term: i.term.clone(),
                    kind: convert::issue_kind(&i.kind),
                })
                .collect(),
            full_text: results.full_text,
        }
    }

    fn search_filter_preview(&self, query: &str) -> Option<String> {
        // The same store rows the engine resolves `in:` against (not the
        // overlaid views), and no mailbox limit, as the `Cmd::ServerSearch`
        // arm in `app/mod.rs` sends none. The engine and this store both
        // use the system time zone (`tui.rs`), so dates match too.
        let rows = self.ok("mailboxes", self.reader.mailboxes());
        let (filter, _) = mejl_sync::filter_preview(query, None, &self.time_zone, &rows);
        Some(filter.unwrap_or_else(|| "null".to_owned()))
    }

    fn saved_searches(&self) -> Vec<ui::SavedSearchView> {
        self.saved
            .iter()
            .map(|s| ui::SavedSearchView {
                id: s.id.clone(),
                name: s.name.clone(),
                query: s.query.clone(),
                unread: None,
            })
            .collect()
    }

    fn rules(&self) -> Vec<ui::RuleJson> {
        self.ok("rules", self.reader.rules())
            .into_iter()
            .map(|r| ui::RuleJson {
                id: r.id,
                json: r.rule_json,
            })
            .collect()
    }

    fn drafts(&self) -> Vec<ui::DraftView> {
        self.ok("drafts", self.reader.drafts())
            .iter()
            .map(convert::draft_view)
            .collect()
    }

    fn rule_renders(&self, limit: usize) -> Vec<ui::RuleRenderView> {
        self.ok("rule copies", self.reader.rule_renders(limit))
            .iter()
            .map(convert::rule_render_view)
            .collect()
    }

    fn dry_run_messages(&self, limit: usize) -> Vec<ui::DryRunMessage> {
        self.ok("recent messages", self.reader.recent_messages(limit))
            .iter()
            .map(convert::dry_run_message)
            .collect()
    }
}

/// Group-by helper used by tests and the mutation planner: emails of the
/// given ids keyed by thread, in first-seen order.
pub fn by_thread(data: &StoreData, emails: &[ui::EmailId]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in emails {
        if let Some(t) = data.thread_of(e.as_str()) {
            out.entry(t).or_default().push(e.as_str().to_owned());
        }
    }
    out
}
