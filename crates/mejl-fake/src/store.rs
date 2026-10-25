//! The account state and the mutation primitives every write goes through.
//!
//! All writes, whether from `/set` methods, submissions, simulated delivery
//! or "another client", use these primitives. They keep threads, mailbox
//! counts and the change logs consistent.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::{SignedDuration, Timestamp};

use crate::changes::{ChangeKind, ChangeLog};
use crate::config::{FakeConfig, Limits};
use crate::ids::{IdGen, Rng};
use crate::model::{
    Address, Attachment, Body, ContactCard, Email, Identity, Mailbox, MaskedEmail,
    MaskedEmailState, Submission, Thread, Upload,
};

/// A deterministic clock. It starts at a base time and moves one second on
/// every read, so new objects are always strictly newer than old ones.
#[derive(Debug, Clone)]
pub(crate) struct Clock {
    base: Timestamp,
    ticks: i64,
}

impl Clock {
    pub(crate) fn new(base: Timestamp) -> Self {
        Clock { base, ticks: 0 }
    }

    pub(crate) fn now(&mut self) -> Timestamp {
        let t = self
            .base
            .checked_add(SignedDuration::from_secs(self.ticks))
            .unwrap_or(self.base);
        self.ticks += 1;
        t
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub total_emails: u64,
    pub unread_emails: u64,
    pub total_threads: u64,
    pub unread_threads: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.total_emails += o.total_emails;
        self.unread_emails += o.unread_emails;
        self.total_threads += o.total_threads;
        self.unread_threads += o.unread_threads;
    }

    fn sub(&mut self, o: &Counts) {
        self.total_emails = self.total_emails.saturating_sub(o.total_emails);
        self.unread_emails = self.unread_emails.saturating_sub(o.unread_emails);
        self.total_threads = self.total_threads.saturating_sub(o.total_threads);
        self.unread_threads = self.unread_threads.saturating_sub(o.unread_threads);
    }
}

/// The JMAP data types the fake keeps state for, in push order.
pub(crate) const PUSH_TYPES: &[&str] = &[
    "Mailbox",
    "Email",
    "EmailDelivery",
    "Thread",
    "Identity",
    "EmailSubmission",
    "MaskedEmail",
    "AddressBook",
    "ContactCard",
];

#[derive(Debug, Clone)]
pub(crate) struct Logs {
    pub mailbox: ChangeLog,
    pub email: ChangeLog,
    pub thread: ChangeLog,
    pub identity: ChangeLog,
    pub submission: ChangeLog,
    pub masked: ChangeLog,
    pub contact: ChangeLog,
    pub address_book: ChangeLog,
    pub email_delivery: u64,
}

impl Logs {
    fn new(limit: usize) -> Self {
        Logs {
            mailbox: ChangeLog::new(limit),
            email: ChangeLog::new(limit),
            thread: ChangeLog::new(limit),
            identity: ChangeLog::new(limit),
            submission: ChangeLog::new(limit),
            masked: ChangeLog::new(limit),
            contact: ChangeLog::new(limit),
            address_book: ChangeLog::new(limit),
            email_delivery: 0,
        }
    }

    pub(crate) fn expire_all(&mut self) {
        for log in [
            &mut self.mailbox,
            &mut self.email,
            &mut self.thread,
            &mut self.identity,
            &mut self.submission,
            &mut self.masked,
            &mut self.contact,
            &mut self.address_book,
        ] {
            log.expire();
        }
    }
}

/// Everything needed to create an email.
#[derive(Debug, Clone, Default)]
pub(crate) struct NewEmail {
    pub mailbox_ids: BTreeSet<String>,
    pub keywords: BTreeSet<String>,
    pub received_at: Option<Timestamp>,
    pub sent_at: Option<Timestamp>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<Vec<String>>,
    pub references: Option<Vec<String>>,
    pub sender: Option<Vec<Address>>,
    pub from: Option<Vec<Address>>,
    pub to: Option<Vec<Address>>,
    pub cc: Option<Vec<Address>>,
    pub bcc: Option<Vec<Address>>,
    pub reply_to: Option<Vec<Address>>,
    pub subject: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    pub text: Option<Body>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// Why a simulated delivery did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeliveryError {
    /// The recipient is a deleted masked address, so the mail bounces.
    Bounced(String),
    /// No mailbox to deliver into.
    NoMailbox,
}

/// The name of the masked email header the fake adds on delivery.
///
/// Fastmail's help pages name `X-FM-Masked-Email`, but whether it is stored
/// on the message, and what its value is, has not been checked against a
/// live account. Fastmail's generated Sieve also mentions
/// `Fastmail-MaskedEmail`. The fake stores the masked address as the value.
pub const MASKED_EMAIL_HEADER: &str = "X-FM-Masked-Email";

pub(crate) struct Store {
    pub account_id: String,
    pub username: String,
    pub display_name: String,
    pub masked_domain: String,
    pub masked_created_by: String,
    pub limits: Limits,
    pub clock: Clock,
    pub ids: IdGen,
    pub rng: Rng,
    pub session_state: String,
    pub mailboxes: BTreeMap<String, Mailbox>,
    pub emails: BTreeMap<String, Email>,
    pub threads: BTreeMap<String, Thread>,
    /// Message-ID to thread id. Lookups only, never iterated.
    msgid_threads: HashMap<String, String>,
    pub identities: BTreeMap<String, Identity>,
    pub masked: BTreeMap<String, MaskedEmail>,
    pub contacts: BTreeMap<String, ContactCard>,
    pub submissions: BTreeMap<String, Submission>,
    pub uploads: BTreeMap<String, Upload>,
    pub logs: Logs,
    counts: BTreeMap<String, Counts>,
    /// Threads touched since the last count flush, with their contribution
    /// to mailbox counts before the first touch.
    touched: BTreeMap<String, BTreeMap<String, Counts>>,
    /// While true, writes skip change logs and count tracking. Used to load
    /// initial data, after which counts are rebuilt and logs reset.
    bulk: bool,
}

impl Store {
    pub(crate) fn new(cfg: &FakeConfig, start: Timestamp) -> Self {
        let mut store = Store {
            account_id: cfg.account_id.clone(),
            username: cfg.username.clone(),
            display_name: cfg.display_name.clone(),
            masked_domain: cfg.masked_email_domain.clone(),
            masked_created_by: cfg.masked_email_created_by.clone(),
            limits: cfg.limits.clone(),
            clock: Clock::new(start),
            ids: IdGen::default(),
            rng: Rng::new(start.as_second() as u64),
            session_state: "S1".to_owned(),
            mailboxes: BTreeMap::new(),
            emails: BTreeMap::new(),
            threads: BTreeMap::new(),
            msgid_threads: HashMap::new(),
            identities: BTreeMap::new(),
            masked: BTreeMap::new(),
            contacts: BTreeMap::new(),
            submissions: BTreeMap::new(),
            uploads: BTreeMap::new(),
            logs: Logs::new(cfg.change_log_limit),
            counts: BTreeMap::new(),
            touched: BTreeMap::new(),
            bulk: false,
        };
        store.begin_bulk();
        for (name, role, order) in [
            ("Inbox", "inbox", 1),
            ("Drafts", "drafts", 2),
            ("Sent", "sent", 3),
            ("Archive", "archive", 4),
            ("Spam", "junk", 5),
            ("Trash", "trash", 6),
        ] {
            store.create_mailbox(name, None, Some(role), order);
        }
        let identity = Identity {
            id: store.ids.make('I'),
            name: cfg.display_name.clone(),
            email: cfg.username.clone(),
            reply_to: None,
            bcc: None,
            text_signature: String::new(),
            html_signature: String::new(),
            may_delete: false,
        };
        store.identities.insert(identity.id.clone(), identity);
        store.end_bulk();
        store
    }

    /// Starts loading initial data without change tracking.
    pub(crate) fn begin_bulk(&mut self) {
        self.bulk = true;
    }

    /// Ends initial loading: rebuilds counts and makes the current states
    /// the baseline every client starts from.
    pub(crate) fn end_bulk(&mut self) {
        self.bulk = false;
        self.touched.clear();
        self.recompute_all_counts();
        self.logs.expire_all();
    }

    pub(crate) fn now(&mut self) -> Timestamp {
        self.clock.now()
    }

    // ----- lookups

    pub(crate) fn role_id(&self, role: &str) -> Option<String> {
        self.mailboxes
            .values()
            .find(|m| m.role.as_deref() == Some(role))
            .map(|m| m.id.clone())
    }

    /// Finds a mailbox by role, id, name or slash-separated path such as
    /// `Projects/Garden club`.
    pub(crate) fn find_mailbox(&self, key: &str) -> Option<String> {
        if self.mailboxes.contains_key(key) {
            return Some(key.to_owned());
        }
        if let Some(id) = self.role_id(&key.to_ascii_lowercase()) {
            return Some(id);
        }
        let mut parent: Option<String> = None;
        for segment in key.split('/') {
            let found = self
                .mailboxes
                .values()
                .find(|m| m.parent_id == parent && m.name.eq_ignore_ascii_case(segment))?;
            parent = Some(found.id.clone());
        }
        parent
    }

    pub(crate) fn counts(&self, mailbox_id: &str) -> Counts {
        self.counts.get(mailbox_id).copied().unwrap_or_default()
    }

    pub(crate) fn email_state(&self) -> String {
        self.logs.email.state()
    }

    /// Current state strings for push, keyed by type name.
    pub(crate) fn push_states(&self, masked_without_state: bool) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for ty in PUSH_TYPES {
            let state = match *ty {
                "Mailbox" => self.logs.mailbox.state(),
                "Email" => self.logs.email.state(),
                "EmailDelivery" => self.logs.email_delivery.to_string(),
                "Thread" => self.logs.thread.state(),
                "Identity" => self.logs.identity.state(),
                "EmailSubmission" => self.logs.submission.state(),
                "MaskedEmail" if masked_without_state => continue,
                "MaskedEmail" => self.logs.masked.state(),
                "AddressBook" => self.logs.address_book.state(),
                "ContactCard" => self.logs.contact.state(),
                _ => continue,
            };
            out.insert((*ty).to_owned(), state);
        }
        out
    }

    // ----- mailbox counts

    fn thread_contrib(&self, thread_id: &str) -> BTreeMap<String, Counts> {
        let mut out: BTreeMap<String, Counts> = BTreeMap::new();
        let Some(thread) = self.threads.get(thread_id) else {
            return out;
        };
        let trash = self.role_id("trash");
        let emails: Vec<&Email> = thread
            .email_ids
            .iter()
            .filter_map(|id| self.emails.get(id))
            .collect();
        let only_in_trash = |e: &Email| {
            trash
                .as_ref()
                .is_some_and(|t| e.mailbox_ids.len() == 1 && e.mailbox_ids.contains(t))
        };
        let in_trash = |e: &Email| trash.as_ref().is_some_and(|t| e.mailbox_ids.contains(t));
        // RFC 8621 section 2: mail only in Trash does not make a thread
        // unread elsewhere, and mail outside Trash does not count for Trash.
        let unread_outside = emails.iter().any(|e| e.is_unread() && !only_in_trash(e));
        let unread_in_trash = emails.iter().any(|e| e.is_unread() && in_trash(e));
        for e in &emails {
            for m in &e.mailbox_ids {
                let c = out.entry(m.clone()).or_default();
                c.total_emails += 1;
                if e.is_unread() {
                    c.unread_emails += 1;
                }
            }
        }
        for (m, c) in out.iter_mut() {
            c.total_threads = 1;
            let unread = if trash.as_deref() == Some(m.as_str()) {
                unread_in_trash
            } else {
                unread_outside
            };
            c.unread_threads = u64::from(unread);
        }
        out
    }

    /// Call before changing any email in this thread.
    fn touch_thread(&mut self, thread_id: &str) {
        if self.bulk || self.touched.contains_key(thread_id) {
            return;
        }
        let before = self.thread_contrib(thread_id);
        self.touched.insert(thread_id.to_owned(), before);
    }

    /// Applies count changes from touched threads and logs a counts-only
    /// update for every mailbox whose counts moved.
    pub(crate) fn flush_counts(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for (thread_id, before) in touched {
            let after = self.thread_contrib(&thread_id);
            let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
            for m in keys {
                let b = before.get(m).copied().unwrap_or_default();
                let a = after.get(m).copied().unwrap_or_default();
                if a == b {
                    continue;
                }
                let entry = self.counts.entry(m.clone()).or_default();
                entry.sub(&b);
                entry.add(&a);
                changed.insert(m.clone());
            }
        }
        for m in changed {
            if self.mailboxes.contains_key(&m) {
                self.logs.mailbox.record_counts(&m);
            } else {
                self.counts.remove(&m);
            }
        }
    }

    pub(crate) fn recompute_all_counts(&mut self) {
        let mut counts: BTreeMap<String, Counts> = BTreeMap::new();
        let thread_ids: Vec<String> = self.threads.keys().cloned().collect();
        for t in thread_ids {
            for (m, c) in self.thread_contrib(&t) {
                counts.entry(m).or_default().add(&c);
            }
        }
        self.counts = counts;
    }

    // ----- mailboxes

    pub(crate) fn create_mailbox(
        &mut self,
        name: &str,
        parent_id: Option<String>,
        role: Option<&str>,
        sort_order: u32,
    ) -> String {
        let id = self.ids.make('M');
        self.mailboxes.insert(
            id.clone(),
            Mailbox {
                id: id.clone(),
                name: name.to_owned(),
                parent_id,
                role: role.map(str::to_owned),
                sort_order,
                is_subscribed: true,
            },
        );
        if !self.bulk {
            self.logs.mailbox.record(&id, ChangeKind::Created);
        }
        id
    }

    pub(crate) fn update_mailbox(&mut self, id: &str, f: impl FnOnce(&mut Mailbox)) -> bool {
        let Some(mb) = self.mailboxes.get_mut(id) else {
            return false;
        };
        let before = mb.clone();
        f(mb);
        let changed = before.name != mb.name
            || before.parent_id != mb.parent_id
            || before.sort_order != mb.sort_order
            || before.is_subscribed != mb.is_subscribed
            || before.role != mb.role;
        if changed && !self.bulk {
            self.logs.mailbox.record(id, ChangeKind::Updated);
        }
        true
    }

    /// Destroys a mailbox. Emails only in it are destroyed; others just
    /// leave it. The caller checks children and `onDestroyRemoveEmails`.
    pub(crate) fn destroy_mailbox(&mut self, id: &str) {
        let members: Vec<String> = self
            .emails
            .values()
            .filter(|e| e.mailbox_ids.contains(id))
            .map(|e| e.id.clone())
            .collect();
        for email_id in members {
            let Some(e) = self.emails.get(&email_id) else {
                continue;
            };
            if e.mailbox_ids.len() == 1 {
                self.destroy_email(&email_id);
            } else {
                let mut ids = e.mailbox_ids.clone();
                let keywords = e.keywords.clone();
                ids.remove(id);
                self.set_email_flags(&email_id, ids, keywords);
            }
        }
        self.mailboxes.remove(id);
        self.counts.remove(id);
        if !self.bulk {
            self.logs.mailbox.record(id, ChangeKind::Destroyed);
        }
    }

    // ----- emails

    pub(crate) fn insert_email(&mut self, new: NewEmail) -> String {
        let id = self.ids.make('E');
        let received_at = match new.received_at {
            Some(t) => t,
            None => self.now(),
        };
        let message_id = new
            .message_id
            .clone()
            .unwrap_or_else(|| format!("{id}@{}", self.message_id_domain()));
        let mut lookups: Vec<&String> = Vec::new();
        if let Some(refs) = &new.references {
            lookups.extend(refs.iter().rev());
        }
        if let Some(irt) = &new.in_reply_to {
            lookups.extend(irt.iter());
        }
        lookups.push(&message_id);
        let existing_thread = lookups
            .into_iter()
            .filter_map(|m| self.msgid_threads.get(m))
            .find(|t| self.threads.contains_key(*t))
            .cloned();
        let (thread_id, new_thread) = match existing_thread {
            Some(t) => (t, false),
            None => (self.ids.make('T'), true),
        };
        self.msgid_threads
            .entry(message_id.clone())
            .or_insert_with(|| thread_id.clone());

        let mut email = Email {
            id: id.clone(),
            thread_id: thread_id.clone(),
            mailbox_ids: new.mailbox_ids,
            keywords: new.keywords,
            received_at,
            sent_at: new.sent_at,
            size: 0,
            message_id: Some(vec![message_id]),
            in_reply_to: new.in_reply_to,
            references: new.references,
            sender: new.sender,
            from: new.from,
            to: new.to,
            cc: new.cc,
            bcc: new.bcc,
            reply_to: new.reply_to,
            subject: new.subject,
            extra_headers: new.extra_headers,
            text: new.text,
            html: new.html,
            attachments: new.attachments,
        };
        email.size = email.compute_size();

        if !new_thread {
            self.touch_thread(&thread_id);
        }
        self.emails.insert(id.clone(), email);
        let thread = self
            .threads
            .entry(thread_id.clone())
            .or_insert_with(|| Thread {
                id: thread_id.clone(),
                email_ids: Vec::new(),
            });
        thread.email_ids.push(id.clone());
        let emails = &self.emails;
        thread.email_ids.sort_by(|a, b| {
            let ka = emails.get(a).map(|e| e.received_at);
            let kb = emails.get(b).map(|e| e.received_at);
            ka.cmp(&kb).then_with(|| a.cmp(b))
        });
        if new_thread {
            // A brand new thread has no "before" contribution.
            if !self.bulk {
                self.touched.entry(thread_id.clone()).or_default();
            }
        }
        if !self.bulk {
            self.logs
                .email
                .record_email(&id, ChangeKind::Created, &thread_id);
            let kind = if new_thread {
                ChangeKind::Created
            } else {
                ChangeKind::Updated
            };
            self.logs.thread.record(&thread_id, kind);
        }
        id
    }

    fn message_id_domain(&self) -> String {
        self.username
            .rsplit_once('@')
            .map(|(_, d)| d.to_owned())
            .unwrap_or_else(|| "example.org".to_owned())
    }

    /// Replaces an email's mailboxes and keywords. Returns false when the
    /// email does not exist. A no-op write is not logged.
    pub(crate) fn set_email_flags(
        &mut self,
        id: &str,
        mailbox_ids: BTreeSet<String>,
        keywords: BTreeSet<String>,
    ) -> bool {
        let Some(e) = self.emails.get(id) else {
            return false;
        };
        if e.mailbox_ids == mailbox_ids && e.keywords == keywords {
            return true;
        }
        let thread_id = e.thread_id.clone();
        self.touch_thread(&thread_id);
        if let Some(e) = self.emails.get_mut(id) {
            e.mailbox_ids = mailbox_ids;
            e.keywords = keywords;
        }
        if !self.bulk {
            self.logs
                .email
                .record_email(id, ChangeKind::Updated, &thread_id);
        }
        true
    }

    pub(crate) fn destroy_email(&mut self, id: &str) -> bool {
        let Some(thread_id) = self.emails.get(id).map(|e| e.thread_id.clone()) else {
            return false;
        };
        self.touch_thread(&thread_id);
        self.emails.remove(id);
        let mut thread_gone = false;
        if let Some(t) = self.threads.get_mut(&thread_id) {
            t.email_ids.retain(|e| e != id);
            thread_gone = t.email_ids.is_empty();
        }
        if thread_gone {
            self.threads.remove(&thread_id);
        }
        if !self.bulk {
            self.logs
                .email
                .record_email(id, ChangeKind::Destroyed, &thread_id);
            let kind = if thread_gone {
                ChangeKind::Destroyed
            } else {
                ChangeKind::Updated
            };
            self.logs.thread.record(&thread_id, kind);
        }
        true
    }

    /// Delivers a message as if it arrived over SMTP for `recipients`.
    ///
    /// Mail to a masked address gets the masked email header. A deleted mask
    /// bounces, a disabled one files the mail in Trash, and a pending one
    /// becomes enabled.
    pub(crate) fn deliver(
        &mut self,
        mut new: NewEmail,
        recipients: &[String],
    ) -> Result<String, DeliveryError> {
        let received_at = match new.received_at {
            Some(t) => t,
            None => self.now(),
        };
        new.received_at = Some(received_at);
        let lowered: Vec<String> = recipients.iter().map(|r| r.to_ascii_lowercase()).collect();
        let mask_id = self
            .masked
            .values()
            .find(|m| lowered.contains(&m.email.to_ascii_lowercase()))
            .map(|m| m.id.clone());
        if let Some(mask_id) = mask_id {
            let trash = self.role_id("trash");
            let Some(mask) = self.masked.get_mut(&mask_id) else {
                return Err(DeliveryError::NoMailbox);
            };
            if mask.state == MaskedEmailState::Deleted {
                return Err(DeliveryError::Bounced(mask.email.clone()));
            }
            if mask.state == MaskedEmailState::Disabled {
                let trash = trash.ok_or(DeliveryError::NoMailbox)?;
                new.mailbox_ids = BTreeSet::from([trash]);
            }
            if mask.state == MaskedEmailState::Pending {
                mask.state = MaskedEmailState::Enabled;
            }
            mask.last_message_at = Some(received_at);
            let address = mask.email.clone();
            if !new
                .extra_headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case(MASKED_EMAIL_HEADER))
            {
                new.extra_headers
                    .push((MASKED_EMAIL_HEADER.to_owned(), address));
            }
            if !self.bulk {
                self.logs.masked.record(&mask_id, ChangeKind::Updated);
            }
        }
        if new.mailbox_ids.is_empty() {
            let inbox = self.role_id("inbox").ok_or(DeliveryError::NoMailbox)?;
            new.mailbox_ids.insert(inbox);
        }
        let id = self.insert_email(new);
        if !self.bulk {
            self.logs.email_delivery += 1;
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let cfg = FakeConfig::default();
        Store::new(&cfg, "2026-10-23T12:00:00Z".parse().unwrap())
    }

    fn mail(store: &Store, mailbox: &str, seen: bool) -> NewEmail {
        let mut n = NewEmail::default();
        n.mailbox_ids.insert(store.find_mailbox(mailbox).unwrap());
        if seen {
            n.keywords.insert("$seen".into());
        }
        n.subject = Some("x".into());
        n
    }

    #[test]
    fn standard_mailboxes_and_identity_exist() {
        let s = store();
        for role in ["inbox", "drafts", "sent", "archive", "junk", "trash"] {
            assert!(s.role_id(role).is_some(), "{role}");
        }
        assert_eq!(s.identities.len(), 1);
        // Initial data is the baseline: nothing to report, no older state.
        let page = s
            .logs
            .mailbox
            .changes(&s.logs.mailbox.state(), None)
            .unwrap();
        assert!(page.created.is_empty());
        assert!(s.logs.mailbox.changes("1", None).is_none());
    }

    #[test]
    fn counts_follow_writes() {
        let mut s = store();
        let inbox = s.role_id("inbox").unwrap();
        let a = s.insert_email(mail(&s, "inbox", false));
        let _b = s.insert_email(mail(&s, "inbox", true));
        s.flush_counts();
        let c = s.counts(&inbox);
        assert_eq!(
            (
                c.total_emails,
                c.unread_emails,
                c.total_threads,
                c.unread_threads
            ),
            (2, 1, 2, 1)
        );
        let mailbox_state = s.logs.mailbox.state();
        let mut kw = BTreeSet::new();
        kw.insert("$seen".to_owned());
        let ids = s.emails[&a].mailbox_ids.clone();
        s.set_email_flags(&a, ids, kw);
        s.flush_counts();
        assert_eq!(s.counts(&inbox).unread_emails, 0);
        let page = s.logs.mailbox.changes(&mailbox_state, None).unwrap();
        assert_eq!(page.updated, vec![inbox.clone()]);
        assert!(page.updated_counts_only);
        s.destroy_email(&a);
        s.flush_counts();
        assert_eq!(s.counts(&inbox).total_emails, 1);
    }

    #[test]
    fn trash_rules_for_unread_threads() {
        let mut s = store();
        let inbox = s.role_id("inbox").unwrap();
        let trash = s.role_id("trash").unwrap();
        let first = s.insert_email(mail(&s, "inbox", true));
        let msgid = s.emails[&first].message_id.clone().unwrap();
        let mut reply = mail(&s, "trash", false);
        reply.in_reply_to = Some(msgid);
        let second = s.insert_email(reply);
        s.flush_counts();
        assert_eq!(s.emails[&first].thread_id, s.emails[&second].thread_id);
        // The only unread mail is only in Trash, so the inbox thread is read.
        assert_eq!(s.counts(&inbox).unread_threads, 0);
        assert_eq!(s.counts(&trash).unread_threads, 1);
        assert_eq!(s.counts(&trash).total_threads, 1);
    }

    #[test]
    fn unread_mail_elsewhere_makes_thread_unread_here() {
        let mut s = store();
        let inbox = s.role_id("inbox").unwrap();
        let first = s.insert_email(mail(&s, "inbox", true));
        let msgid = s.emails[&first].message_id.clone().unwrap();
        let mut reply = mail(&s, "archive", false);
        reply.references = Some(msgid);
        s.insert_email(reply);
        s.flush_counts();
        let c = s.counts(&inbox);
        assert_eq!((c.unread_emails, c.unread_threads), (0, 1));
    }

    #[test]
    fn drafts_are_not_unread() {
        let mut s = store();
        let drafts = s.role_id("drafts").unwrap();
        let mut n = mail(&s, "drafts", false);
        n.keywords.insert("$draft".into());
        s.insert_email(n);
        s.flush_counts();
        assert_eq!(s.counts(&drafts).unread_emails, 0);
        assert_eq!(s.counts(&drafts).total_emails, 1);
    }

    #[test]
    fn destroying_a_mailbox_moves_or_destroys_mail() {
        let mut s = store();
        let folder = s.create_mailbox("Folder", None, None, 10);
        let inbox = s.role_id("inbox").unwrap();
        let mut only = NewEmail::default();
        only.mailbox_ids.insert(folder.clone());
        let only = s.insert_email(only);
        let mut both = NewEmail::default();
        both.mailbox_ids.insert(folder.clone());
        both.mailbox_ids.insert(inbox.clone());
        let both = s.insert_email(both);
        s.flush_counts();
        s.destroy_mailbox(&folder);
        s.flush_counts();
        assert!(!s.emails.contains_key(&only));
        assert_eq!(s.emails[&both].mailbox_ids, BTreeSet::from([inbox]));
        assert!(!s.mailboxes.contains_key(&folder));
    }

    #[test]
    fn find_mailbox_by_path() {
        let mut s = store();
        let projects = s.create_mailbox("Projects", None, None, 10);
        let garden = s.create_mailbox("Garden club", Some(projects.clone()), None, 1);
        assert_eq!(s.find_mailbox("Projects/Garden club"), Some(garden.clone()));
        assert_eq!(s.find_mailbox("projects/garden CLUB"), Some(garden));
        assert_eq!(s.find_mailbox("Projects"), Some(projects));
        assert_eq!(s.find_mailbox("Garden club"), None);
        assert!(s.find_mailbox("Inbox").is_some());
        assert!(s.find_mailbox("junk").is_some());
    }

    #[test]
    fn clock_moves_forward() {
        let mut s = store();
        let a = s.now();
        let b = s.now();
        assert!(b > a);
    }
}
