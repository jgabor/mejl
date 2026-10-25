//! An in-memory [`MailData`] for tests, snapshots and demos.
//!
//! [`MemoryData`] holds emails, mailboxes and bodies in plain collections
//! and builds view rows on demand. It also applies [`Mutation`]s, so tests
//! can play the shell's part of the optimistic contract: apply the `Cmd`,
//! feed back `Msg::DataChanged`, check the new frame.
//!
//! [`MemoryData::synthetic`] generates rows from their index instead of
//! storing them, so a 100k conversation mailbox costs nothing to build and
//! read counters show exactly what the UI asked for.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use jiff::{SignedDuration, Timestamp};

use super::{
    AccountView, AttachmentView, BodyView, ContactView, ConversationRow, ConversationView,
    DraftView, DryRunMessage, EmailId, Highlight, IdentityView, MailData, MailboxId, MailboxRole,
    MailboxView, MaskedEmailView, MessageSummary, Page, Participant, RuleJson, RuleRenderView,
    SavedSearchView, SearchHit, SearchIssue, SearchIssueKind, SearchResults, ThreadId,
};
use crate::event::{DataChange, Keyword, Mutation, Target, TargetIds};

/// Counters of what the UI read, for tests that check virtualisation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Calls to `conversation_page`.
    pub page_calls: usize,
    /// Rows returned by `conversation_page`, summed.
    pub rows_returned: usize,
    /// Largest `limit` passed to `conversation_page`.
    pub max_limit: usize,
    /// Calls to `conversation`.
    pub conversation_calls: usize,
    /// Calls to `message_body`.
    pub body_calls: usize,
    /// Calls to `mailboxes`.
    pub mailbox_calls: usize,
}

#[derive(Debug, Default)]
struct Counters {
    page_calls: AtomicUsize,
    rows_returned: AtomicUsize,
    max_limit: AtomicUsize,
    conversation_calls: AtomicUsize,
    body_calls: AtomicUsize,
    mailbox_calls: AtomicUsize,
}

/// A message to add to a [`MemoryData`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEmail {
    /// Message id.
    pub id: EmailId,
    /// Conversation id.
    pub thread: ThreadId,
    /// Sender.
    pub from: Participant,
    /// To recipients.
    pub to: Vec<Participant>,
    /// Cc recipients.
    pub cc: Vec<Participant>,
    /// Subject.
    pub subject: String,
    /// Preview text.
    pub preview: String,
    /// Received date.
    pub date: Timestamp,
    /// Mailboxes it is in.
    pub mailboxes: Vec<MailboxId>,
    /// Not yet read.
    pub unread: bool,
    /// Pinned.
    pub flagged: bool,
    /// Has attachments.
    pub has_attachment: bool,
    /// Reply-To addresses.
    pub reply_to: Vec<Participant>,
    /// Message-ID without angle brackets.
    pub message_id: Option<String>,
    /// References ids, oldest first.
    pub references: Vec<String>,
    /// Size in bytes.
    pub size: u64,
}

impl NewEmail {
    /// A read, unpinned message in one mailbox with no recipients.
    pub fn new(
        id: impl Into<EmailId>,
        thread: impl Into<ThreadId>,
        from: Participant,
        subject: impl Into<String>,
        date: Timestamp,
        mailbox: impl Into<MailboxId>,
    ) -> Self {
        NewEmail {
            id: id.into(),
            thread: thread.into(),
            from,
            to: Vec::new(),
            cc: Vec::new(),
            subject: subject.into(),
            preview: String::new(),
            date,
            mailboxes: vec![mailbox.into()],
            unread: false,
            flagged: false,
            has_attachment: false,
            reply_to: Vec::new(),
            message_id: None,
            references: Vec::new(),
            size: 4_096,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EmailState {
    mailboxes: BTreeSet<MailboxId>,
    unread: bool,
    flagged: bool,
    muted: bool,
}

#[derive(Clone, Debug)]
struct Email {
    data: NewEmail,
    state: EmailState,
}

#[derive(Clone, Debug)]
struct Synthetic {
    count: usize,
    now: Timestamp,
    mailbox: MailboxId,
}

type Snapshot = Vec<(EmailId, EmailState)>;

/// An in-memory mail store implementing [`MailData`].
#[derive(Debug, Default)]
pub struct MemoryData {
    account: Option<AccountView>,
    mailboxes: Vec<MailboxView>,
    emails: Vec<Email>,
    by_id: HashMap<EmailId, usize>,
    threads: HashMap<ThreadId, Vec<usize>>,
    lists: HashMap<MailboxId, Vec<ThreadId>>,
    bodies: HashMap<EmailId, BodyView>,
    identities: Vec<IdentityView>,
    masked: Vec<MaskedEmailView>,
    contacts: Vec<ContactView>,
    saved: Vec<SavedSearchView>,
    rules: Vec<RuleJson>,
    drafts: Vec<DraftView>,
    renders: Vec<RuleRenderView>,
    filter_previews: HashMap<String, String>,
    synthetic: Option<Synthetic>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    counters: Counters,
}

const SYN_NAMES: [&str; 12] = [
    "Juniper Tran",
    "Owen Castillo",
    "Lena Okafor",
    "Priya Raman",
    "Marco Bellini",
    "Dana Whitfield",
    "Sam Ito",
    "Ravi Menon",
    "Grace Lindqvist",
    "Felix Moreau",
    "Noor Haddad",
    "Theo Park",
];

const SYN_TOPICS: [&str; 10] = [
    "Garden club plot rota",
    "Draft budget for the hall repairs",
    "Thursday standup notes",
    "Trip to the coast",
    "Recipe for the plum cake",
    "Board game night",
    "Moving the book club",
    "Spare room this weekend",
    "Translation review",
    "Running club route",
];

impl MemoryData {
    /// An empty store for `account`.
    pub fn new(account: AccountView) -> Self {
        MemoryData {
            account: Some(account),
            ..MemoryData::default()
        }
    }

    /// The fictional demo account that matches docs/mockups.html: Mira
    /// Holt's mailbox with 214 inbox conversations, 12 unread, as seen at
    /// `now` (the mock-ups use Friday 2026-10-23 14:02:31 UTC).
    pub fn demo(now: Timestamp) -> Self {
        super::fixture::demo(now)
    }

    /// A single Inbox with `count` generated conversations, one every six
    /// minutes back from `now`. Rows are generated from their index, so
    /// building and paging cost the same at any size. Mutations are not
    /// applied in this mode.
    pub fn synthetic(count: usize, now: Timestamp) -> Self {
        let mut data = MemoryData::new(AccountView {
            name: "Mira Holt".into(),
            email: "mira@example.org".into(),
        });
        let inbox = MailboxId::from("syn-inbox");
        data.add_mailbox(inbox.clone(), None, "Inbox", Some(MailboxRole::Inbox), 0);
        data.synthetic = Some(Synthetic {
            count,
            now,
            mailbox: inbox,
        });
        data
    }

    /// Adds a mailbox. Counts are computed from the messages.
    pub fn add_mailbox(
        &mut self,
        id: impl Into<MailboxId>,
        parent: Option<MailboxId>,
        name: impl Into<String>,
        role: Option<MailboxRole>,
        sort_order: u32,
    ) {
        let parent_depth = parent
            .as_ref()
            .and_then(|p| self.mailboxes.iter().find(|m| &m.id == p))
            .map(|m| m.depth + 1)
            .unwrap_or(0);
        self.mailboxes.push(MailboxView {
            id: id.into(),
            parent,
            name: name.into(),
            role,
            depth: parent_depth,
            sort_order,
            total_threads: 0,
            unread_threads: 0,
            total_emails: 0,
            unread_emails: 0,
        });
    }

    /// Adds a message.
    pub fn add_email(&mut self, email: NewEmail) {
        self.push_email(email);
        self.reindex();
    }

    /// Adds a message without rebuilding the indexes; the caller runs
    /// [`MemoryData::reindex`] once after a batch.
    pub(crate) fn push_email(&mut self, email: NewEmail) {
        let state = EmailState {
            mailboxes: email.mailboxes.iter().cloned().collect(),
            unread: email.unread,
            flagged: email.flagged,
            muted: false,
        };
        let index = self.emails.len();
        self.by_id.insert(email.id.clone(), index);
        let thread = self.threads.entry(email.thread.clone()).or_default();
        thread.push(index);
        self.emails.push(Email { data: email, state });
    }

    /// Stores a body, making it "cached".
    pub fn add_body(&mut self, body: BodyView) {
        self.bodies.insert(body.email_id.clone(), body);
    }

    /// Removes a cached body, so the UI has to request it.
    pub fn remove_body(&mut self, email: &EmailId) -> Option<BodyView> {
        self.bodies.remove(email)
    }

    /// Adds a sending identity.
    pub fn add_identity(&mut self, identity: IdentityView) {
        self.identities.push(identity);
    }

    /// Adds a Masked Email address.
    pub fn add_masked(&mut self, masked: MaskedEmailView) {
        self.masked.push(masked);
    }

    /// Adds a contact.
    pub fn add_contact(&mut self, contact: ContactView) {
        self.contacts.push(contact);
    }

    /// Adds a saved search.
    pub fn add_saved_search(&mut self, saved: SavedSearchView) {
        self.saved.push(saved);
    }

    /// Adds a rule, or replaces the rule with the same id.
    pub fn add_rule(&mut self, rule: RuleJson) {
        match self.rules.iter_mut().find(|r| r.id == rule.id) {
            Some(r) => *r = rule,
            None => self.rules.push(rule),
        }
    }

    /// Removes a rule. Returns true when it existed.
    pub fn remove_rule(&mut self, id: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        self.rules.len() != before
    }

    /// Puts the rules in the order of `ids`; unknown ids are ignored and
    /// rules not named keep their place at the end.
    pub fn reorder_rules(&mut self, ids: &[String]) {
        let mut out = Vec::with_capacity(self.rules.len());
        for id in ids {
            if let Some(i) = self.rules.iter().position(|r| &r.id == id) {
                out.push(self.rules.remove(i));
            }
        }
        out.append(&mut self.rules);
        self.rules = out;
    }

    /// Records a copied rule block, newest first in [`MailData::rule_renders`].
    pub fn add_rule_render(&mut self, render: RuleRenderView) {
        self.renders.push(render);
    }

    /// Adds or replaces a draft.
    pub fn add_draft(&mut self, draft: DraftView) {
        self.drafts
            .retain(|d| d.message.draft_id != draft.message.draft_id);
        self.drafts.push(draft);
    }

    /// Removes a draft. Returns true when it existed.
    pub fn remove_draft(&mut self, id: &super::DraftId) -> bool {
        let before = self.drafts.len();
        self.drafts.retain(|d| &d.message.draft_id != id);
        self.drafts.len() != before
    }

    /// Replaces a Masked Email address with the same id, or adds it.
    pub fn set_masked(&mut self, masked: MaskedEmailView) {
        match self.masked.iter_mut().find(|m| m.id == masked.id) {
            Some(m) => *m = masked,
            None => self.masked.push(masked),
        }
    }

    /// Adds a saved search under a generated id.
    pub fn save_search(&mut self, name: &str, query: &str) {
        let id = format!("s{}", self.saved.len() + 1);
        self.saved.push(SavedSearchView {
            id,
            name: name.to_owned(),
            query: query.to_owned(),
            unread: None,
        });
    }

    /// Sets the JMAP filter [`MailData::search_filter_preview`] reports
    /// for `query`. The memory store has no server search of its own, so
    /// it only knows the filters it is told; other queries get `None`.
    pub fn set_filter_preview(&mut self, query: &str, filter: &str) {
        self.filter_previews
            .insert(query.to_owned(), filter.to_owned());
    }

    pub(crate) fn set_has_attachment(&mut self, email: &EmailId) {
        if let Some(&i) = self.by_id.get(email) {
            self.emails[i].data.has_attachment = true;
        }
    }

    /// The id of the mailbox with `role`.
    pub fn mailbox_for_role(&self, role: MailboxRole) -> Option<MailboxId> {
        self.mailboxes
            .iter()
            .find(|m| m.role == Some(role))
            .map(|m| m.id.clone())
    }

    /// Read counters since creation or the last [`MemoryData::reset_stats`].
    pub fn stats(&self) -> ReadStats {
        let c = &self.counters;
        ReadStats {
            page_calls: c.page_calls.load(Ordering::Relaxed),
            rows_returned: c.rows_returned.load(Ordering::Relaxed),
            max_limit: c.max_limit.load(Ordering::Relaxed),
            conversation_calls: c.conversation_calls.load(Ordering::Relaxed),
            body_calls: c.body_calls.load(Ordering::Relaxed),
            mailbox_calls: c.mailbox_calls.load(Ordering::Relaxed),
        }
    }

    /// Zeroes the read counters.
    pub fn reset_stats(&self) {
        let c = &self.counters;
        for counter in [
            &c.page_calls,
            &c.rows_returned,
            &c.max_limit,
            &c.conversation_calls,
            &c.body_calls,
            &c.mailbox_calls,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }

    /// Applies a mutation the way the shell's local store would, and
    /// returns what changed. Undo and redo restore message state captured
    /// before each change. Permanent deletion cannot be undone.
    pub fn apply(&mut self, mutation: &Mutation) -> DataChange {
        if self.synthetic.is_some() {
            return DataChange::default();
        }
        match mutation {
            Mutation::Undo => self.restore(true),
            Mutation::Redo => self.restore(false),
            Mutation::DeletePermanently(target) => {
                let indices = self.resolve(target, false);
                let threads = self.threads_of(&indices);
                let doomed: HashSet<usize> = indices.into_iter().collect();
                let kept: Vec<Email> = std::mem::take(&mut self.emails)
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| !doomed.contains(i))
                    .map(|(_, e)| e)
                    .collect();
                self.rebuild(kept);
                DataChange {
                    mailboxes: true,
                    threads,
                    ..DataChange::default()
                }
            }
            other => {
                let (target, keyword_change) = match other {
                    Mutation::SetKeyword { target, .. } => (target, true),
                    Mutation::SetMailbox { target, .. } => (target, true),
                    Mutation::Archive(t) | Mutation::Trash(t) | Mutation::ReportSpam(t) => {
                        (t, false)
                    }
                    Mutation::MoveTo { target, .. } => (target, false),
                    Mutation::Undo | Mutation::Redo | Mutation::DeletePermanently(_) => {
                        return DataChange::default();
                    }
                };
                let indices = self.resolve(target, keyword_change);
                if indices.is_empty() {
                    return DataChange::default();
                }
                let snapshot: Snapshot = indices
                    .iter()
                    .map(|&i| (self.emails[i].data.id.clone(), self.emails[i].state.clone()))
                    .collect();
                for &i in &indices {
                    self.change(i, other, target.from_mailbox.as_ref());
                }
                self.undo.push(snapshot);
                self.redo.clear();
                let threads = self.threads_of(&indices);
                self.reindex();
                DataChange {
                    mailboxes: true,
                    threads,
                    emails: indices
                        .iter()
                        .map(|&i| self.emails[i].data.id.clone())
                        .collect(),
                    ..DataChange::default()
                }
            }
        }
    }

    fn change(&mut self, index: usize, mutation: &Mutation, from: Option<&MailboxId>) {
        let role_box = |role| self.mailbox_for_role(role);
        let archive = role_box(MailboxRole::Archive);
        let trash = role_box(MailboxRole::Trash);
        let junk = role_box(MailboxRole::Junk);
        let inbox = role_box(MailboxRole::Inbox);
        let state = &mut self.emails[index].state;
        let leave = |state: &mut EmailState| match from.or(inbox.as_ref()) {
            Some(m) => {
                state.mailboxes.remove(m);
            }
            None => state.mailboxes.clear(),
        };
        match mutation {
            Mutation::Archive(_) => {
                leave(state);
                if let Some(a) = archive {
                    state.mailboxes.insert(a);
                }
            }
            Mutation::Trash(_) => {
                if let Some(t) = trash {
                    state.mailboxes = BTreeSet::from([t]);
                }
            }
            Mutation::ReportSpam(_) => {
                if let Some(j) = junk {
                    state.mailboxes = BTreeSet::from([j]);
                }
            }
            Mutation::MoveTo { mailbox, .. } => {
                leave(state);
                state.mailboxes.insert(mailbox.clone());
            }
            Mutation::SetKeyword { keyword, value, .. } => match keyword {
                Keyword::Seen => state.unread = !value,
                Keyword::Flagged => state.flagged = *value,
                Keyword::Muted => state.muted = *value,
            },
            Mutation::SetMailbox {
                mailbox, member, ..
            } => {
                if *member {
                    state.mailboxes.insert(mailbox.clone());
                } else {
                    state.mailboxes.remove(mailbox);
                }
            }
            Mutation::DeletePermanently(_) | Mutation::Undo | Mutation::Redo => {}
        }
    }

    fn restore(&mut self, undo: bool) -> DataChange {
        let popped = if undo {
            self.undo.pop()
        } else {
            self.redo.pop()
        };
        let Some(snapshot) = popped else {
            return DataChange::default();
        };
        let mut inverse = Snapshot::new();
        let mut indices = Vec::new();
        for (id, state) in snapshot {
            if let Some(&i) = self.by_id.get(&id) {
                inverse.push((id, self.emails[i].state.clone()));
                self.emails[i].state = state;
                indices.push(i);
            }
        }
        if undo {
            self.redo.push(inverse);
        } else {
            self.undo.push(inverse);
        }
        let threads = self.threads_of(&indices);
        self.reindex();
        DataChange {
            mailboxes: true,
            threads,
            ..DataChange::default()
        }
    }

    fn rebuild(&mut self, emails: Vec<Email>) {
        self.by_id.clear();
        self.threads.clear();
        for (i, e) in emails.iter().enumerate() {
            self.by_id.insert(e.data.id.clone(), i);
            self.threads
                .entry(e.data.thread.clone())
                .or_default()
                .push(i);
        }
        self.emails = emails;
        self.reindex();
    }

    fn threads_of(&self, indices: &[usize]) -> Vec<ThreadId> {
        let mut seen = HashSet::new();
        indices
            .iter()
            .map(|&i| self.emails[i].data.thread.clone())
            .filter(|t| seen.insert(t.clone()))
            .collect()
    }

    /// Message indices a target refers to. Moves act on the messages of a
    /// conversation that are in the mailbox being viewed, as Fastmail does,
    /// so a sent copy stays in Sent. Keyword changes act on every message.
    fn resolve(&self, target: &Target, whole_threads: bool) -> Vec<usize> {
        let in_scope = |i: usize| {
            whole_threads
                || target
                    .from_mailbox
                    .as_ref()
                    .is_none_or(|m| self.emails[i].state.mailboxes.contains(m))
        };
        let of_threads = |threads: &mut dyn Iterator<Item = &ThreadId>| -> Vec<usize> {
            threads
                .filter_map(|t| self.threads.get(t))
                .flatten()
                .copied()
                .filter(|&i| in_scope(i))
                .collect()
        };
        match &target.ids {
            TargetIds::Threads(threads) => of_threads(&mut threads.iter()),
            TargetIds::Emails(ids) => ids
                .iter()
                .filter_map(|id| self.by_id.get(id).copied())
                .collect(),
            TargetIds::AllInMailbox { mailbox, except } => {
                let except: HashSet<&ThreadId> = except.iter().collect();
                let threads: Vec<ThreadId> = self
                    .lists
                    .get(mailbox)
                    .map(|l| l.iter().filter(|t| !except.contains(t)).cloned().collect())
                    .unwrap_or_default();
                of_threads(&mut threads.iter())
            }
        }
    }

    /// Rebuilds the per-mailbox conversation order and the counts.
    pub(crate) fn reindex(&mut self) {
        for list in self.threads.values_mut() {
            let emails = &self.emails;
            list.sort_by_key(|&i| (emails[i].data.date, i));
        }
        let mut lists: HashMap<MailboxId, Vec<(Timestamp, ThreadId)>> = HashMap::new();
        for (thread, indices) in &self.threads {
            let mut newest: HashMap<&MailboxId, Timestamp> = HashMap::new();
            for &i in indices {
                let e = &self.emails[i];
                for m in &e.state.mailboxes {
                    let entry = newest.entry(m).or_insert(e.data.date);
                    *entry = (*entry).max(e.data.date);
                }
            }
            for (m, date) in newest {
                lists
                    .entry(m.clone())
                    .or_default()
                    .push((date, thread.clone()));
            }
        }
        self.lists = lists
            .into_iter()
            .map(|(m, mut v)| {
                v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                (m, v.into_iter().map(|(_, t)| t).collect())
            })
            .collect();
        for mb in &mut self.mailboxes {
            let list = self.lists.get(&mb.id);
            mb.total_threads = list.map_or(0, |l| l.len() as u32);
            mb.unread_threads = list.map_or(0, |l| {
                l.iter()
                    .filter(|t| {
                        self.threads.get(*t).is_some_and(|ix| {
                            ix.iter().any(|&i| {
                                let s = &self.emails[i].state;
                                s.unread && s.mailboxes.contains(&mb.id)
                            })
                        })
                    })
                    .count() as u32
            });
            let in_box = self
                .emails
                .iter()
                .filter(|e| e.state.mailboxes.contains(&mb.id));
            let (total, unread) = in_box.fold((0u32, 0u32), |(t, u), e| {
                (t + 1, u + u32::from(e.state.unread))
            });
            mb.total_emails = total;
            mb.unread_emails = unread;
        }
    }

    fn is_masked(&self, p: &Participant) -> bool {
        self.masked
            .iter()
            .any(|m| m.email.eq_ignore_ascii_case(&p.email))
    }

    fn row(&self, mailbox: &MailboxId, thread: &ThreadId) -> Option<ConversationRow> {
        let indices = self.threads.get(thread)?;
        let all: Vec<&Email> = indices.iter().map(|&i| &self.emails[i]).collect();
        let latest = all
            .iter()
            .filter(|e| e.state.mailboxes.contains(mailbox))
            .max_by_key(|e| e.data.date)
            .or_else(|| all.iter().max_by_key(|e| e.data.date))?;
        let mut participants: Vec<Participant> = Vec::new();
        for e in all.iter().rev() {
            if !participants.iter().any(|p| p.email == e.data.from.email) {
                participants.push(e.data.from.clone());
            }
        }
        Some(ConversationRow {
            thread_id: thread.clone(),
            latest_email_id: latest.data.id.clone(),
            from: latest.data.from.clone(),
            participants,
            subject: latest.data.subject.clone(),
            preview: latest.data.preview.clone(),
            date: latest.data.date,
            message_count: all.len() as u32,
            unread: all
                .iter()
                .any(|e| e.state.unread && e.state.mailboxes.contains(mailbox)),
            flagged: all.iter().any(|e| e.state.flagged),
            has_attachment: all.iter().any(|e| e.data.has_attachment),
            masked: all.iter().any(|e| {
                e.data
                    .to
                    .iter()
                    .chain(&e.data.cc)
                    .any(|p| self.is_masked(p))
            }),
        })
    }

    fn synthetic_row(syn: &Synthetic, index: usize) -> ConversationRow {
        let name = SYN_NAMES[index % SYN_NAMES.len()];
        let topic = SYN_TOPICS[(index / SYN_NAMES.len()) % SYN_TOPICS.len()];
        let minutes = i64::try_from(index).unwrap_or(i64::MAX).saturating_mul(6);
        let date = syn
            .now
            .checked_sub(SignedDuration::from_mins(minutes))
            .unwrap_or(syn.now);
        let email = format!(
            "{}@example.net",
            name.to_ascii_lowercase().replace(' ', ".")
        );
        ConversationRow {
            thread_id: ThreadId(format!("s{index}")),
            latest_email_id: EmailId(format!("se{index}")),
            from: Participant::new(name, email),
            participants: Vec::new(),
            subject: format!("{topic} {index}"),
            preview: format!("Message number {index} about {}", topic.to_lowercase()),
            date,
            message_count: 1 + (index % 4) as u32,
            unread: index.is_multiple_of(7),
            flagged: index.is_multiple_of(23),
            has_attachment: index.is_multiple_of(13),
            masked: index.is_multiple_of(11),
        }
    }

    fn synthetic_index(syn: &Synthetic, thread: &ThreadId) -> Option<usize> {
        let index: usize = thread.as_str().strip_prefix('s')?.parse().ok()?;
        (index < syn.count).then_some(index)
    }
}

impl MailData for MemoryData {
    fn account(&self) -> AccountView {
        self.account.clone().unwrap_or(AccountView {
            name: String::new(),
            email: String::new(),
        })
    }

    fn mailboxes(&self) -> Vec<MailboxView> {
        self.counters.mailbox_calls.fetch_add(1, Ordering::Relaxed);
        let mut boxes = self.mailboxes.clone();
        if let Some(syn) = &self.synthetic {
            for mb in &mut boxes {
                if mb.id == syn.mailbox {
                    let count = u32::try_from(syn.count).unwrap_or(u32::MAX);
                    mb.total_threads = count;
                    mb.total_emails = count;
                    mb.unread_threads = count.div_ceil(7);
                    mb.unread_emails = count.div_ceil(7);
                }
            }
        }
        boxes
    }

    fn conversation_page(
        &self,
        mailbox: &MailboxId,
        offset: usize,
        limit: usize,
    ) -> Page<ConversationRow> {
        let c = &self.counters;
        c.page_calls.fetch_add(1, Ordering::Relaxed);
        c.max_limit.fetch_max(limit, Ordering::Relaxed);
        let page = if let Some(syn) = self.synthetic.as_ref().filter(|s| &s.mailbox == mailbox) {
            let end = offset.saturating_add(limit).min(syn.count);
            Page {
                offset,
                items: (offset.min(end)..end)
                    .map(|i| Self::synthetic_row(syn, i))
                    .collect(),
                total: syn.count,
            }
        } else {
            let list = self.lists.get(mailbox).map(Vec::as_slice).unwrap_or(&[]);
            let start = offset.min(list.len());
            let end = offset.saturating_add(limit).min(list.len());
            Page {
                offset,
                items: list[start..end]
                    .iter()
                    .filter_map(|t| self.row(mailbox, t))
                    .collect(),
                total: list.len(),
            }
        };
        c.rows_returned
            .fetch_add(page.items.len(), Ordering::Relaxed);
        page
    }

    fn conversation_index(&self, mailbox: &MailboxId, thread: &ThreadId) -> Option<usize> {
        if let Some(syn) = self.synthetic.as_ref().filter(|s| &s.mailbox == mailbox) {
            return Self::synthetic_index(syn, thread);
        }
        self.lists.get(mailbox)?.iter().position(|t| t == thread)
    }

    fn conversation(&self, thread: &ThreadId) -> Option<ConversationView> {
        self.counters
            .conversation_calls
            .fetch_add(1, Ordering::Relaxed);
        if let Some(syn) = &self.synthetic {
            let index = Self::synthetic_index(syn, thread)?;
            let row = Self::synthetic_row(syn, index);
            return Some(ConversationView {
                thread_id: row.thread_id.clone(),
                subject: row.subject.clone(),
                messages: vec![MessageSummary {
                    email_id: row.latest_email_id.clone(),
                    from: row.from.clone(),
                    to: vec![Participant::new("Mira Holt", "mira@example.org")],
                    cc: Vec::new(),
                    date: row.date,
                    preview: row.preview.clone(),
                    unread: row.unread,
                    flagged: row.flagged,
                    has_attachment: row.has_attachment,
                    mailboxes: vec![syn.mailbox.clone()],
                    reply_to: Vec::new(),
                    message_id: None,
                    references: Vec::new(),
                }],
            });
        }
        let indices = self.threads.get(thread)?;
        let messages: Vec<MessageSummary> = indices
            .iter()
            .map(|&i| {
                let e = &self.emails[i];
                MessageSummary {
                    email_id: e.data.id.clone(),
                    from: e.data.from.clone(),
                    to: e.data.to.clone(),
                    cc: e.data.cc.clone(),
                    date: e.data.date,
                    preview: e.data.preview.clone(),
                    unread: e.state.unread,
                    flagged: e.state.flagged,
                    has_attachment: e.data.has_attachment,
                    mailboxes: e.state.mailboxes.iter().cloned().collect(),
                    reply_to: e.data.reply_to.clone(),
                    message_id: e.data.message_id.clone(),
                    references: e.data.references.clone(),
                }
            })
            .collect();
        let subject = messages
            .last()
            .and_then(|m| self.by_id.get(&m.email_id))
            .map(|&i| self.emails[i].data.subject.clone())
            .unwrap_or_default();
        Some(ConversationView {
            thread_id: thread.clone(),
            subject,
            messages,
        })
    }

    fn message_body(&self, email: &EmailId) -> Option<BodyView> {
        self.counters.body_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(body) = self.bodies.get(email) {
            return Some(body.clone());
        }
        let syn = self.synthetic.as_ref()?;
        let index: usize = email.as_str().strip_prefix("se")?.parse().ok()?;
        if index >= syn.count {
            return None;
        }
        let row = Self::synthetic_row(syn, index);
        let attachments = if row.has_attachment {
            vec![AttachmentView {
                blob_id: format!("b{index}").into(),
                name: format!("notes-{index}.pdf"),
                mime_type: "application/pdf".into(),
                size: 48_000,
            }]
        } else {
            Vec::new()
        };
        Some(BodyView {
            email_id: email.clone(),
            text: Some(format!(
                "Hi Mira,\n\n{}.\n\nThanks,\n{}\n",
                row.preview,
                row.from.display()
            )),
            html: None,
            attachments,
            headers: Vec::new(),
        })
    }

    fn identities(&self) -> Vec<IdentityView> {
        self.identities.clone()
    }

    fn masked_emails(&self) -> Vec<MaskedEmailView> {
        self.masked.clone()
    }

    fn masked_for_address(&self, address: &str) -> Option<MaskedEmailView> {
        self.masked
            .iter()
            .find(|m| m.email.eq_ignore_ascii_case(address))
            .cloned()
    }

    fn contacts_prefix(&self, prefix: &str, limit: usize) -> Vec<ContactView> {
        let p = prefix.to_lowercase();
        self.contacts
            .iter()
            .filter(|c| {
                c.name
                    .to_lowercase()
                    .split_whitespace()
                    .any(|w| w.starts_with(&p))
                    || c.name.to_lowercase().starts_with(&p)
                    || c.emails.iter().any(|e| e.to_lowercase().starts_with(&p))
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn search_filter_preview(&self, query: &str) -> Option<String> {
        self.filter_previews.get(query).cloned()
    }

    fn search_local(&self, query: &str, limit: usize) -> SearchResults {
        let mut words: Vec<String> = Vec::new();
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut issues = Vec::new();
        for term in query.split_whitespace() {
            match term.split_once(':') {
                Some((k, v)) => {
                    let k = k.to_lowercase();
                    let v = v.to_lowercase();
                    if v.is_empty() {
                        issues.push(SearchIssue {
                            term: term.to_owned(),
                            kind: SearchIssueKind::EmptyValue,
                        });
                    } else if matches!(k.as_str(), "from" | "to" | "subject" | "is" | "has") {
                        fields.push((k, v));
                    } else {
                        issues.push(SearchIssue {
                            term: term.to_owned(),
                            kind: SearchIssueKind::UnsupportedOperator,
                        });
                    }
                }
                None => words.push(term.to_lowercase()),
            }
        }
        if words.is_empty() && fields.is_empty() {
            return SearchResults {
                issues,
                ..SearchResults::default()
            };
        }
        let marks = |text: &str| -> Vec<std::ops::Range<usize>> {
            let lower = text.to_lowercase();
            if lower.len() != text.len() {
                // Case folding changed byte offsets; mark nothing rather
                // than mark the wrong bytes.
                return Vec::new();
            }
            let mut v = Vec::new();
            for w in &words {
                let mut from = 0;
                while let Some(i) = lower[from..].find(w.as_str()) {
                    v.push(from + i..from + i + w.len());
                    from += i + w.len().max(1);
                }
            }
            v.sort_by_key(|r| r.start);
            v
        };
        let mut hits: Vec<(Timestamp, usize)> = Vec::new();
        let mut seen = HashSet::new();
        for (i, e) in self.emails.iter().enumerate() {
            let d = &e.data;
            let hay = format!(
                "{} {} {} {}",
                d.subject,
                d.preview,
                d.from.display(),
                d.from.email
            )
            .to_lowercase();
            let words_ok = words.iter().all(|w| hay.contains(w.as_str()));
            let fields_ok = fields.iter().all(|(k, v)| match k.as_str() {
                "from" => {
                    d.from.display().to_lowercase().contains(v.as_str())
                        || d.from.email.to_lowercase().contains(v.as_str())
                }
                "to" => d.to.iter().chain(&d.cc).any(|p| {
                    p.email.to_lowercase().contains(v.as_str())
                        || p.display().to_lowercase().contains(v.as_str())
                }),
                "subject" => d.subject.to_lowercase().contains(v.as_str()),
                "is" => match v.as_str() {
                    "unread" => e.state.unread,
                    "read" | "seen" => !e.state.unread,
                    "pinned" | "flagged" => e.state.flagged,
                    _ => false,
                },
                "has" => v == "attachment" && d.has_attachment,
                _ => false,
            });
            if words_ok && fields_ok && !e.state.mailboxes.is_empty() && seen.insert(&d.thread) {
                hits.push((d.date, i));
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let hits = hits
            .into_iter()
            .take(limit)
            .filter_map(|(_, i)| {
                let e = &self.emails[i];
                let mailbox = e.state.mailboxes.iter().next()?.clone();
                let row = self.row(&mailbox, &e.data.thread)?;
                Some(SearchHit {
                    subject: Highlight {
                        text: e.data.subject.clone(),
                        matches: marks(&e.data.subject),
                    },
                    snippet: Highlight {
                        text: e.data.preview.clone(),
                        matches: marks(&e.data.preview),
                    },
                    row,
                    mailbox: Some(mailbox),
                })
            })
            .collect();
        SearchResults {
            hits,
            issues,
            full_text: false,
        }
    }

    fn saved_searches(&self) -> Vec<SavedSearchView> {
        self.saved.clone()
    }

    fn rules(&self) -> Vec<RuleJson> {
        self.rules.clone()
    }

    fn drafts(&self) -> Vec<DraftView> {
        let mut v = self.drafts.clone();
        v.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
        v
    }

    fn rule_renders(&self, limit: usize) -> Vec<RuleRenderView> {
        let mut v = self.renders.clone();
        v.sort_by_key(|r| std::cmp::Reverse((r.rendered_at, r.id)));
        v.truncate(limit);
        v
    }

    fn dry_run_messages(&self, limit: usize) -> Vec<DryRunMessage> {
        use mejl_sieve::{EmailAddress, MessageFacts};
        let addr = |p: &Participant| EmailAddress {
            name: p.name.clone(),
            email: p.email.clone(),
        };
        let mut order: Vec<usize> = (0..self.emails.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.emails[i].data.date));
        order
            .into_iter()
            .filter(|&i| !self.emails[i].state.mailboxes.is_empty())
            .take(limit)
            .map(|i| {
                let e = &self.emails[i];
                let headers = self
                    .bodies
                    .get(&e.data.id)
                    .map(|b| b.headers.clone())
                    .unwrap_or_default();
                DryRunMessage {
                    email_id: e.data.id.clone(),
                    thread_id: e.data.thread.clone(),
                    date: e.data.date,
                    mailboxes: e.state.mailboxes.iter().cloned().collect(),
                    facts: MessageFacts {
                        from: vec![addr(&e.data.from)],
                        to: e.data.to.iter().map(addr).collect(),
                        cc: e.data.cc.iter().map(addr).collect(),
                        subject: Some(e.data.subject.clone()),
                        headers,
                        size: e.data.size,
                        delivered_to: e.data.to.iter().map(|p| p.email.clone()).collect(),
                    },
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn store() -> MemoryData {
        let mut d = MemoryData::new(AccountView {
            name: "Mira Holt".into(),
            email: "mira@example.org".into(),
        });
        d.add_mailbox("in", None, "Inbox", Some(MailboxRole::Inbox), 0);
        d.add_mailbox("ar", None, "Archive", Some(MailboxRole::Archive), 1);
        d.add_mailbox("tr", None, "Trash", Some(MailboxRole::Trash), 2);
        d.add_mailbox("se", None, "Sent", Some(MailboxRole::Sent), 3);
        let p = Participant::new("Ada Example", "ada@example.com");
        let mut a = NewEmail::new(
            "e1",
            "t1",
            p.clone(),
            "First",
            ts("2026-10-20T10:00:00Z"),
            "in",
        );
        a.unread = true;
        d.add_email(a);
        let mut b = NewEmail::new(
            "e2",
            "t1",
            p.clone(),
            "Re: First",
            ts("2026-10-21T10:00:00Z"),
            "se",
        );
        b.from = Participant::new("Mira Holt", "mira@example.org");
        d.add_email(b);
        d.add_email(NewEmail::new(
            "e3",
            "t2",
            p,
            "Second",
            ts("2026-10-22T10:00:00Z"),
            "in",
        ));
        d
    }

    #[test]
    fn pages_are_newest_first_and_counted() {
        let d = store();
        let inbox = MailboxId::from("in");
        let page = d.conversation_page(&inbox, 0, 10);
        assert_eq!(page.total, 2);
        let ids: Vec<_> = page.items.iter().map(|r| r.thread_id.as_str()).collect();
        assert_eq!(ids, ["t2", "t1"]);
        assert_eq!(page.items[1].message_count, 2);
        assert!(page.items[1].unread);
        assert_eq!(d.stats().page_calls, 1);
        assert_eq!(d.stats().max_limit, 10);
        let inbox_view = d.mailboxes().into_iter().find(|m| m.id == inbox).unwrap();
        assert_eq!(
            (inbox_view.total_threads, inbox_view.unread_threads),
            (2, 1)
        );
    }

    #[test]
    fn archive_keeps_sent_copy_and_undo_restores() {
        let mut d = store();
        let inbox = MailboxId::from("in");
        let target = Target {
            ids: TargetIds::Threads(vec!["t1".into()]),
            from_mailbox: Some(inbox.clone()),
        };
        let change = d.apply(&Mutation::Archive(target));
        assert_eq!(change.threads, vec![ThreadId::from("t1")]);
        assert_eq!(d.conversation_page(&inbox, 0, 10).total, 1);
        assert_eq!(d.conversation_page(&"ar".into(), 0, 10).total, 1);
        assert_eq!(d.conversation_page(&"se".into(), 0, 10).total, 1);
        d.apply(&Mutation::Undo);
        assert_eq!(d.conversation_page(&inbox, 0, 10).total, 2);
        d.apply(&Mutation::Redo);
        assert_eq!(d.conversation_page(&inbox, 0, 10).total, 1);
    }

    #[test]
    fn keywords_and_permanent_delete() {
        let mut d = store();
        let inbox = MailboxId::from("in");
        let t1 = Target {
            ids: TargetIds::Threads(vec!["t1".into()]),
            from_mailbox: Some(inbox.clone()),
        };
        d.apply(&Mutation::SetKeyword {
            target: t1.clone(),
            keyword: Keyword::Seen,
            value: true,
        });
        let page = d.conversation_page(&inbox, 0, 10);
        assert!(!page.items[1].unread);
        d.apply(&Mutation::DeletePermanently(Target {
            ids: TargetIds::AllInMailbox {
                mailbox: inbox.clone(),
                except: vec!["t2".into()],
            },
            from_mailbox: Some(inbox.clone()),
        }));
        let page = d.conversation_page(&inbox, 0, 10);
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].thread_id.as_str(), "t2");
        assert!(d.conversation(&"t1".into()).is_some(), "sent copy survives");
    }

    #[test]
    fn synthetic_rows_come_from_their_index() {
        let now = ts("2026-10-23T14:02:31Z");
        let d = MemoryData::synthetic(100_000, now);
        let inbox = d.mailbox_for_role(MailboxRole::Inbox).unwrap();
        let page = d.conversation_page(&inbox, 99_990, 50);
        assert_eq!(page.total, 100_000);
        assert_eq!(page.items.len(), 10);
        assert_eq!(page.items[0].thread_id.as_str(), "s99990");
        assert_eq!(d.conversation_index(&inbox, &"s99990".into()), Some(99_990));
        assert_eq!(d.conversation_index(&inbox, &"s100000".into()), None);
        assert!(d.message_body(&"se5".into()).is_some());
        assert!(d.conversation(&"s5".into()).is_some());
        assert_eq!(d.stats().rows_returned, 10);
    }

    #[test]
    fn lookups() {
        let mut d = store();
        d.add_masked(MaskedEmailView {
            id: "m1".into(),
            email: "kettle.fern4821@example.org".into(),
            state: crate::data::MaskedState::Enabled,
            for_domain: "shop.example.com".into(),
            description: "Larkspur shop".into(),
            created_at: None,
            last_message_at: None,
            created_by: "mejl".into(),
            local_messages: 0,
            local_in_spam: 0,
        });
        d.add_contact(ContactView {
            id: "c1".into(),
            name: "Juniper Tran".into(),
            emails: vec!["juniper@garden.example.com".into()],
        });
        assert!(
            d.masked_for_address("KETTLE.fern4821@example.org")
                .is_some()
        );
        assert_eq!(d.contacts_prefix("tr", 5).len(), 1);
        assert_eq!(d.contacts_prefix("jun", 5).len(), 1);
        assert!(d.contacts_prefix("zz", 5).is_empty());
        assert_eq!(d.search_local("second", 5).hits.len(), 1);
        assert!(d.search_local("  ", 5).hits.is_empty());
        let r = d.search_local("from:ada deliveredto:x is:unread", 5);
        assert_eq!(r.hits.len(), 1, "{r:?}");
        assert_eq!(r.issues[0].kind, SearchIssueKind::UnsupportedOperator);
        let r = d.search_local("first", 5);
        assert_eq!(r.hits[0].subject.matches, vec![0..5]);
    }
}
