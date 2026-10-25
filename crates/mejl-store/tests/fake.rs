//! End to end with mejl-fake: fetch through mejl-jmap's client, apply,
//! read back, then flush a change and confirm it.

mod common;

use std::sync::Arc;

use common::{Fixture, now};
use mejl_fake::FakeServer;
use mejl_jmap::method::{ChangesArgs, GetArgs, QueryArgs, SetArgs};
use mejl_jmap::methods::{
    EmailChanges, EmailGet, EmailGetArgs, EmailQuery, EmailQueryArgs, EmailSet, IdentityGet,
    MailboxGet, MaskedEmailGet, ThreadGet,
};
use mejl_jmap::types::Email;
use mejl_jmap::{
    Client, Comparator, EmailFilterCondition, Filter, Id, Patch, RequestBuilder, Secret, capability,
};
use mejl_store::{EmailPatchTarget, FlushOp, FlushOutcome, Mutation, type_name};

const LIST_PROPERTIES: &[&str] = &[
    "id",
    "threadId",
    "mailboxIds",
    "keywords",
    "from",
    "to",
    "cc",
    "subject",
    "receivedAt",
    "size",
    "preview",
    "hasAttachment",
    "header:X-FM-Masked-Email:asText",
];

fn list_get(account: &Id) -> EmailGetArgs {
    EmailGetArgs::from(
        GetArgs::<Email>::by_reference(account.clone()).properties(LIST_PROPERTIES.iter().copied()),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn demo_account_cold_start_and_flush() {
    let server = FakeServer::demo(now());
    let client = Client::new(
        Arc::new(server.transport()),
        server.session_url(),
        Secret::new(server.token()),
    );
    let session = client.fetch_session().await.expect("session");
    let account = session
        .primary_account(capability::MAIL)
        .expect("mail account")
        .clone();
    let inbox = Id::from(server.mailbox_id("inbox").expect("inbox").as_str());

    // The cold start batch from the architecture: mailboxes, identities,
    // the first page of the inbox collapsed by thread, its threads and
    // every message in them, and the masked addresses.
    let mut rb = RequestBuilder::new();
    let mailboxes = rb.call::<MailboxGet>(GetArgs::all(account.clone()));
    let identities = rb.call::<IdentityGet>(GetArgs::all(account.clone()));
    let mut query = QueryArgs::<EmailFilterCondition>::new(account.clone());
    query.filter = Some(Filter::from(EmailFilterCondition {
        in_mailbox: Some(inbox.clone()),
        ..Default::default()
    }));
    query.sort = Some(vec![Comparator::desc("receivedAt")]);
    query.limit = Some(60);
    query.calculate_total = Some(true);
    let page_query = rb.call::<EmailQuery>(EmailQueryArgs {
        query,
        collapse_threads: Some(true),
    });
    let page = rb.call_with_refs::<EmailGet>(list_get(&account), [("ids", page_query.ids())]);
    let threads = rb.call_with_refs::<ThreadGet>(
        GetArgs::by_reference(account.clone()),
        [("ids", page.thread_ids())],
    );
    let members = rb.call_with_refs::<EmailGet>(list_get(&account), [("ids", threads.email_ids())]);
    let masked = rb.call::<MaskedEmailGet>(GetArgs::all(account.clone()));
    let response = client.call(&rb.build().unwrap()).await.expect("cold start");

    let mailboxes = response.get(&mailboxes).unwrap();
    let identities = response.get(&identities).unwrap();
    let page_query = response.get(&page_query).unwrap();
    let page = response.get(&page).unwrap();
    let threads = response.get(&threads).unwrap();
    let members = response.get(&members).unwrap();
    let masked = response.get(&masked).unwrap();
    assert!(!page.list.is_empty());

    let mut f = Fixture::new();
    let mut batch = f.store.batch().unwrap();
    batch.save_session(&session).unwrap();
    batch.replace_mailboxes(&mailboxes).unwrap();
    batch.replace_identities(&identities).unwrap();
    batch.replace_masked_emails(&masked).unwrap();
    batch.save_query("inbox", &page_query, true, now()).unwrap();
    let report = batch.upsert_emails(&page.list).unwrap();
    assert!(report.needs_fetch.is_empty());
    batch.upsert_threads(&threads.list).unwrap();
    batch.upsert_emails(&members.list).unwrap();
    batch.set_type_state(type_name::EMAIL, &page.state).unwrap();
    batch
        .set_type_state(type_name::THREAD, &threads.state)
        .unwrap();
    batch.commit().unwrap();
    f.check_consistency();

    let reader = f.reader();
    assert_eq!(
        reader.session().unwrap().map(|s| s.username),
        Some(session.username.clone())
    );
    assert_eq!(reader.identities().unwrap().len(), 3);

    // The local conversation list matches the server's collapsed query.
    let conversations = reader.conversations(inbox.as_str(), 0, 60).unwrap();
    let local: Vec<&str> = conversations
        .rows
        .iter()
        .map(|r| r.email_id.as_str())
        .collect();
    let remote: Vec<&str> = page_query.ids.iter().map(Id::as_str).collect();
    assert_eq!(local, remote);
    assert_eq!(conversations.total, page_query.ids.len() as u64);
    let cached = f.store.query_page("inbox", 0, 60).unwrap().unwrap();
    assert_eq!(cached.ids, page_query.ids);

    // Server counts come through as reported.
    let inbox_row = reader
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.id == inbox)
        .unwrap();
    let server_inbox = mailboxes.list.iter().find(|m| m.id == inbox).unwrap();
    assert_eq!(Some(inbox_row.unread_emails), server_inbox.unread_emails);
    assert_eq!(
        inbox_row.unread_emails,
        conversations
            .rows
            .iter()
            .map(|r| u64::from(r.unread_count))
            .sum::<u64>(),
        "every unread inbox message is on the first page of the demo"
    );

    // The masked order thread is flagged as masked and linked to its mask.
    let shipped = conversations
        .rows
        .iter()
        .find(|r| r.subject == "Re: Your order 48213 has shipped")
        .expect("the masked order thread is in the inbox");
    assert!(shipped.masked);
    let thread = reader.conversation(shipped.thread_id.as_str()).unwrap();
    assert_eq!(thread.len(), 3);
    assert!(
        thread
            .windows(2)
            .all(|w| w[0].row.received_at <= w[1].row.received_at)
    );
    let delivered = thread.iter().find(|m| m.row.masked).unwrap();
    let mask_id = delivered
        .row
        .masked_email_id
        .clone()
        .expect("linked to a mask");
    let mask = reader
        .masked_emails()
        .unwrap()
        .into_iter()
        .find(|m| m.masked.id == mask_id)
        .unwrap();
    assert_eq!(
        mask.masked.email.as_deref(),
        Some("kettle.fern4821@example.org")
    );
    assert!(mask.local_messages >= 1);
    assert_eq!(
        reader
            .thread_email_ids(shipped.thread_id.as_str())
            .unwrap()
            .map(|ids| ids.len()),
        Some(3)
    );

    // Mark the first unread conversation read, flush it to the fake, then
    // sync and see the overlay confirmed.
    let unread = conversations.rows.iter().find(|r| r.is_unread()).unwrap();
    let email_id = unread.email_id.clone();
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![EmailPatchTarget::new(
                email_id.clone(),
                Patch::new().keyword("$seen", true),
            )]),
            Default::default(),
            now(),
        )
        .unwrap();
    assert!(
        reader
            .email(email_id.as_str())
            .unwrap()
            .unwrap()
            .headers
            .row
            .seen
    );

    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::EmailSet { update, destroy } = &unit.op else {
        panic!("expected Email/set");
    };
    let mut set = SetArgs::<Email>::new(account.clone());
    for (id, patch) in update {
        set = set.update(id.clone(), patch.clone());
    }
    for id in destroy {
        set = set.destroy(id.clone());
    }
    let mut rb = RequestBuilder::new();
    let set_call = rb.call::<EmailSet>(set);
    let response = client.call(&rb.build().unwrap()).await.expect("Email/set");
    let set_response = response.get(&set_call).unwrap();
    assert!(set_response.is_complete_success());
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();

    let since = f.store.type_state(type_name::EMAIL).unwrap().unwrap();
    let mut rb = RequestBuilder::new();
    let changes = rb.call::<EmailChanges>(ChangesArgs::new(account.clone(), since));
    let updated = rb.call_with_refs::<EmailGet>(
        EmailGetArgs::from(
            GetArgs::<Email>::by_reference(account.clone()).properties(["mailboxIds", "keywords"]),
        ),
        [("ids", changes.updated())],
    );
    let response = client.call(&rb.build().unwrap()).await.expect("sync");
    let changes = response.get(&changes).unwrap();
    let updated = response.get(&updated).unwrap();
    assert!(changes.updated.contains(&email_id));
    let mut batch = f.store.batch().unwrap();
    let report = batch.apply_email_changes(&changes, &updated.list).unwrap();
    batch.commit().unwrap();
    assert!(report.needs_fetch.is_empty());

    let overlays: i64 = f
        .cache()
        .query_row("SELECT count(*) FROM overlay", [], |r| r.get(0))
        .unwrap();
    assert_eq!(overlays, 0, "the server's answer confirmed the change");
    assert!(
        reader
            .email(email_id.as_str())
            .unwrap()
            .unwrap()
            .headers
            .row
            .seen
    );
    f.check_consistency();
}
