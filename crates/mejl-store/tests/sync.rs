//! Applying server data: every writer path, read back through a Reader.

mod common;

use common::*;
use mejl_jmap::method::{ChangesResponse, GetResponse, QueryChangesResponse, QueryResponse};
use mejl_jmap::types::{ContactCard, Email, Identity, Mailbox, MaskedEmail, Thread};
use mejl_jmap::{Id, Session};
use mejl_store::{QueryChangesOutcome, StoreOptions, type_name};
use serde_json::json;

fn get<T: serde::de::DeserializeOwned>(state: &str, list: serde_json::Value) -> GetResponse<T> {
    serde_json::from_value(json!({"accountId": "A1", "state": state, "list": list, "notFound": []}))
        .unwrap()
}

#[test]
fn session_and_type_states() {
    let mut f = Fixture::new();
    let session: Session = serde_json::from_value(json!({
        "capabilities": {"urn:ietf:params:jmap:core": {}},
        "accounts": {"A1": {"name": "mira@example.org", "isPersonal": true}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
        "username": "mira@example.org",
        "apiUrl": "https://jmap.example.com/api/",
        "state": "sess-1"
    }))
    .unwrap();
    let mut batch = f.store.batch().unwrap();
    batch.save_session(&session).unwrap();
    batch
        .set_type_state(type_name::EMAIL, &state("e-7"))
        .unwrap();
    assert_eq!(
        batch.type_state(type_name::EMAIL).unwrap(),
        Some(state("e-7"))
    );
    batch.commit().unwrap();

    let reader = f.reader();
    assert_eq!(reader.session().unwrap(), Some(session.clone()));
    assert_eq!(
        reader.type_state(type_name::EMAIL).unwrap(),
        Some(state("e-7"))
    );
    assert_eq!(f.store.type_state(type_name::MAILBOX).unwrap(), None);

    let mut batch = f.store.batch().unwrap();
    batch.clear_type_state(type_name::EMAIL).unwrap();
    batch.commit().unwrap();
    assert_eq!(f.store.type_state(type_name::EMAIL).unwrap(), None);
}

#[test]
fn dropped_batch_rolls_back() {
    let mut f = Fixture::new().with_mailboxes();
    {
        let mut batch = f.store.batch().unwrap();
        batch.upsert_emails(&[email("e1").build()]).unwrap();
        batch.set_type_state(type_name::EMAIL, &state("x")).unwrap();
        // Dropped without commit.
    }
    let page = f.reader().conversations(INBOX, 0, 10).unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(f.store.type_state(type_name::EMAIL).unwrap(), None);
}

#[test]
fn mailboxes_full_replace_and_partial_changes() {
    let mut f = Fixture::new().with_mailboxes();
    let reader = f.reader();
    let boxes = reader.mailboxes().unwrap();
    assert_eq!(boxes.len(), 8);
    let garden = boxes.iter().find(|m| m.id == GARDEN).unwrap();
    assert_eq!(garden.parent_id.as_ref().map(Id::as_str), Some(PROJECTS));
    assert!(garden.my_rights.as_ref().is_some_and(|r| r.may_rename));
    assert_eq!(
        f.store.type_state(type_name::MAILBOX).unwrap(),
        Some(state("mb-1"))
    );

    // Count-only update merges into the stored mailbox.
    let mut changes = ChangesResponse::<Mailbox>::new("A1", "mb-1", "mb-2");
    changes.updated = vec![Id::from(INBOX)];
    changes.destroyed = vec![Id::from(GARDEN)];
    changes.updated_properties = Some(vec!["unreadEmails".into()]);
    let partial: Mailbox =
        serde_json::from_value(json!({"id": INBOX, "unreadEmails": 12, "totalEmails": 214}))
            .unwrap();
    let mut batch = f.store.batch().unwrap();
    batch.apply_mailbox_changes(&changes, &[partial]).unwrap();
    batch.commit().unwrap();

    let boxes = reader.mailboxes().unwrap();
    let inbox = boxes.iter().find(|m| m.id == INBOX).unwrap();
    assert_eq!(inbox.name, "Inbox");
    assert_eq!(inbox.unread_emails, 12);
    assert_eq!(inbox.total_emails, 214);
    assert_eq!(inbox.role.as_ref().map(|r| r.as_str()), Some("inbox"));
    assert!(boxes.iter().all(|m| m.id != GARDEN));
    assert_eq!(
        f.store.type_state(type_name::MAILBOX).unwrap(),
        Some(state("mb-2"))
    );

    // A full replace drops what is gone.
    let mut batch = f.store.batch().unwrap();
    batch
        .replace_mailboxes(&get(
            "mb-3",
            json!([mailbox(INBOX, "Inbox", Some("inbox"), None)]),
        ))
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(reader.mailboxes().unwrap().len(), 1);
}

#[test]
fn email_insert_partial_update_and_needs_fetch() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[email("e1").subject("Plot rota").build()]);

    // A flags-only update keeps every list property.
    let mut batch = f.store.batch().unwrap();
    let report = batch
        .upsert_emails(&[
            flags("e1", &[ARCHIVE], &["$seen", "$Flagged"]),
            flags("e2", &[INBOX], &[]),
        ])
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.inserted, 0);
    assert_eq!(report.needs_fetch, vec![Id::from("e2")]);

    let reader = f.reader();
    let detail = reader.email("e1").unwrap().unwrap();
    assert_eq!(detail.headers.row.subject, "Plot rota");
    assert_eq!(detail.headers.row.from[0].email, "juniper@example.org");
    assert!(detail.headers.row.seen);
    assert!(detail.headers.row.flagged, "keywords are case-insensitive");
    assert!(detail.headers.keywords.contains("$flagged"));
    assert_eq!(
        detail
            .headers
            .mailbox_ids
            .iter()
            .map(Id::as_str)
            .collect::<Vec<_>>(),
        vec![ARCHIVE]
    );
    assert!(reader.conversations(INBOX, 0, 10).unwrap().rows.is_empty());
    assert_eq!(reader.conversations(ARCHIVE, 0, 10).unwrap().rows.len(), 1);
    assert!(reader.email("e2").unwrap().is_none());
    f.check_consistency();
}

#[test]
fn changes_with_created_updated_and_destroyed() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("e1").thread("t1").build(),
        email("e2").thread("t1").at("2026-10-21T09:00:00Z").build(),
        email("e3").build(),
    ]);
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_body(
            &serde_json::from_value(json!({
                "id": "e3",
                "bodyValues": {"1": {"value": "zebra crossing"}},
                "textBody": [{"partId": "1", "type": "text/plain"}],
                "attachments": [{"partId": "2", "blobId": "B2", "name": "a.pdf", "type": "application/pdf", "size": 10}]
            }))
            .unwrap(),
            now(),
        )
        .unwrap();
    batch.commit().unwrap();

    let mut changes = ChangesResponse::<Email>::new("A1", "e-1", "e-2");
    changes.created = vec![Id::from("e4"), Id::from("e5")];
    changes.updated = vec![Id::from("e2")];
    changes.destroyed = vec![Id::from("e3"), Id::from("never-cached")];
    let fetched = vec![
        email("e4").thread("t1").at("2026-10-22T09:00:00Z").build(),
        flags("e2", &[INBOX], &["$seen"]),
    ];
    let mut batch = f.store.batch().unwrap();
    let report = batch.apply_email_changes(&changes, &fetched).unwrap();
    batch.commit().unwrap();
    assert_eq!(report.inserted, 1);
    assert_eq!(report.updated, 1);
    assert_eq!(report.needs_fetch, vec![Id::from("e5")]);
    assert_eq!(
        f.store.type_state(type_name::EMAIL).unwrap(),
        Some(state("e-2"))
    );

    let reader = f.reader();
    assert!(reader.email("e3").unwrap().is_none());
    let conn = f.cache();
    for table in ["body_meta", "body_part", "attachment"] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table} rows survive the destroy");
    }
    let hits = reader.search("zebra", &Default::default()).unwrap();
    assert!(hits.hits.is_empty());

    let page = reader.conversations(INBOX, 0, 10).unwrap();
    assert_eq!(page.total, 1);
    let row = &page.rows[0];
    assert_eq!(row.thread_id, "t1");
    assert_eq!(row.email_id, "e4");
    assert_eq!(row.count_in_mailbox, 3);
    assert_eq!(row.unread_count, 2);
    f.check_consistency();
}

#[test]
fn conversation_rows_collapse_threads_per_mailbox() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("a1")
            .thread("ta")
            .at("2026-10-20T08:00:00Z")
            .from("Juniper Tran", "juniper@example.org")
            .seen()
            .build(),
        email("a2")
            .thread("ta")
            .mailboxes(&[SENT])
            .at("2026-10-20T10:00:00Z")
            .from("Mira Holt", "mira@example.org")
            .seen()
            .build(),
        email("a3")
            .thread("ta")
            .at("2026-10-21T08:00:00Z")
            .from("Juniper Tran", "juniper@example.org")
            .attachment()
            .build(),
        email("b1")
            .thread("tb")
            .at("2026-10-22T08:00:00Z")
            .from("Owen Castillo", "owen@example.net")
            .keywords(&["$flagged", "$seen"])
            .build(),
        email("c1")
            .thread("tc")
            .at("2026-10-19T08:00:00Z")
            .masked("Kettle.Fern4821@example.org")
            .build(),
    ]);
    let reader = f.reader();
    let page = reader.conversations(INBOX, 0, 60).unwrap();
    assert_eq!(page.total, 3);
    let ids: Vec<&str> = page.rows.iter().map(|r| r.thread_id.as_str()).collect();
    assert_eq!(ids, vec!["tb", "ta", "tc"]);

    let ta = &page.rows[1];
    assert_eq!(ta.email_id, "a3");
    assert_eq!(ta.count_in_mailbox, 2);
    assert_eq!(ta.thread_count, 3);
    assert_eq!(ta.unread_count, 1);
    assert!(ta.is_unread());
    assert!(ta.has_attachment);
    assert!(!ta.flagged);
    let people: Vec<&str> = ta.participants.iter().map(|p| p.email.as_str()).collect();
    assert_eq!(people, vec!["juniper@example.org", "mira@example.org"]);
    assert!(page.rows[0].flagged);
    assert!(!page.rows[0].is_unread());
    assert!(page.rows[2].masked);

    let sent = reader.conversations(SENT, 0, 60).unwrap();
    assert_eq!(sent.rows[0].email_id, "a2");
    assert_eq!(sent.rows[0].count_in_mailbox, 1);

    // Offset and limit.
    let second = reader.conversations(INBOX, 1, 1).unwrap();
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].thread_id, "ta");
    assert_eq!(second.offset, 1);
    assert!(reader.conversations(INBOX, 10, 5).unwrap().rows.is_empty());
    assert!(
        reader
            .conversations("no-such-mailbox", 0, 5)
            .unwrap()
            .rows
            .is_empty()
    );

    // Flat messages, newest first.
    let flat = reader.messages(INBOX, 0, 60).unwrap();
    let ids: Vec<&str> = flat.rows.iter().map(|r| r.email_id.as_str()).collect();
    assert_eq!(ids, vec!["b1", "a3", "a1", "c1"]);
    assert_eq!(flat.total, 4);

    // Conversation detail in date order, across mailboxes.
    let thread = reader.conversation("ta").unwrap();
    let ids: Vec<&str> = thread.iter().map(|m| m.row.email_id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "a2", "a3"]);
    assert!(thread[1].mailbox_ids.contains(&Id::from(SENT)));
    assert_eq!(thread[0].row.to[0].name.as_deref(), Some("Mira Holt"));
    f.check_consistency();
}

#[test]
fn threads_identities_and_contacts() {
    let mut f = Fixture::new();
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_threads(&[serde_json::from_value::<Thread>(
            json!({"id": "t1", "emailIds": ["e1", "e2"]}),
        )
        .unwrap()])
        .unwrap();
    batch
        .replace_identities(&get::<Identity>(
            "id-1",
            json!([
                {"id": "I2", "name": "Mira at work", "email": "mira@work.example.com"},
                {"id": "I1", "name": "Mira Holt", "email": "mira@example.org"}
            ]),
        ))
        .unwrap();
    batch
        .replace_contacts(&get::<ContactCard>(
            "c-1",
            json!([
                {"id": "c1", "name": {"full": "Ångström Åsa"}, "emails": {"a": {"address": "Asa@Example.org"}}},
                {"id": "c2", "name": {"components": [{"kind": "given", "value": "Noor"}, {"kind": "surname", "value": "Haddad"}]},
                 "emails": {"a": {"address": "noor@example.net", "pref": 2}, "b": {"address": "noor.h@example.com", "pref": 1}}},
                {"id": "g1", "kind": "group", "name": {"full": "Garden club"}, "members": {"u1": true}}
            ]),
        ))
        .unwrap();
    batch.commit().unwrap();

    let reader = f.reader();
    assert_eq!(
        reader.thread_email_ids("t1").unwrap(),
        Some(vec![Id::from("e1"), Id::from("e2")])
    );
    let names: Vec<String> = reader
        .identities()
        .unwrap()
        .into_iter()
        .filter_map(|i| i.name)
        .collect();
    assert_eq!(names, vec!["Mira at work", "Mira Holt"]);

    let s = reader.address_suggestions("åsa", 10).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].email, "Asa@Example.org");
    assert_eq!(s[0].contact_id.as_ref().map(Id::as_str), Some("c1"));
    let s = reader.address_suggestions("ÅNG", 10).unwrap();
    assert_eq!(s.len(), 1, "prefix on a name word, ignoring case");
    let s = reader.address_suggestions("hadd", 10).unwrap();
    assert_eq!(s.len(), 2);
    assert!(reader.address_suggestions("", 10).unwrap().is_empty());
    assert!(
        reader.address_suggestions("%", 10).unwrap().is_empty(),
        "LIKE wildcards are escaped"
    );

    // Thread changes and identity changes.
    let mut tc = ChangesResponse::<Thread>::new("A1", "t-1", "t-2");
    tc.destroyed = vec![Id::from("t1")];
    let mut ic = ChangesResponse::<Identity>::new("A1", "id-1", "id-2");
    ic.destroyed = vec![Id::from("I2")];
    let added: Identity =
        serde_json::from_value(json!({"id": "I3", "name": "Hello", "email": "hello@example.org"}))
            .unwrap();
    let mut cc = ChangesResponse::<ContactCard>::new("A1", "c-1", "c-2");
    cc.destroyed = vec![Id::from("c1")];
    let mut batch = f.store.batch().unwrap();
    batch.apply_thread_changes(&tc, &[]).unwrap();
    batch.apply_identity_changes(&ic, &[added]).unwrap();
    batch.apply_contact_changes(&cc, &[]).unwrap();
    batch.commit().unwrap();
    assert_eq!(reader.thread_email_ids("t1").unwrap(), None);
    let names: Vec<String> = reader
        .identities()
        .unwrap()
        .into_iter()
        .filter_map(|i| i.name)
        .collect();
    assert_eq!(names, vec!["Mira Holt", "Hello"]);
    assert!(reader.address_suggestions("åsa", 10).unwrap().is_empty());
    assert_eq!(
        f.store.type_state(type_name::THREAD).unwrap(),
        Some(state("t-2"))
    );
    assert_eq!(
        f.store.type_state(type_name::IDENTITY).unwrap(),
        Some(state("id-2"))
    );
    assert_eq!(
        f.store.type_state(type_name::CONTACT_CARD).unwrap(),
        Some(state("c-2"))
    );
}

#[test]
fn recent_correspondents_rank_first() {
    let mut f = Fixture::new().with_mailboxes();
    let mut batch = f.store.batch().unwrap();
    batch
        .replace_contacts(&get::<ContactCard>(
            "c-1",
            json!([
                {"id": "c1", "name": {"full": "Lena Okafor"}, "emails": {"a": {"address": "lena@example.org"}}},
                {"id": "c2", "name": {"full": "Lars Berg"}, "emails": {"a": {"address": "lars@example.net"}}}
            ]),
        ))
        .unwrap();
    batch.commit().unwrap();
    f.apply(&[
        email("s1")
            .mailboxes(&[SENT])
            .to("Lars Berg", "lars@example.net")
            .at("2026-10-01T10:00:00Z")
            .build(),
        email("s2")
            .mailboxes(&[SENT])
            .to("Lars Berg", "Lars@Example.net")
            .at("2026-10-05T10:00:00Z")
            .build(),
        email("s3")
            .mailboxes(&[SENT])
            .to("Laila Noor", "laila@example.com")
            .at("2026-09-01T10:00:00Z")
            .build(),
    ]);
    let s = f.reader().address_suggestions("la", 10).unwrap();
    let emails: Vec<&str> = s.iter().map(|s| s.email.as_str()).collect();
    assert_eq!(emails, vec!["lars@example.net", "laila@example.com"]);
    assert_eq!(s[0].use_count, 2);
    assert_eq!(s[0].last_used, Some(ts("2026-10-05T10:00:00Z")));
    let s = f.reader().address_suggestions("le", 10).unwrap();
    assert_eq!(s[0].email, "lena@example.org");
    assert_eq!(s[0].last_used, None);
    let s = f.reader().address_suggestions("l", 1).unwrap();
    assert_eq!(s.len(), 1);
}

#[test]
fn masked_emails_diff_links_and_stats() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("m1")
            .masked("kettle.fern4821@example.org")
            .at("2026-10-20T09:00:00Z")
            .build(),
        email("m2")
            .masked("<Kettle.Fern4821@Example.org>")
            .at("2026-10-22T09:00:00Z")
            .mailboxes(&[SPAM])
            .build(),
        email("m3").masked("rapid.harbor9917@example.org").build(),
        email("plain").build(),
    ]);
    let reader = f.reader();
    assert!(
        reader
            .email("m1")
            .unwrap()
            .unwrap()
            .headers
            .row
            .masked_email_id
            .is_none()
    );

    let masks = json!([
        {"id": "me1", "email": "kettle.fern4821@example.org", "state": "enabled", "forDomain": "https://shop.example.com",
         "description": "Larkspur shop", "createdAt": "2025-03-14T10:00:00Z", "lastMessageAt": "2026-10-22T09:00:00Z"},
        {"id": "me2", "email": "quiet.otter@example.org", "state": "pending"}
    ]);
    let mut batch = f.store.batch().unwrap();
    let diff = batch
        .replace_masked_emails(&get::<MaskedEmail>("", masks))
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(diff.created.len(), 2);
    assert!(diff.updated.is_empty() && diff.destroyed.is_empty());
    assert_eq!(
        f.store.type_state(type_name::MASKED_EMAIL).unwrap(),
        Some(state(""))
    );

    let m1 = reader.email("m1").unwrap().unwrap();
    assert_eq!(
        m1.headers.row.masked_email_id.as_ref().map(Id::as_str),
        Some("me1")
    );
    assert_eq!(
        m1.headers.masked_address.as_deref(),
        Some("kettle.fern4821@example.org")
    );
    let rows = reader.masked_emails().unwrap();
    let me1 = rows.iter().find(|r| r.masked.id == "me1").unwrap();
    assert_eq!(me1.local_messages, 2);
    assert_eq!(me1.local_in_spam, 1);
    assert_eq!(me1.last_seen, Some(ts("2026-10-22T09:00:00Z")));
    let me2 = rows.iter().find(|r| r.masked.id == "me2").unwrap();
    assert_eq!(me2.local_messages, 0);
    assert!(
        reader
            .masked_email_by_address("KETTLE.fern4821@example.org")
            .unwrap()
            .is_some()
    );

    // A refresh that changes one, drops one and adds one, and links m3.
    let masks = json!([
        {"id": "me1", "email": "kettle.fern4821@example.org", "state": "disabled"},
        {"id": "me3", "email": "rapid.harbor9917@example.org", "state": "enabled"}
    ]);
    let mut batch = f.store.batch().unwrap();
    let diff = batch
        .replace_masked_emails(&get::<MaskedEmail>("", masks))
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(diff.created, vec![Id::from("me3")]);
    assert_eq!(diff.updated, vec![Id::from("me1")]);
    assert_eq!(diff.destroyed, vec![Id::from("me2")]);
    let m3 = reader.email("m3").unwrap().unwrap();
    assert_eq!(
        m3.headers.row.masked_email_id.as_ref().map(Id::as_str),
        Some("me3")
    );
    assert!(
        reader
            .email("plain")
            .unwrap()
            .unwrap()
            .headers
            .row
            .masked_email_id
            .is_none()
    );

    // Unchanged refresh reports nothing.
    let mut batch = f.store.batch().unwrap();
    let again = json!([
        {"id": "me1", "email": "kettle.fern4821@example.org", "state": "disabled"},
        {"id": "me3", "email": "rapid.harbor9917@example.org", "state": "enabled"}
    ]);
    let diff = batch
        .replace_masked_emails(&get::<MaskedEmail>("", again))
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(diff, Default::default());
}

#[test]
fn masked_header_alternate_name_and_clearing() {
    let mut f = Fixture::new().with_mailboxes();
    let alt: Email = serde_json::from_value(json!({
        "id": "x1", "threadId": "tx", "mailboxIds": {INBOX: true}, "receivedAt": "2026-10-20T09:00:00Z",
        "header:Fastmail-MaskedEmail:asText": " quiet.otter@example.org "
    }))
    .unwrap();
    f.apply(&[alt]);
    let reader = f.reader();
    assert_eq!(
        reader
            .email("x1")
            .unwrap()
            .unwrap()
            .headers
            .masked_address
            .as_deref(),
        Some("quiet.otter@example.org")
    );
    // A later fetch that asked for the header and got null clears it; a
    // fetch that did not ask leaves it alone.
    f.apply(&[flags("x1", &[INBOX], &[])]);
    assert!(reader.email("x1").unwrap().unwrap().headers.row.masked);
    let cleared: Email =
        serde_json::from_value(json!({"id": "x1", "header:X-FM-Masked-Email:asText": null}))
            .unwrap();
    f.apply(&[cleared]);
    assert!(!reader.email("x1").unwrap().unwrap().headers.row.masked);
    assert!(!reader.conversations(INBOX, 0, 10).unwrap().rows[0].masked);
}

fn body_email(id: &str, text: &str) -> Email {
    serde_json::from_value(json!({
        "id": id,
        "headers": [{"name": "Subject", "value": " hi"}],
        "bodyValues": {"1": {"value": text}, "2": {"value": format!("<p>{text}</p>"), "isTruncated": true}},
        "textBody": [{"partId": "1", "type": "text/plain", "charset": "utf-8"}],
        "htmlBody": [{"partId": "2", "type": "text/html"}],
        "attachments": [{"partId": "3", "blobId": "B3", "name": "plan.pdf", "type": "application/pdf",
                         "size": 84000, "disposition": "attachment"}]
    }))
    .unwrap()
}

#[test]
fn bodies_are_stored_and_read_back() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[email("e1").build()]);
    let mut batch = f.store.batch().unwrap();
    assert!(
        batch
            .upsert_body(&body_email("e1", "Swapping Saturday works"), now())
            .unwrap()
    );
    assert!(
        !batch
            .upsert_body(&body_email("missing", "x"), now())
            .unwrap()
    );
    batch.commit().unwrap();

    let detail = f.reader().email("e1").unwrap().unwrap();
    assert!(detail.headers.body_cached);
    let body = detail.body.unwrap();
    assert_eq!(body.text_parts[0].value, "Swapping Saturday works");
    assert_eq!(body.text_parts[0].charset.as_deref(), Some("utf-8"));
    assert!(body.html_parts[0].is_truncated);
    assert_eq!(body.attachments[0].name.as_deref(), Some("plan.pdf"));
    assert_eq!(body.attachments[0].size, 84000);
    assert_eq!(body.headers[0].name, "Subject");
    assert_eq!(body.fetched_at, now());
    assert_eq!(body.opened_at, None);

    assert!(
        f.store
            .touch_body("e1", ts("2026-10-23T15:00:00Z"))
            .unwrap()
    );
    assert!(!f.store.touch_body("missing", now()).unwrap());
    let body = f.reader().email("e1").unwrap().unwrap().body.unwrap();
    assert_eq!(body.opened_at, Some(ts("2026-10-23T15:00:00Z")));

    let hits = f.reader().search("saturday", &Default::default()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    // Refetching replaces the old parts.
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_body(&body_email("e1", "Sunday instead"), now())
        .unwrap();
    batch.commit().unwrap();
    assert!(
        f.reader()
            .search("saturday", &Default::default())
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        f.reader()
            .search("sunday", &Default::default())
            .unwrap()
            .hits
            .len(),
        1
    );
}

#[test]
fn html_only_bodies_are_indexed_as_text() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[email("h1").build()]);
    let html: Email = serde_json::from_value(json!({
        "id": "h1",
        "bodyValues": {"2": {"value": "<style>.x{}</style><p>Track your <b>parcel</b> &amp; more</p>"}},
        "textBody": [],
        "htmlBody": [{"partId": "2", "type": "text/html"}]
    }))
    .unwrap();
    let mut batch = f.store.batch().unwrap();
    batch.upsert_body(&html, now()).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        f.reader()
            .search("parcel", &Default::default())
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(
        f.reader()
            .search("style", &Default::default())
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn body_cache_evicts_least_recently_opened() {
    let mut f = Fixture::with_options(StoreOptions {
        // Each test body is about 2 KB: 1000 bytes of text plus its HTML copy.
        body_cache_bytes: 4500,
        ..StoreOptions::default()
    })
    .with_mailboxes();
    f.apply(&[
        email("b1").build(),
        email("b2").build(),
        email("b3").build(),
        email("b4").build(),
    ]);
    let text = "x".repeat(1000);
    for (i, id) in ["b1", "b2"].iter().enumerate() {
        let mut batch = f.store.batch().unwrap();
        batch
            .upsert_body(
                &body_email(id, &text),
                ts(&format!("2026-10-23T1{i}:00:00Z")),
            )
            .unwrap();
        batch.commit().unwrap();
    }
    // b1 was fetched first but opened last.
    f.store
        .touch_body("b1", ts("2026-10-23T18:00:00Z"))
        .unwrap();
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_body(&body_email("b3", &text), ts("2026-10-23T12:00:00Z"))
        .unwrap();
    batch.commit().unwrap();
    let reader = f.reader();
    let cached = |id: &str| reader.email(id).unwrap().unwrap().headers.body_cached;
    assert!(cached("b1"));
    assert!(!cached("b2"), "least recently opened goes first");
    assert!(cached("b3"));

    // A body larger than the whole cap still stays right after its fetch.
    let big = "y".repeat(10_000);
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_body(&body_email("b4", &big), ts("2026-10-23T13:00:00Z"))
        .unwrap();
    batch.commit().unwrap();
    assert!(cached("b4"));
    assert!(!cached("b1") && !cached("b3"));
    // Evicted bodies leave the index too.
    assert!(
        reader
            .search("xxxx", &Default::default())
            .unwrap()
            .hits
            .is_empty()
    );
    f.check_consistency();
}

#[test]
fn query_cache_and_query_changes() {
    let mut f = Fixture::new();
    let q = |ids: &[&str], position: u64, total: u64| -> QueryResponse {
        serde_json::from_value(json!({
            "accountId": "A1", "queryState": "q-1", "canCalculateChanges": true,
            "position": position, "ids": ids, "total": total
        }))
        .unwrap()
    };
    let mut batch = f.store.batch().unwrap();
    batch
        .save_query("inbox", &q(&["a", "b", "c"], 0, 6), true, now())
        .unwrap();
    batch
        .save_query("inbox", &q(&["d", "e"], 3, 6), true, now())
        .unwrap();
    assert!(
        batch
            .save_query("inbox", &q(&["z"], 9, 6), true, now())
            .is_err()
    );
    assert!(
        batch
            .save_query("other", &q(&["z"], 2, 6), true, now())
            .is_err()
    );
    batch.commit().unwrap();

    let page = f.store.query_page("inbox", 1, 3).unwrap().unwrap();
    assert_eq!(page.ids, ["b", "c", "d"].map(Id::from).to_vec());
    assert_eq!(page.cached, 5);
    assert_eq!(page.total, Some(6));
    assert!(page.collapse_threads);
    assert!(page.can_calculate_changes);

    let changes: QueryChangesResponse = serde_json::from_value(json!({
        "accountId": "A1", "oldQueryState": "q-1", "newQueryState": "q-2",
        "removed": ["b", "d"], "added": [{"id": "n0", "index": 0}, {"id": "c", "index": 1}, {"id": "far", "index": 40}]
    }))
    .unwrap();
    let mut batch = f.store.batch().unwrap();
    assert_eq!(
        batch.apply_query_changes("inbox", &changes, now()).unwrap(),
        QueryChangesOutcome::Applied
    );
    // Applying the same response again is a state mismatch.
    assert_eq!(
        batch.apply_query_changes("inbox", &changes, now()).unwrap(),
        QueryChangesOutcome::StateMismatch
    );
    assert_eq!(
        batch.apply_query_changes("nope", &changes, now()).unwrap(),
        QueryChangesOutcome::NotCached
    );
    batch.commit().unwrap();
    let page = f.reader().query_page("inbox", 0, 100).unwrap().unwrap();
    assert_eq!(page.ids, ["n0", "c", "a", "e"].map(Id::from).to_vec());
    assert_eq!(page.query_state, state("q-2"));
    // 6 - 2 removed + 3 added.
    assert_eq!(page.total, Some(7));

    let mut batch = f.store.batch().unwrap();
    batch.drop_query("inbox").unwrap();
    batch.commit().unwrap();
    assert!(f.reader().query_page("inbox", 0, 1).unwrap().is_none());
}

#[test]
fn unicode_subjects_names_and_search() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("u1")
            .subject("Café au lait ☕ at the 東京タワー")
            .from("Zoë Ångström", "zoe@example.org")
            .build(),
        email("u2")
            .subject("Résumé review 📄")
            .from("Nadia Ünal", "nadia@example.net")
            .preview("naïve façade")
            .build(),
        email("u3").subject("Control\u{2}chars\u{3}inside").build(),
    ]);
    let reader = f.reader();
    let page = reader.conversations(INBOX, 0, 10).unwrap();
    assert!(page.rows.iter().any(|r| r.subject.contains('☕')));
    let detail = reader.email("u1").unwrap().unwrap();
    assert_eq!(
        detail.headers.row.from[0].name.as_deref(),
        Some("Zoë Ångström")
    );

    let opts = Default::default();
    // remove_diacritics: plain letters match accented ones and back.
    assert_eq!(reader.search("cafe", &opts).unwrap().hits.len(), 1);
    assert_eq!(reader.search("résumé", &opts).unwrap().hits.len(), 1);
    assert_eq!(reader.search("resume", &opts).unwrap().hits.len(), 1);
    assert_eq!(reader.search("from:angstrom", &opts).unwrap().hits.len(), 1);
    assert_eq!(reader.search("naive", &opts).unwrap().hits.len(), 1);
    // CJK runs are one token, so they match by prefix as typed.
    assert_eq!(reader.search("東京", &opts).unwrap().hits.len(), 1);
    // Highlight ranges are valid byte ranges in multi-byte text.
    let hit = &reader.search("lait", &opts).unwrap().hits[0];
    let m = hit.subject.matches[0].clone();
    assert_eq!(&hit.subject.text[m], "lait");
    // Control characters cannot fake highlight markers.
    let hit = &reader.search("chars", &opts).unwrap().hits[0];
    assert_eq!(hit.subject.matches.len(), 1);
    assert_eq!(&hit.subject.text[hit.subject.matches[0].clone()], "chars");
    f.check_consistency();
}

#[test]
fn reopen_keeps_cache_and_reader_sees_writer_commits() {
    let mut f = Fixture::new().with_mailboxes();
    let reader = f.reader();
    f.apply(&[email("e1").build()]);
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 1);
    let f = f.reopen();
    assert_eq!(
        f.reader().conversations(INBOX, 0, 10).unwrap().rows.len(),
        1
    );
    f.check_consistency();
}

#[test]
fn readers_work_from_other_threads() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[email("e1").build(), email("e2").build()]);
    let factory = f.store.reader_factory();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let factory = factory.clone();
            std::thread::spawn(move || {
                let reader = factory.open().unwrap();
                reader.conversations(INBOX, 0, 10).unwrap().rows.len()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 2);
    }
}

#[test]
fn pages_join_up_with_tied_timestamps() {
    let mut f = Fixture::new().with_mailboxes();
    let mut emails = Vec::new();
    for i in 0..40 {
        // Every fourth thread shares one timestamp, to test tie order.
        let at = if i % 4 == 0 {
            "2026-10-10T10:00:00Z".to_owned()
        } else {
            format!("2026-10-{:02}T08:{:02}:00Z", 1 + i % 28, i)
        };
        let id = format!("p{i:02}");
        emails.push(email(&id).thread(&format!("tp{i:02}")).at(&at).build());
        if i % 3 == 0 {
            emails.push(
                email(&format!("{id}r"))
                    .thread(&format!("tp{i:02}"))
                    .at(&at)
                    .build(),
            );
        }
    }
    f.apply(&emails);
    let reader = f.reader();
    let whole = reader.conversations(INBOX, 0, 1000).unwrap();
    assert_eq!(whole.total, 40);
    let mut paged = Vec::new();
    let mut offset = 0;
    loop {
        let page = reader.conversations(INBOX, offset, 7).unwrap();
        if page.rows.is_empty() {
            break;
        }
        offset += page.rows.len();
        paged.extend(page.rows);
    }
    assert_eq!(paged, whole.rows);
    assert!(
        whole
            .rows
            .windows(2)
            .all(|w| w[0].received_at >= w[1].received_at)
    );

    let whole = reader.messages(INBOX, 0, 1000).unwrap();
    assert_eq!(whole.total, emails.len() as u64);
    let mut paged = Vec::new();
    for offset in (0..whole.rows.len()).step_by(5) {
        paged.extend(reader.messages(INBOX, offset, 5).unwrap().rows);
    }
    assert_eq!(paged, whole.rows);
    f.check_consistency();
}
