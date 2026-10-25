//! Server copies of drafts: create, replace, discard, send, offline, lost
//! answers, races with deletion, and accounts without a Drafts mailbox.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use mejl_fake::{ExternalChange, FakeServer};
use mejl_jmap::Id;
use mejl_jmap::types::EmailAddress;
use mejl_store::{Draft, DraftHeaders};
use mejl_sync::{Command, LocalWrite, OutgoingMessage, SyncEvent};
use serde_json::{Value, json};

fn quiet_setup() -> Setup {
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    setup
}

fn message(server: &FakeServer, subject: &str) -> OutgoingMessage {
    let r = server.call("Identity/get", json!({"ids": null})).unwrap();
    let first = &r["list"][0];
    OutgoingMessage {
        identity_id: Id::from(first["id"].as_str().unwrap()),
        from: EmailAddress::with_name("Mira Holt", first["email"].as_str().unwrap()),
        to: vec![EmailAddress::with_name(
            "Juniper Tran",
            "juniper@garden.example.com",
        )],
        cc: vec![],
        bcc: vec![],
        reply_to: vec![],
        subject: subject.into(),
        text_body: format!("Body of {subject}"),
        html_body: None,
        in_reply_to: vec![],
        references: vec![],
        attachments: vec![],
        draft_local_id: Some("d1".into()),
    }
}

fn local_draft(id: &str, subject: &str) -> Draft {
    Draft {
        local_id: id.into(),
        server_email_id: None,
        identity_id: None,
        headers: DraftHeaders {
            subject: subject.into(),
            ..Default::default()
        },
        body: format!("Body of {subject}"),
        format: "plain".into(),
        attachments: json!([]),
        created_at: now(),
        updated_at: now(),
    }
}

/// Saves the local draft `d1`, then asks for its server copy, the way the
/// app does.
fn save(h: &Harness, token: u64, subject: &str) {
    h.handle.send(Command::Local {
        token,
        write: LocalWrite::SaveDraft(Box::new(local_draft("d1", subject))),
    });
    h.handle.send(Command::SaveDraftToServer {
        local_id: "d1".into(),
        message: Box::new(message(&h.server, subject)),
    });
}

fn server_copy(h: &Harness) -> Option<Id> {
    h.read(|r| r.draft("d1").unwrap().and_then(|d| d.server_email_id))
}

/// Waits until the local draft records a server copy other than `not`.
fn wait_copy(h: &Harness, not: Option<&Id>) -> Id {
    h.wait_store("a server copy", |r| {
        r.draft("d1")
            .unwrap()
            .and_then(|d| d.server_email_id)
            .is_some_and(|id| Some(&id) != not)
    });
    server_copy(h).expect("a server copy")
}

fn get(server: &FakeServer, id: &str) -> Option<Value> {
    let r = server
        .call(
            "Email/get",
            json!({"ids": [id], "properties": ["mailboxIds", "keywords", "subject", "messageId"]}),
        )
        .unwrap();
    r["list"].as_array().unwrap().first().cloned()
}

/// Draft copies on the server with this Message-ID.
fn copies(server: &FakeServer, message_id: &str) -> Vec<String> {
    let r = server
        .call(
            "Email/query",
            json!({"filter": {"header": ["Message-ID", message_id], "hasKeyword": "$draft"}}),
        )
        .unwrap();
    r["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

/// Every draft copy in the Drafts mailbox whose subject starts with
/// `prefix`, so the demo account's own drafts do not count.
fn drafts_named(h: &Harness, prefix: &str) -> Vec<String> {
    let drafts = h.mailbox("drafts");
    server_ids_in(&h.server, drafts.as_str())
        .into_iter()
        .filter(|id| {
            get(&h.server, id)
                .is_some_and(|e| e["subject"].as_str().is_some_and(|s| s.starts_with(prefix)))
        })
        .collect()
}

fn message_id_of(server: &FakeServer, id: &str) -> String {
    get(server, id).expect("the copy")["messageId"][0]
        .as_str()
        .expect("a Message-ID")
        .to_owned()
}

fn wait_status(h: &Harness, what: &str, pred: impl Fn(&mejl_sync::SyncStatus) -> bool) {
    h.events
        .wait(what, |e| matches!(e, SyncEvent::Status(s) if pred(s)));
}

/// Waits until a draft was pending and then nothing is, online.
fn wait_uploaded(h: &Harness) {
    let (i, _) = h.events.wait_index(
        0,
        "a pending draft",
        |e| matches!(e, SyncEvent::Status(s) if s.drafts_pending > 0),
    );
    h.events.wait_after(
        i,
        "nothing pending",
        |e| matches!(e, SyncEvent::Status(s) if s.online && s.drafts_pending == 0),
    );
}

#[test]
fn a_saved_draft_gets_a_copy_in_drafts() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let drafts = h.mailbox("drafts");
    save(&h, 1, "Plot rota");
    let id = wait_copy(&h, None);
    let email = get(&h.server, id.as_str()).expect("on the server");
    assert_eq!(email["mailboxIds"], json!({drafts.as_str(): true}));
    assert_eq!(email["keywords"], json!({"$draft": true, "$seen": true}));
    assert_eq!(email["subject"], "Plot rota");
    wait_uploaded(&h);
    assert!(!h.events.last_status().unwrap().drafts_local_only);
    // The copy reaches the cache as an ordinary draft.
    h.wait_store("copy cached", |r| {
        r.email(id.as_str())
            .unwrap()
            .is_some_and(|d| d.headers.row.draft)
    });
    h.shutdown();
}

#[test]
fn an_update_replaces_the_old_copy() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    save(&h, 1, "Plot rota");
    let first = wait_copy(&h, None);
    let message_id = message_id_of(&h.server, first.as_str());
    save(&h, 2, "Plot rota, take two");
    let second = wait_copy(&h, Some(&first));
    assert!(
        get(&h.server, first.as_str()).is_none(),
        "the old copy is gone"
    );
    let email = get(&h.server, second.as_str()).expect("the new copy");
    assert_eq!(email["subject"], "Plot rota, take two");
    assert_eq!(email["keywords"], json!({"$draft": true, "$seen": true}));
    // Every copy of the draft carries one Message-ID, and only one exists.
    assert_eq!(message_id_of(&h.server, second.as_str()), message_id);
    assert_eq!(copies(&h.server, &message_id), vec![second.to_string()]);
    // Replacing is one request: create and destroy in the same call.
    let replaced = h
        .transport
        .bodies
        .lock()
        .unwrap()
        .iter()
        .flat_map(|b| b["methodCalls"].as_array().cloned().unwrap_or_default())
        .any(|c| {
            c[0] == "Email/set"
                && c[1]["create"].is_object()
                && c[1]["destroy"] == json!([first.as_str()])
        });
    assert!(replaced);
    h.shutdown();
}

#[test]
fn rapid_saves_end_in_one_copy_of_the_newest_version() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    for i in 0..12 {
        save(&h, i, &format!("Rota v{i}"));
    }
    h.wait_server("the newest version", |_| {
        drafts_named(&h, "Rota v")
            .iter()
            .filter_map(|id| get(&h.server, id))
            .any(|e| e["subject"] == "Rota v11")
    });
    wait_uploaded(&h);
    assert_eq!(drafts_named(&h, "Rota v").len(), 1, "one copy");
    let uploads = h
        .transport
        .bodies
        .lock()
        .unwrap()
        .iter()
        .flat_map(|b| b["methodCalls"].as_array().cloned().unwrap_or_default())
        .filter(|c| c[0] == "Email/set" && c[1]["create"].is_object())
        .count();
    assert!(uploads < 12, "saves coalesce: {uploads} uploads");
    h.shutdown();
}

#[test]
fn discarding_a_draft_destroys_its_copy() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    save(&h, 1, "Never mind");
    let id = wait_copy(&h, None);
    let message_id = message_id_of(&h.server, id.as_str());
    h.handle.send(Command::Local {
        token: 2,
        write: LocalWrite::DeleteDraft("d1".into()),
    });
    h.wait_server("copy destroyed", |s| get(s, id.as_str()).is_none());
    assert!(copies(&h.server, &message_id).is_empty());
    assert!(h.read(|r| r.draft("d1").unwrap().is_none()));
    h.wait_store("copy gone from the cache", |r| {
        r.email(id.as_str()).unwrap().is_none()
    });
    h.shutdown();
}

#[test]
fn sending_a_draft_leaves_nothing_in_drafts() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let sent = h.mailbox("sent");
    save(&h, 1, "Seedlings");
    let copy = wait_copy(&h, None);
    let draft_mid = message_id_of(&h.server, copy.as_str());
    h.handle.send(Command::Send {
        token: 2,
        message: Box::new(message(&h.server, "Seedlings")),
        undo_window: Duration::from_millis(100),
    });
    let e = h
        .events
        .wait("sent", |e| matches!(e, SyncEvent::Sent { .. }));
    let SyncEvent::Sent {
        message_id,
        email_id,
        ..
    } = e
    else {
        panic!("expected sent");
    };
    h.wait_server("draft copy destroyed", |s| get(s, copy.as_str()).is_none());
    assert!(copies(&h.server, &draft_mid).is_empty());
    assert!(
        drafts_named(&h, "Seedlings").is_empty(),
        "nothing in Drafts"
    );
    let sent_copy = get(&h.server, email_id.expect("sent id").as_str()).expect("sent");
    assert_eq!(sent_copy["mailboxIds"], json!({sent.as_str(): true}));
    assert_eq!(sent_copy["messageId"], json!([message_id]));
    let in_sent: Vec<String> = server_ids_in(&h.server, sent.as_str())
        .into_iter()
        .filter(|id| get(&h.server, id).is_some_and(|e| e["subject"] == "Seedlings"))
        .collect();
    assert_eq!(in_sent.len(), 1, "exactly one sent copy");
    assert!(h.read(|r| r.draft("d1").unwrap().is_none()));
    h.shutdown();
}

#[test]
fn a_send_held_while_a_copy_uploads_leaves_no_orphan() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    save(&h, 1, "Race v1");
    let first = wait_copy(&h, None);
    // The next upload is answered late, so it lands after the send.
    *h.transport.hold.lock().unwrap() = Some("Email/set".into());
    save(&h, 2, "Race v2");
    h.wait_server("held upload sent", |_| {
        h.transport.holding.load(Ordering::SeqCst)
    });
    h.handle.send(Command::Send {
        token: 3,
        message: Box::new(message(&h.server, "Race v2")),
        undo_window: Duration::ZERO,
    });
    std::thread::sleep(Duration::from_millis(100));
    h.transport.release();
    h.events
        .wait("sent", |e| matches!(e, SyncEvent::Sent { .. }));
    h.wait_server("no draft copy left", |_| {
        drafts_named(&h, "Race").is_empty()
    });
    assert!(get(&h.server, first.as_str()).is_none());
    h.shutdown();
}

#[test]
fn a_draft_deleted_during_an_upload_leaves_no_copy() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    *h.transport.hold.lock().unwrap() = Some("Email/set".into());
    save(&h, 1, "Short lived");
    h.wait_server("upload sent", |_| {
        h.transport.holding.load(Ordering::SeqCst)
    });
    // The server made the copy, but its answer has not arrived.
    assert_eq!(drafts_named(&h, "Short lived").len(), 1);
    h.handle.send(Command::Local {
        token: 2,
        write: LocalWrite::DeleteDraft("d1".into()),
    });
    h.events.wait("deleted", |e| {
        matches!(e, SyncEvent::LocalDone { token: 2, .. })
    });
    h.transport.release();
    h.wait_server("the late copy is destroyed", |_| {
        drafts_named(&h, "Short lived").is_empty()
    });
    h.shutdown();
}

#[test]
fn offline_saves_upload_when_the_connection_returns() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let mark = h.events.mark();
    h.transport.down.store(true, Ordering::SeqCst);
    save(&h, 1, "Offline rota");
    h.events.wait_after(
        mark,
        "offline",
        |e| matches!(e, SyncEvent::Status(s) if !s.online),
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(drafts_named(&h, "Offline rota").is_empty());
    assert_eq!(h.events.last_status().unwrap().drafts_pending, 1);
    // A newer version written offline is the one that goes up.
    save(&h, 2, "Offline rota, final");
    std::thread::sleep(Duration::from_millis(50));
    h.transport.down.store(false, Ordering::SeqCst);
    let id = wait_copy(&h, None);
    assert_eq!(
        get(&h.server, id.as_str()).unwrap()["subject"],
        "Offline rota, final"
    );
    wait_uploaded(&h);
    assert_eq!(drafts_named(&h, "Offline rota").len(), 1);
    h.shutdown();
}

#[test]
fn a_queued_copy_survives_a_restart() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.transport.down.store(true, Ordering::SeqCst);
    save(&h, 1, "After the restart");
    wait_status(&h, "pending", |s| s.drafts_pending == 1);
    let (server, dir) = h.stop();
    let h = Harness::start(
        server,
        Setup {
            dir: Some(dir),
            ..quiet_setup()
        },
    );
    let id = wait_copy(&h, None);
    assert_eq!(
        get(&h.server, id.as_str()).unwrap()["subject"],
        "After the restart"
    );
    assert_eq!(drafts_named(&h, "After the restart").len(), 1);
    h.shutdown();
}

#[test]
fn a_lost_answer_leaves_no_stray_copy() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.server.quirks_mut().lose_api_response_next = 1;
    save(&h, 1, "Lost once");
    let id = wait_copy(&h, None);
    let message_id = message_id_of(&h.server, id.as_str());
    // The first upload made a copy whose answer was lost; the retry found
    // it by Message-ID and destroyed it.
    h.wait_server("one copy", |s| copies(s, &message_id).len() == 1);
    assert_eq!(copies(&h.server, &message_id), vec![id.to_string()]);
    assert_eq!(drafts_named(&h, "Lost once").len(), 1);
    let creates = h
        .transport
        .bodies
        .lock()
        .unwrap()
        .iter()
        .flat_map(|b| b["methodCalls"].as_array().cloned().unwrap_or_default())
        .filter(|c| c[0] == "Email/set" && c[1]["create"].is_object())
        .count();
    assert_eq!(creates, 2, "the lost upload and its retry");
    assert!(
        h.transport.calls_named("Email/query") >= 1,
        "the retry looked first"
    );
    h.shutdown();
}

#[test]
fn without_a_drafts_mailbox_drafts_stay_local() {
    let server = FakeServer::demo(now());
    let drafts = server.mailbox_id("drafts").expect("a Drafts mailbox");
    server
        .mutate_from_elsewhere(ExternalChange::ClearMailboxRole {
            mailbox_id: drafts.clone(),
        })
        .unwrap();
    let before = server_ids_in(&server, &drafts).len();
    let h = Harness::start(server, quiet_setup());
    h.synced();
    save(&h, 1, "Stays here");
    wait_status(&h, "local only", |s| {
        s.drafts_local_only && s.drafts_pending == 1
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(server_ids_in(&h.server, &drafts).len(), before);
    assert_eq!(server_copy(&h), None);
    assert!(h.read(|r| r.draft("d1").unwrap().is_some()), "kept locally");
    h.shutdown();
}

#[test]
fn a_send_that_fails_for_good_leaves_no_extra_copy() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    save(&h, 1, "Bad identity");
    let copy = wait_copy(&h, None);
    let mut m = message(&h.server, "Bad identity");
    m.identity_id = Id::from("no-such-identity");
    h.handle.send(Command::Send {
        token: 2,
        message: Box::new(m),
        undo_window: Duration::ZERO,
    });
    h.events.wait("send failed", |e| {
        matches!(e, SyncEvent::OutboxFailed { .. })
    });
    // The copy the send made is destroyed; the draft and its copy remain.
    h.wait_server("only the draft's copy", |_| {
        drafts_named(&h, "Bad identity") == vec![copy.to_string()]
    });
    assert!(h.read(|r| r.draft("d1").unwrap().is_some()));
    h.shutdown();
}

#[test]
fn a_save_for_a_draft_that_is_not_saved_locally_is_ignored() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.handle.send(Command::SaveDraftToServer {
        local_id: "unknown".into(),
        message: Box::new(message(&h.server, "Nowhere")),
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(drafts_named(&h, "Nowhere").is_empty());
    assert_eq!(h.events.last_status().unwrap().drafts_pending, 0);
    h.shutdown();
}

#[test]
fn a_copy_sent_from_elsewhere_is_never_destroyed() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let sent = h.mailbox("sent");
    save(&h, 1, "Sent from the web");
    let first = wait_copy(&h, None);
    // The user sends the server copy from another client.
    h.server
        .mutate_from_elsewhere(ExternalChange::SetKeyword {
            email_id: first.to_string(),
            keyword: "$draft".into(),
            value: false,
        })
        .unwrap();
    h.server
        .mutate_from_elsewhere(ExternalChange::MoveToMailbox {
            email_id: first.to_string(),
            mailbox_id: sent.to_string(),
        })
        .unwrap();
    h.handle.send(Command::RefreshNow);
    h.wait_store("the cache sees it sent", |r| {
        r.email(first.as_str())
            .unwrap()
            .is_some_and(|d| !d.headers.row.draft)
    });
    // Editing on here makes a new copy and leaves the sent one alone.
    save(&h, 2, "Sent from the web, edited");
    let second = wait_copy(&h, Some(&first));
    assert!(
        get(&h.server, first.as_str()).is_some(),
        "the sent mail stays"
    );
    assert!(get(&h.server, second.as_str()).is_some());
    // Discarding the draft destroys its copies by Message-ID, which the
    // sent mail shares; only drafts go.
    h.handle.send(Command::Local {
        token: 3,
        write: LocalWrite::DeleteDraft("d1".into()),
    });
    h.wait_server("the new copy is destroyed", |s| {
        get(s, second.as_str()).is_none()
    });
    let mid = message_id_of(&h.server, first.as_str());
    assert!(copies(&h.server, &mid).is_empty());
    assert_eq!(
        get(&h.server, first.as_str()).unwrap()["mailboxIds"],
        json!({sent.as_str(): true})
    );
    h.shutdown();
}

#[test]
fn the_trash_only_destroys_drafts() {
    let server = FakeServer::demo(now());
    let sent = server.mailbox_id("sent").unwrap();
    let drafts = server.mailbox_id("drafts").unwrap();
    let make = |mailbox: &str, keywords: Value| {
        let r = server
            .call(
                "Email/set",
                json!({"create": {"x": {
                    "mailboxIds": {mailbox: true},
                    "keywords": keywords,
                    "subject": "Trash test",
                    "messageId": ["trash@example.org"],
                }}}),
            )
            .unwrap();
        Id::from(r["created"]["x"]["id"].as_str().unwrap())
    };
    let mail = make(&sent, json!({"$seen": true}));
    let draft = make(&drafts, json!({"$seen": true, "$draft": true}));
    let mut setup = quiet_setup();
    let ids = vec![mail.clone(), draft.clone(), Id::from("gone-already")];
    setup.before_start = Some(Box::new(move |store| {
        store
            .trash_draft_copies(&ids, &["trash@example.org"], now())
            .unwrap();
    }));
    let h = Harness::start(server, setup);
    h.wait_server("the draft is destroyed", |s| {
        get(s, draft.as_str()).is_none()
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(get(&h.server, mail.as_str()).is_some(), "sent mail is kept");
    h.shutdown();
}

#[test]
fn the_pending_count_moves_while_offline() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let mark = h.events.mark();
    h.transport.down.store(true, Ordering::SeqCst);
    h.handle.send(Command::RefreshNow);
    let (offline, _) = h.events.wait_index(
        mark,
        "offline",
        |e| matches!(e, SyncEvent::Status(s) if !s.online),
    );
    // The worker does not run offline; the count still shows the save.
    save(&h, 1, "Written offline");
    h.events.wait_after(
        offline,
        "one pending",
        |e| matches!(e, SyncEvent::Status(s) if !s.online && s.drafts_pending == 1),
    );
    // And the delete.
    h.handle.send(Command::Local {
        token: 2,
        write: LocalWrite::DeleteDraft("d1".into()),
    });
    h.events.wait_after(
        offline,
        "none pending",
        |e| matches!(e, SyncEvent::Status(s) if !s.online && s.drafts_pending == 0),
    );
    // A restart offline counts what the last run left.
    save(&h, 3, "Left for later");
    let (server, dir) = h.stop();
    let mut setup = quiet_setup();
    setup.dir = Some(dir);
    let down = TestTransport::new(Arc::new(server.transport()));
    down.down.store(true, Ordering::SeqCst);
    setup.transport = Some((down, server.session_url()));
    let h = Harness::start(server, setup);
    h.events.wait(
        "counted at start",
        |e| matches!(e, SyncEvent::Status(s) if !s.online && s.drafts_pending == 1),
    );
    h.shutdown();
}

#[test]
fn a_cleanup_the_server_refuses_does_not_block_uploads() {
    let mut setup = quiet_setup();
    setup.before_start = Some(Box::new(|store| {
        store
            .trash_draft_copies(&[], &["refused@example.org"], now())
            .unwrap();
    }));
    // The lookup of that copy is refused for good.
    setup.refuse = Some(("refused@example.org".into(), 400));
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    save(&h, 1, "Still uploads");
    let id = wait_copy(&h, None);
    assert!(get(&h.server, id.as_str()).is_some());
    let lookups = h
        .transport
        .bodies
        .lock()
        .unwrap()
        .iter()
        .filter(|b| b.to_string().contains("refused@example.org"))
        .count();
    assert_eq!(lookups, 1, "the refused cleanup was given up, not retried");
    h.shutdown();
}
