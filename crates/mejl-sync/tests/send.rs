//! Sending: the undo hold, cancel, send now, attachments, and never sending
//! twice after a crash or a lost response.

mod common;

use std::time::Duration;

use common::*;
use mejl_fake::FakeServer;
use mejl_jmap::Id;
use mejl_jmap::types::EmailAddress;
use mejl_store::{Draft, DraftHeaders, StoreOptions, UndoOutcome};
use mejl_sync::{Command, OutgoingAttachment, OutgoingMessage, SyncEvent, decode_send_payload};
use serde_json::{Value, json};

fn quiet_setup() -> Setup {
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    setup
}

fn identity(server: &FakeServer) -> (Id, String) {
    let r = server.call("Identity/get", json!({"ids": null})).unwrap();
    let first = &r["list"][0];
    (
        Id::from(first["id"].as_str().unwrap()),
        first["email"].as_str().unwrap().to_owned(),
    )
}

fn message(server: &FakeServer, subject: &str) -> OutgoingMessage {
    let (identity_id, from) = identity(server);
    OutgoingMessage {
        identity_id,
        from: EmailAddress::with_name("Mira Holt", from),
        to: vec![EmailAddress::with_name(
            "Juniper Tran",
            "juniper@garden.example.com",
        )],
        cc: vec![],
        bcc: vec![],
        reply_to: vec![],
        subject: subject.into(),
        text_body: "Tuesday works for me.".into(),
        html_body: None,
        in_reply_to: vec![],
        references: vec![],
        attachments: vec![],
        draft_local_id: None,
    }
}

fn by_message_id(server: &FakeServer, message_id: &str) -> Vec<Value> {
    let ids = server
        .call(
            "Email/query",
            json!({"filter": {"header": ["Message-ID", message_id]}}),
        )
        .unwrap()["ids"]
        .clone();
    server
        .call(
            "Email/get",
            json!({"ids": ids, "properties": ["mailboxIds", "keywords", "messageId", "hasAttachment", "subject"]}),
        )
        .unwrap()["list"]
        .as_array()
        .unwrap()
        .clone()
}

fn submissions(server: &FakeServer) -> usize {
    server
        .call("EmailSubmission/get", json!({"ids": null}))
        .unwrap()["list"]
        .as_array()
        .unwrap()
        .len()
}

fn queued(h: &Harness, token: u64) -> (mejl_store::Seq, String) {
    match h.events.wait(
        "queued",
        |e| matches!(e, SyncEvent::SendQueued { token: t, .. } if *t == token),
    ) {
        SyncEvent::SendQueued {
            seq, message_id, ..
        } => (seq, message_id),
        other => panic!("expected a queued send, got {other:?}"),
    }
}

#[test]
fn a_cancelled_send_never_reaches_the_server() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.handle.send(Command::Send {
        token: 1,
        message: Box::new(message(&h.server, "Never sent")),
        undo_window: Duration::from_secs(5),
    });
    let (seq, message_id) = queued(&h, 1);
    assert!(message_id.ends_with("@example.org"), "{message_id}");
    h.events.wait(
        "held",
        |e| matches!(e, SyncEvent::Status(s) if s.held_sends == 1),
    );
    h.handle.send(Command::CancelSend { token: 2, seq });
    let cancelled = h.events.wait("cancelled", |e| {
        matches!(e, SyncEvent::SendCancelled { token: 2, .. })
    });
    let SyncEvent::SendCancelled { message, .. } = cancelled else {
        panic!("expected a cancel");
    };
    assert_eq!(
        message.expect("the message comes back").subject,
        "Never sent"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(by_message_id(&h.server, &message_id).is_empty());
    assert_eq!(submissions(&h.server), 0);
    // Too late now.
    h.handle.send(Command::CancelSend { token: 3, seq });
    assert!(matches!(
        h.events.wait("too late", |e| matches!(
            e,
            SyncEvent::SendCancelled { token: 3, .. }
        )),
        SyncEvent::SendCancelled { message: None, .. }
    ));
    h.shutdown();
}

#[test]
fn a_send_after_its_hold_lands_in_sent() {
    let mut setup = quiet_setup();
    setup.before_start = Some(Box::new(|store| {
        store
            .save_draft(&Draft {
                local_id: "d1".into(),
                server_email_id: None,
                identity_id: None,
                headers: DraftHeaders::default(),
                body: "Tuesday works for me.".into(),
                format: "plain".into(),
                attachments: json!([]),
                created_at: now(),
                updated_at: now(),
            })
            .unwrap();
    }));
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    let sent = h.mailbox("sent");
    let drafts = h.mailbox("drafts");
    let mut m = message(&h.server, "Rota for Tuesday");
    m.draft_local_id = Some("d1".into());
    m.in_reply_to = vec!["parent@garden.example.com".into()];
    h.handle.send(Command::Send {
        token: 4,
        message: Box::new(m),
        undo_window: Duration::from_millis(150),
    });
    let (seq, message_id) = queued(&h, 4);
    let e = h.events.wait(
        "sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    let SyncEvent::Sent {
        message_id: sent_id,
        email_id,
        ..
    } = e
    else {
        panic!("expected sent");
    };
    assert_eq!(sent_id, message_id);
    let found = by_message_id(&h.server, &message_id);
    assert_eq!(found.len(), 1, "{found:?}");
    let email = &found[0];
    assert_eq!(email["id"].as_str(), email_id.as_ref().map(Id::as_str));
    assert_eq!(email["mailboxIds"][sent.as_str()], true);
    assert!(email["mailboxIds"].get(drafts.as_str()).is_none());
    assert!(email["keywords"].get("$draft").is_none());
    assert_eq!(email["messageId"], json!([message_id]));
    assert_eq!(submissions(&h.server), 1);
    // The local draft is gone, and the sent copy reaches the cache.
    h.wait_store("draft deleted", |r| r.draft("d1").unwrap().is_none());
    h.wait_store("sent copy cached", |r| {
        r.conversations(sent.as_str(), 0, 500)
            .unwrap()
            .rows
            .iter()
            .any(|c| Some(&c.email_id) == email_id.as_ref())
    });
    h.events.wait(
        "outbox empty",
        |e| matches!(e, SyncEvent::Status(s) if s.pending == 0 && s.held_sends == 0),
    );
    h.shutdown();
}

#[test]
fn send_now_skips_the_rest_of_the_hold() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.handle.send(Command::Send {
        token: 5,
        message: Box::new(message(&h.server, "Right away")),
        undo_window: Duration::from_secs(600),
    });
    let (seq, message_id) = queued(&h, 5);
    std::thread::sleep(Duration::from_millis(100));
    assert!(by_message_id(&h.server, &message_id).is_empty());
    h.handle.send(Command::SendNow { seq });
    h.events.wait(
        "sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    assert_eq!(by_message_id(&h.server, &message_id).len(), 1);
    h.shutdown();
}

#[test]
fn undo_cancels_a_held_send_and_returns_it() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.handle.send(Command::Send {
        token: 6,
        message: Box::new(message(&h.server, "Oops")),
        undo_window: Duration::from_secs(600),
    });
    let (_, message_id) = queued(&h, 6);
    h.handle.send(Command::Undo { token: 7 });
    let undone = h.events.wait("undone", |e| {
        matches!(e, SyncEvent::Undone { token: 7, .. })
    });
    let SyncEvent::Undone {
        outcome:
            Some(UndoOutcome::Cancelled {
                payload: Some(payload),
                ..
            }),
        ..
    } = undone
    else {
        panic!("expected a cancelled send, got {undone:?}");
    };
    assert_eq!(decode_send_payload(&payload).unwrap().subject, "Oops");
    std::thread::sleep(Duration::from_millis(200));
    assert!(by_message_id(&h.server, &message_id).is_empty());
    h.shutdown();
}

#[test]
fn sends_without_recipients_are_refused() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    let mut m = message(&h.server, "Nobody");
    m.to.clear();
    h.handle.send(Command::Send {
        token: 8,
        message: Box::new(m),
        undo_window: Duration::ZERO,
    });
    h.events.wait("refused", |e| {
        matches!(e, SyncEvent::Rejected { token: 8, .. })
    });
    h.shutdown();
}

#[test]
fn a_lost_response_does_not_send_twice() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.server.quirks_mut().lose_api_response_next = 1;
    h.handle.send(Command::Send {
        token: 9,
        message: Box::new(message(&h.server, "Exactly once")),
        undo_window: Duration::ZERO,
    });
    let (seq, message_id) = queued(&h, 9);
    h.events.wait(
        "sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    // The first attempt went through although its answer was lost; the
    // retry found it by Message-ID.
    assert_eq!(by_message_id(&h.server, &message_id).len(), 1);
    assert_eq!(submissions(&h.server), 1);
    assert!(h.transport.calls_named("Email/query") >= 1);
    h.shutdown();
}

#[test]
fn an_interrupted_send_is_not_duplicated() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let sent_box = h.mailbox("sent");
    let m = message(&h.server, "Sent before the crash");
    h.handle.send(Command::Send {
        token: 10,
        message: Box::new(m.clone()),
        undo_window: Duration::from_secs(600),
    });
    let (seq, message_id) = queued(&h, 10);
    let (server, dir) = h.stop();

    // Simulate a crash mid-send: the entry goes in flight, the message
    // reaches the server, and mejl dies before recording the result.
    {
        let mut store =
            mejl_store::Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        assert!(store.send_now(seq).unwrap());
        let unit = store.next_flush(now(), 100).unwrap().expect("the send");
        assert!(!unit.interrupted);
        server
            .call(
                "Email/set",
                json!({"create": {"x": {
                    "mailboxIds": {sent_box.as_str(): true},
                    "keywords": {"$seen": true},
                    "from": [{"name": "Mira Holt", "email": m.from.email}],
                    "to": [{"email": "juniper@garden.example.com"}],
                    "subject": "Sent before the crash",
                    "messageId": [message_id],
                }}}),
            )
            .unwrap();
    }
    let h = Harness::start(
        server,
        Setup {
            dir: Some(dir),
            ..quiet_setup()
        },
    );
    h.events.wait(
        "recorded as sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    assert_eq!(by_message_id(&h.server, &message_id).len(), 1);
    assert_eq!(submissions(&h.server), 0, "no second submission");
    h.shutdown();
}

#[test]
fn an_interrupted_draft_is_submitted_not_recreated() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let drafts = h.mailbox("drafts");
    let sent_box = h.mailbox("sent");
    let m = message(&h.server, "Created but not sent");
    h.handle.send(Command::Send {
        token: 11,
        message: Box::new(m.clone()),
        undo_window: Duration::from_secs(600),
    });
    let (seq, message_id) = queued(&h, 11);
    let (server, dir) = h.stop();
    {
        let mut store =
            mejl_store::Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        store.send_now(seq).unwrap();
        store.next_flush(now(), 100).unwrap().expect("the send");
        // Only the create landed.
        server
            .call(
                "Email/set",
                json!({"create": {"x": {
                    "mailboxIds": {drafts.as_str(): true},
                    "keywords": {"$seen": true, "$draft": true},
                    "from": [{"name": "Mira Holt", "email": m.from.email}],
                    "to": [{"email": "juniper@garden.example.com"}],
                    "subject": "Created but not sent",
                    "messageId": [message_id],
                }}}),
            )
            .unwrap();
    }
    let h = Harness::start(
        server,
        Setup {
            dir: Some(dir),
            ..quiet_setup()
        },
    );
    h.events.wait(
        "sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    let found = by_message_id(&h.server, &message_id);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["mailboxIds"][sent_box.as_str()], true);
    assert!(found[0]["keywords"].get("$draft").is_none());
    assert_eq!(submissions(&h.server), 1);
    h.shutdown();
}

#[test]
fn uploaded_attachments_go_out_with_the_message() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let file = h.dir.path().join("rota.txt");
    std::fs::write(&file, b"Mon: Juniper\nTue: Owen\n").unwrap();
    h.handle.send(Command::UploadAttachment {
        token: 12,
        path: file,
        content_type: None,
    });
    let up = h.events.wait("uploaded", |e| {
        matches!(e, SyncEvent::Uploaded { token: 12, .. })
    });
    let SyncEvent::Uploaded {
        blob_id,
        name,
        content_type,
        size,
        ..
    } = up
    else {
        panic!("expected an upload");
    };
    assert_eq!(name, "rota.txt");
    assert_eq!(content_type, "text/plain");
    assert_eq!(size, 23);
    let mut m = message(&h.server, "Rota attached");
    m.attachments = vec![OutgoingAttachment {
        blob_id,
        name,
        content_type,
    }];
    h.handle.send(Command::Send {
        token: 13,
        message: Box::new(m),
        undo_window: Duration::ZERO,
    });
    let (seq, message_id) = queued(&h, 13);
    h.events.wait(
        "sent",
        |e| matches!(e, SyncEvent::Sent { seq: s, .. } if *s == seq),
    );
    let found = by_message_id(&h.server, &message_id);
    assert_eq!(found[0]["hasAttachment"], true);

    // A missing file fails cleanly.
    h.handle.send(Command::UploadAttachment {
        token: 14,
        path: h.dir.path().join("missing.pdf"),
        content_type: None,
    });
    h.events.wait("upload failed", |e| {
        matches!(e, SyncEvent::UploadFailed { token: 14, .. })
    });
    h.shutdown();
}
