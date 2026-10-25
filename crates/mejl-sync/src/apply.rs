//! Applying server data on the store thread, in one cache transaction per
//! response, with the staleness check and the data change scope.

use std::collections::BTreeSet;

use mejl_jmap::method::{ChangesResponse, GetResponse, QueryChangesResponse, QueryResponse};
use mejl_jmap::types::{ContactCard, Email, Identity, Mailbox, MaskedEmail, Thread};
use mejl_jmap::{Id, Session, State};
use mejl_store::{QueryChangesOutcome, type_name};

use crate::error::SyncError;
use crate::event::DataChanges;
use crate::store_thread::{Kinds, Stamp, StoreCtx};

/// The prefix of the query cache key for a mailbox's conversation window.
const MAILBOX_KEY_PREFIX: &str = "mailbox:";

/// The query cache key for a mailbox's conversation window. Read the window
/// with [`Reader::query_page`](mejl_store::Reader::query_page).
pub fn mailbox_query_key(mailbox: &Id) -> String {
    format!("{MAILBOX_KEY_PREFIX}{mailbox}")
}

/// The mailbox a query cache key belongs to.
pub(crate) fn mailbox_of_key(key: &str) -> Option<Id> {
    key.strip_prefix(MAILBOX_KEY_PREFIX).map(Id::from)
}

/// Server data to store. Everything in one `Apply` goes into one cache
/// transaction.
#[derive(Debug, Default)]
pub(crate) struct Apply {
    /// The flush generations when the data was requested.
    pub stamp: Stamp,
    pub session: Option<Session>,
    pub mailboxes_all: Option<GetResponse<Mailbox>>,
    pub mailbox_changes: Option<(ChangesResponse<Mailbox>, Vec<Mailbox>)>,
    pub clear_states: Vec<&'static str>,
    pub destroyed_emails: Vec<Id>,
    pub email_changes: Option<(ChangesResponse<Email>, Vec<Email>)>,
    pub emails: Vec<Email>,
    pub email_state: Option<State>,
    pub email_state_if_unset: Option<State>,
    pub thread_changes: Option<(ChangesResponse<Thread>, Vec<Thread>)>,
    pub threads: Vec<Thread>,
    pub thread_state: Option<State>,
    pub thread_state_if_unset: Option<State>,
    pub identities_all: Option<GetResponse<Identity>>,
    pub identity_changes: Option<(ChangesResponse<Identity>, Vec<Identity>)>,
    pub contacts_all: Option<GetResponse<ContactCard>>,
    pub contact_changes: Option<(ChangesResponse<ContactCard>, Vec<ContactCard>)>,
    pub masked_all: Option<GetResponse<MaskedEmail>>,
    /// Collapsed-thread query windows to save, by cache key.
    pub queries: Vec<(String, QueryResponse)>,
    pub query_changes: Vec<(String, QueryChangesResponse)>,
}

/// What applying did.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// Nothing was stored: the data was requested before a recorded flush
    /// result. Fetch it again.
    pub stale: bool,
    /// Ids the server named that are not cached and came without list
    /// properties.
    pub needs_fetch: Vec<Id>,
    pub query_outcomes: Vec<(String, QueryChangesOutcome)>,
    pub changes: DataChanges,
}

impl Apply {
    pub(crate) fn new(stamp: Stamp) -> Self {
        Apply {
            stamp,
            ..Default::default()
        }
    }

    /// The kinds of object that can confirm a done overlay. Threads and
    /// query windows cannot, so they are not checked.
    fn kinds(&self) -> Kinds {
        Kinds {
            email: self.email_changes.is_some()
                || !self.emails.is_empty()
                || !self.destroyed_emails.is_empty(),
            mailbox: self.mailboxes_all.is_some() || self.mailbox_changes.is_some(),
            masked: self.masked_all.is_some(),
            contact: self.contacts_all.is_some() || self.contact_changes.is_some(),
        }
    }

    fn email_ids(&self) -> Vec<Id> {
        let mut ids: BTreeSet<Id> = self.emails.iter().map(|e| e.id.clone()).collect();
        ids.extend(self.destroyed_emails.iter().cloned());
        if let Some((changes, fetched)) = &self.email_changes {
            ids.extend(changes.created.iter().cloned());
            ids.extend(changes.updated.iter().cloned());
            ids.extend(changes.destroyed.iter().cloned());
            ids.extend(fetched.iter().map(|e| e.id.clone()));
        }
        ids.into_iter().collect()
    }
}

/// Stores `a`, unless it is stale.
pub(crate) fn run(ctx: &mut StoreCtx, a: Apply) -> Result<Applied, SyncError> {
    if ctx.gens.is_stale(a.stamp, a.kinds()) {
        tracing::debug!("dropping server data requested before a recorded flush");
        return Ok(Applied {
            stale: true,
            ..Default::default()
        });
    }
    let now = ctx.now();
    let email_ids = a.email_ids();
    let before = ctx.memberships(&email_ids);
    let mut changes = DataChanges::default();
    let mut applied = Applied::default();
    let mut destroyed: BTreeSet<Id> = a.destroyed_emails.iter().cloned().collect();
    {
        let mut batch = ctx.store.batch()?;
        if let Some(session) = &a.session {
            batch.save_session(session)?;
        }
        if let Some(get) = &a.mailboxes_all {
            batch.replace_mailboxes(get)?;
            changes.mailboxes = true;
        }
        if let Some((ch, fetched)) = &a.mailbox_changes {
            batch.apply_mailbox_changes(ch, fetched)?;
            if !ch.created.is_empty() || !ch.updated.is_empty() || !ch.destroyed.is_empty() {
                changes.mailboxes = true;
            }
        }
        for ty in &a.clear_states {
            batch.clear_type_state(ty)?;
        }
        if !a.destroyed_emails.is_empty() {
            batch.destroy_emails(&a.destroyed_emails)?;
        }
        if let Some((ch, fetched)) = &a.email_changes {
            let report = batch.apply_email_changes(ch, fetched)?;
            destroyed.extend(ch.destroyed.iter().cloned());
            applied.needs_fetch.extend(report.needs_fetch);
        }
        if !a.emails.is_empty() {
            let report = batch.upsert_emails(&a.emails)?;
            applied.needs_fetch.extend(report.needs_fetch);
        }
        if let Some(state) = &a.email_state {
            batch.set_type_state(type_name::EMAIL, state)?;
        }
        if let Some(state) = &a.email_state_if_unset
            && batch.type_state(type_name::EMAIL)?.is_none()
        {
            batch.set_type_state(type_name::EMAIL, state)?;
        }
        if let Some((ch, fetched)) = &a.thread_changes {
            batch.apply_thread_changes(ch, fetched)?;
            changes.threads.extend(ch.created.iter().cloned());
            changes.threads.extend(ch.updated.iter().cloned());
            changes.threads.extend(ch.destroyed.iter().cloned());
        }
        if !a.threads.is_empty() {
            batch.upsert_threads(&a.threads)?;
            changes
                .threads
                .extend(a.threads.iter().map(|t| t.id.clone()));
        }
        if let Some(state) = &a.thread_state {
            batch.set_type_state(type_name::THREAD, state)?;
        }
        if let Some(state) = &a.thread_state_if_unset
            && batch.type_state(type_name::THREAD)?.is_none()
        {
            batch.set_type_state(type_name::THREAD, state)?;
        }
        if let Some(get) = &a.identities_all {
            batch.replace_identities(get)?;
            changes.identities = true;
        }
        if let Some((ch, fetched)) = &a.identity_changes {
            batch.apply_identity_changes(ch, fetched)?;
            changes.identities |=
                !ch.created.is_empty() || !ch.updated.is_empty() || !ch.destroyed.is_empty();
        }
        if let Some(get) = &a.contacts_all {
            batch.replace_contacts(get)?;
            changes.contacts = true;
        }
        if let Some((ch, fetched)) = &a.contact_changes {
            batch.apply_contact_changes(ch, fetched)?;
            changes.contacts |=
                !ch.created.is_empty() || !ch.updated.is_empty() || !ch.destroyed.is_empty();
        }
        if let Some(get) = &a.masked_all {
            let diff = batch.replace_masked_emails(get)?;
            if !diff.created.is_empty() || !diff.updated.is_empty() || !diff.destroyed.is_empty() {
                changes.masked_emails = true;
                // Masks are linked to messages, so their badges may change.
                changes.everything |= !diff.destroyed.is_empty();
            }
        }
        for (key, response) in &a.queries {
            batch.save_query(key, response, true, now)?;
            if let Some(mailbox) = mailbox_of_key(key) {
                changes.mailbox_lists.insert(mailbox);
            }
        }
        for (key, response) in &a.query_changes {
            let outcome = batch.apply_query_changes(key, response, now)?;
            if outcome == QueryChangesOutcome::Applied
                && let Some(mailbox) = mailbox_of_key(key)
                && (!response.added.is_empty() || !response.removed.is_empty())
            {
                changes.mailbox_lists.insert(mailbox);
            }
            applied.query_outcomes.push((key.clone(), outcome));
        }
        batch.commit()?;
    }
    applied.needs_fetch.retain(|id| !destroyed.contains(id));
    applied.needs_fetch.sort();
    applied.needs_fetch.dedup();
    let mut scope = ctx.email_scope(&email_ids, &before);
    if !scope.everything {
        for email in a
            .emails
            .iter()
            .chain(a.email_changes.iter().flat_map(|(_, f)| f.iter()))
        {
            if let Some(thread) = &email.thread_id {
                scope.threads.insert(thread.clone());
            }
        }
    }
    if changes.threads.len() > crate::store_thread::SCOPE_LIMIT {
        changes.threads.clear();
        changes.everything = true;
    }
    changes.merge(scope);
    applied.changes = changes.clone();
    ctx.events.data(changes);
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_jmap::Patch;
    use mejl_store::{EmailPatchTarget, EnqueueOptions, FlushOutcome, Mutation, Store};
    use serde_json::json;

    fn email(id: &str, mailbox: &str) -> Email {
        serde_json::from_value(json!({
            "id": id,
            "threadId": format!("t-{id}"),
            "mailboxIds": {mailbox: true},
            "keywords": {},
            "receivedAt": "2026-10-20T09:00:00Z",
            "subject": "Rota",
            "from": [{"name": "Juniper Tran", "email": "juniper@example.org"}],
        }))
        .unwrap()
    }

    fn flags(id: &str, mailbox: &str) -> Email {
        serde_json::from_value(json!({"id": id, "mailboxIds": {mailbox: true}, "keywords": {}}))
            .unwrap()
    }

    fn mailboxes() -> GetResponse<Mailbox> {
        serde_json::from_value(json!({
            "accountId": "A1",
            "state": "m1",
            "list": [
                {"id": "inbox", "name": "Inbox", "role": "inbox"},
                {"id": "archive", "name": "Archive", "role": "archive"}
            ]
        }))
        .unwrap()
    }

    fn ids_in(ctx: &mut StoreCtx, mailbox: &str) -> Vec<String> {
        ctx.reader()
            .unwrap()
            .messages(mailbox, 0, 10)
            .unwrap()
            .rows
            .into_iter()
            .map(|r| r.email_id.into_inner())
            .collect()
    }

    /// The confirmation contract: data requested before a flush result was
    /// recorded is dropped, so it cannot confirm the change with old data.
    #[test]
    fn stale_data_never_confirms_a_recorded_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("acct"), Default::default()).unwrap();
        let mut ctx = StoreCtx::for_test(store);
        let now = ctx.now();
        let mut first = Apply::new(ctx.gens.stamp());
        first.mailboxes_all = Some(mailboxes());
        first.emails = vec![email("e1", "inbox")];
        first.email_state = Some(State::from("s1"));
        assert!(!run(&mut ctx, first).unwrap().stale);

        // The user archives e1 and the flusher sends it.
        ctx.store
            .enqueue(
                Mutation::EmailPatch(vec![EmailPatchTarget::new(
                    "e1",
                    Patch::new()
                        .mailbox("inbox", false)
                        .mailbox("archive", true),
                )]),
                EnqueueOptions::default(),
                now,
            )
            .unwrap();
        let unit = ctx.store.next_flush(now, 100).unwrap().unwrap();
        // A sync request goes out before the flush result is recorded...
        let old_stamp = ctx.gens.stamp();
        // ...then the result is recorded, as the flusher does.
        ctx.store
            .finish_flush(&unit, FlushOutcome::ok(), now)
            .unwrap();
        ctx.gens.bump(Kinds {
            email: true,
            ..Default::default()
        });

        // The old response still has e1 in the Inbox.
        let mut changes = ChangesResponse::<Email>::new("A1", "s1", "s2");
        changes.updated = vec![Id::from("e1")];
        let mut stale = Apply::new(old_stamp);
        stale.email_changes = Some((changes.clone(), vec![flags("e1", "inbox")]));
        let applied = run(&mut ctx, stale).unwrap();
        assert!(applied.stale);
        assert!(applied.changes.is_empty());
        // Nothing was stored: the row stays archived and the state stays.
        assert_eq!(ids_in(&mut ctx, "archive"), ["e1"]);
        assert!(ids_in(&mut ctx, "inbox").is_empty());
        assert_eq!(
            ctx.store.type_state(type_name::EMAIL).unwrap(),
            Some(State::from("s1"))
        );

        // A response requested after the flush applies and confirms.
        let mut fresh = Apply::new(ctx.gens.stamp());
        fresh.email_changes = Some((changes, vec![flags("e1", "archive")]));
        let applied = run(&mut ctx, fresh).unwrap();
        assert!(!applied.stale);
        assert!(applied.changes.mailbox_lists.contains("archive"));
        assert_eq!(
            ctx.store.type_state(type_name::EMAIL).unwrap(),
            Some(State::from("s2"))
        );
        assert_eq!(ids_in(&mut ctx, "archive"), ["e1"]);

        // Confirmed: a later move elsewhere shows as is, with no overlay
        // pulling the row back.
        let mut later = ChangesResponse::<Email>::new("A1", "s2", "s3");
        later.updated = vec![Id::from("e1")];
        let mut moved = Apply::new(ctx.gens.stamp());
        moved.email_changes = Some((later, vec![flags("e1", "inbox")]));
        run(&mut ctx, moved).unwrap();
        assert_eq!(ids_in(&mut ctx, "inbox"), ["e1"]);
    }

    #[test]
    fn data_of_other_kinds_is_not_held_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("acct"), Default::default()).unwrap();
        let mut ctx = StoreCtx::for_test(store);
        let stamp = ctx.gens.stamp();
        ctx.gens.bump(Kinds {
            masked: true,
            ..Default::default()
        });
        let mut a = Apply::new(stamp);
        a.mailboxes_all = Some(mailboxes());
        a.emails = vec![email("e1", "inbox")];
        let applied = run(&mut ctx, a).unwrap();
        assert!(!applied.stale);
        assert!(applied.changes.mailboxes);
        assert!(applied.changes.emails.contains("e1"));
        assert!(applied.changes.threads.contains("t-e1"));
        assert!(applied.changes.mailbox_lists.contains("inbox"));
    }

    #[test]
    fn mailbox_keys_round_trip() {
        let key = mailbox_query_key(&Id::from("M1"));
        assert_eq!(key, "mailbox:M1");
        assert_eq!(mailbox_of_key(&key), Some(Id::from("M1")));
        assert_eq!(mailbox_of_key("search:x"), None);
    }
}
