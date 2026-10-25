//! Cold and warm sync, push-driven updates, resync, load-more, backoff,
//! auth failure and offline recovery, through the in-process fake.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use mejl_fake::{Address, ExternalChange, FakeServer, IncomingMessage};
use mejl_jmap::{Id, Patch, Secret};
use mejl_store::{EmailPatchTarget, Mutation, type_name};
use mejl_sync::{Command, PushState, SyncEvent, mailbox_query_key};
use serde_json::json;

fn mail(subject: &str) -> IncomingMessage {
    IncomingMessage::new(Address::new("Ada Abara", "ada@example.com"), subject).text("hello there")
}

#[test]
fn cold_sync_fills_the_store() {
    let h = Harness::demo();
    h.synced();
    let inbox = h.mailbox("inbox");

    // Mailboxes, with the roles the UI needs.
    let mailboxes = h.read(|r| r.mailboxes().unwrap());
    assert!(mailboxes.iter().any(|m| m.id == inbox));
    assert!(mailboxes.iter().any(|m| m.name == "Garden club"));

    // The Inbox page matches the server's conversation count.
    let server_total = h
        .server
        .call(
            "Email/query",
            json!({"filter": {"inMailbox": inbox.as_str()}, "collapseThreads": true, "calculateTotal": true}),
        )
        .unwrap()["total"]
        .as_u64()
        .unwrap();
    let page = h.read(|r| r.conversations(inbox.as_str(), 0, 500).unwrap());
    assert_eq!(page.total, server_total);
    assert!(!page.rows.is_empty());
    let window = h.read(|r| r.query_page(&mailbox_query_key(&inbox), 0, 0).unwrap());
    assert_eq!(window.map(|w| w.cached as u64), Some(server_total));
    assert!(h.events.any(|e| matches!(
        e,
        SyncEvent::WindowLoaded { mailbox, .. } if *mailbox == inbox
    )));

    // A thread with all of its messages.
    let row = page.rows.iter().max_by_key(|r| r.thread_count).unwrap();
    let thread = h.read(|r| r.conversation(row.thread_id.as_str()).unwrap());
    assert_eq!(thread.len(), row.thread_count as usize);

    // Identities, contacts and masks.
    assert_eq!(h.read(|r| r.identities().unwrap().len()), 3);
    assert_eq!(h.read(|r| r.masked_emails().unwrap().len()), 20);
    let juniper = h.read(|r| r.address_suggestions("juniper", 10).unwrap());
    assert!(
        juniper.iter().any(|s| s.contact_id.is_some()),
        "{juniper:?}"
    );
    for ty in [
        type_name::MAILBOX,
        type_name::EMAIL,
        type_name::THREAD,
        type_name::IDENTITY,
        type_name::CONTACT_CARD,
    ] {
        assert!(
            h.read(|r| r.type_state(ty).unwrap()).is_some(),
            "{ty} state"
        );
    }

    // A body on demand.
    let email = row.email_id.clone();
    h.handle.send(Command::FetchBody {
        email_id: email.clone(),
    });
    h.events.wait(
        "body",
        |e| matches!(e, SyncEvent::BodyReady { email_id } if *email_id == email),
    );
    let detail = h.read(|r| r.email(email.as_str()).unwrap().unwrap());
    let body = detail.body.expect("body cached");
    assert!(!body.text_parts.is_empty() || !body.html_parts.is_empty());

    // Asking again needs no request.
    let before = h.transport.sends.load(Ordering::SeqCst);
    let mark = h.events.mark();
    h.handle.send(Command::FetchBody {
        email_id: email.clone(),
    });
    h.events.wait_after(
        mark,
        "cached body",
        |e| matches!(e, SyncEvent::BodyReady { email_id } if *email_id == email),
    );
    assert_eq!(h.transport.sends.load(Ordering::SeqCst), before);
    h.shutdown();
}

#[test]
fn push_drives_warm_sync() {
    let h = Harness::demo();
    h.synced();
    h.events.wait(
        "push connected",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Connected),
    );
    let inbox = h.mailbox("inbox");
    let mark = h.events.mark();
    let id = h.server.deliver_incoming(mail("Fresh news")).unwrap();
    let id = Id::from(id);
    h.events.wait_after(mark, "data change for the new mail", |e| {
        matches!(e, SyncEvent::DataChanged(c) if c.emails.contains(&id) && c.mailbox_lists.contains(&inbox))
    });
    h.wait_store("new mail in the Inbox", |r| {
        r.conversations(inbox.as_str(), 0, 5)
            .unwrap()
            .rows
            .iter()
            .any(|row| row.email_id == id)
    });

    // A change made in another client arrives too.
    h.server
        .mutate_from_elsewhere(ExternalChange::SetKeyword {
            email_id: id.to_string(),
            keyword: "$flagged".into(),
            value: true,
        })
        .unwrap();
    h.wait_store("flag from elsewhere", |r| {
        r.email(id.as_str())
            .unwrap()
            .is_some_and(|d| d.headers.row.flagged)
    });
    h.shutdown();
}

#[test]
fn expired_change_logs_lead_to_a_resync() {
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    let state_before = h.read(|r| r.type_state(type_name::EMAIL).unwrap());
    h.server.expire_change_logs();
    let id = Id::from(h.server.deliver_incoming(mail("After the purge")).unwrap());
    // Destroy one cached email so the resync must notice a removal too.
    let inbox = h.mailbox("inbox");
    let gone = h.read(|r| {
        r.conversations(inbox.as_str(), 0, 50).unwrap().rows[5]
            .email_id
            .clone()
    });
    h.server
        .mutate_from_elsewhere(ExternalChange::DestroyEmail {
            email_id: gone.to_string(),
        })
        .unwrap();
    let mark = h.events.mark();
    h.handle.send(Command::RefreshNow);
    h.synced_after(mark);
    h.wait_store("new mail after resync", |r| {
        r.email(id.as_str()).unwrap().is_some()
    });
    h.wait_store("destroyed mail gone", |r| {
        r.email(gone.as_str()).unwrap().is_none()
    });
    assert_ne!(
        h.read(|r| r.type_state(type_name::EMAIL).unwrap()),
        state_before
    );
    // The next pass uses /changes again.
    let calls = h.transport.calls_named("Email/changes");
    let mark = h.events.mark();
    h.server.deliver_incoming(mail("Back to normal")).unwrap();
    h.handle.send(Command::RefreshNow);
    h.synced_after(mark);
    assert!(h.transport.calls_named("Email/changes") > calls);
    h.shutdown();
}

#[test]
fn load_more_extends_the_window() {
    let mut setup = Setup::default();
    setup.config.window = 5;
    setup.config.load_more = 4;
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    let inbox = h.mailbox("inbox");
    let key = mailbox_query_key(&inbox);
    assert_eq!(
        h.read(|r| r.query_page(&key, 0, 0).unwrap().unwrap().cached),
        5
    );
    let local_before = h.read(|r| r.conversations(inbox.as_str(), 0, 0).unwrap().total);
    let mark = h.events.mark();
    h.handle.send(Command::LoadMore {
        mailbox: inbox.clone(),
    });
    let e = h.events.wait_after(
        mark,
        "window loaded",
        |e| matches!(e, SyncEvent::WindowLoaded { mailbox, .. } if *mailbox == inbox),
    );
    let SyncEvent::WindowLoaded { cached, total, .. } = e else {
        panic!("expected a window event, got {e:?}");
    };
    assert_eq!(cached, 9);
    assert!(total.unwrap() >= 9);
    assert_eq!(
        h.read(|r| r.query_page(&key, 0, 0).unwrap().unwrap().cached),
        9
    );
    assert!(h.read(|r| r.conversations(inbox.as_str(), 0, 0).unwrap().total) > local_before);
    h.shutdown();
}

#[test]
fn rate_limits_and_unavailable_back_off_and_recover() {
    let mut setup = Setup::default();
    // Without push the sync loop is the only one talking, so the faults
    // land on it.
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    {
        let mut q = h.server.quirks_mut();
        q.http_429_next = 1;
        q.fail_next_with_503 = 1;
        q.retry_after_seconds = 1;
    }
    let mark = h.events.mark();
    let started = Instant::now();
    h.server.deliver_incoming(mail("Slow down")).unwrap();
    h.handle.send(Command::RefreshNow);
    h.synced_after(mark);
    // The refresh also refetches masks, so the two faults may land on two
    // loops at once. Each asked for a second, and each waited for it.
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "{:?}",
        started.elapsed()
    );
    let saw = |needle: &'static str| {
        h.events.any(|e| {
            matches!(e, SyncEvent::Status(s) if s.last_error.as_deref().is_some_and(|m| m.contains(needle)))
        })
    };
    assert!(saw("unavailable"));
    assert!(saw("rate limited"));
    let status = h.events.last_status().unwrap();
    assert!(status.online);
    assert!(status.last_error.is_none());
    h.shutdown();
}

#[test]
fn rejected_token_stops_until_a_new_one_arrives() {
    let server = FakeServer::demo(now());
    let good = server.token().to_owned();
    let setup = Setup {
        token: Some("not-the-token".into()),
        ..Setup::default()
    };
    let h = Harness::start(server, setup);
    h.events
        .wait("auth failed", |e| matches!(e, SyncEvent::AuthFailed { .. }));
    std::thread::sleep(Duration::from_millis(200));
    let sends = h.transport.sends.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    // No retry loop against a server that said no.
    assert_eq!(h.transport.sends.load(Ordering::SeqCst), sends);
    assert_eq!(
        h.events
            .all()
            .iter()
            .filter(|e| matches!(e, SyncEvent::AuthFailed { .. }))
            .count(),
        1
    );
    assert!(h.events.last_status().unwrap().auth_failed);
    h.handle.send(Command::SetToken(Secret::new(good)));
    h.synced();
    assert!(!h.events.last_status().unwrap().auth_failed);
    h.shutdown();
}

#[test]
fn offline_changes_wait_and_flush_on_reconnect() {
    let h = Harness::demo();
    h.synced();
    let inbox = h.mailbox("inbox");
    let archive = h.mailbox("archive");
    let target = h.read(|r| {
        r.conversations(inbox.as_str(), 0, 1).unwrap().rows[0]
            .email_id
            .clone()
    });

    h.transport.down.store(true, Ordering::SeqCst);
    // A refresh notices the outage.
    h.handle.send(Command::RefreshNow);
    h.events.wait(
        "offline",
        |e| matches!(e, SyncEvent::Status(s) if !s.online && s.push == PushState::Offline),
    );
    h.handle.send(Command::Mutate {
        token: 7,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            target.clone(),
            Patch::new()
                .mailbox(inbox.as_str(), false)
                .mailbox(archive.as_str(), true),
        )]),
        label: Some("Archive".into()),
        undoable: true,
    });
    h.events.wait("applied offline", |e| {
        matches!(e, SyncEvent::Applied { token: 7, .. })
    });
    h.events.wait(
        "pending count",
        |e| matches!(e, SyncEvent::Status(s) if !s.online && s.pending == 1),
    );
    // The change is visible locally but not on the server.
    assert!(
        !h.read(|r| r.conversations(inbox.as_str(), 0, 500).unwrap())
            .rows
            .iter()
            .any(|c| c.email_id == target)
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(h.server_email(target.as_str())["mailboxIds"][inbox.as_str()] == true);

    let mark = h.events.mark();
    h.transport.down.store(false, Ordering::SeqCst);
    h.wait_server("archived after reconnect", |_| {
        h.server_email(target.as_str())["mailboxIds"][archive.as_str()] == true
    });
    h.events.wait_after(
        mark,
        "back online, nothing pending",
        |e| matches!(e, SyncEvent::Status(s) if s.online && s.pending == 0),
    );
    h.shutdown();
}

#[test]
fn failing_push_falls_back_to_polling() {
    let mut setup = Setup::default();
    setup.config.push_failures_before_poll = 2;
    setup.config.poll_interval = Duration::from_millis(150);
    setup.push_down = true;
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    h.events.wait(
        "polling",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Polling),
    );
    let inbox = h.mailbox("inbox");
    let id = Id::from(h.server.deliver_incoming(mail("Found by polling")).unwrap());
    h.wait_store("polled mail", |r| {
        r.conversations(inbox.as_str(), 0, 5)
            .unwrap()
            .rows
            .iter()
            .any(|row| row.email_id == id)
    });
    // Push comes back and polling stops.
    h.transport.push_down.store(false, Ordering::SeqCst);
    h.events.wait(
        "push back",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Connected),
    );
    h.shutdown();
}

#[test]
fn shutdown_is_prompt_and_releases_the_store() {
    let h = Harness::demo();
    h.synced();
    let handle = h.handle.clone();
    let dir = h.dir.path().join("acct");
    let started = Instant::now();
    assert!(handle.shutdown());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!handle.send(Command::RefreshNow));
    // A second call is a no-op.
    assert!(handle.shutdown());
    // The store can be opened again at once.
    let store = mejl_store::Store::open(&dir, Default::default()).unwrap();
    assert!(store.type_state(type_name::EMAIL).unwrap().is_some());
}

#[test]
fn long_change_lists_are_read_in_rounds() {
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    setup.config.max_changes = 2;
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    let inbox = h.mailbox("inbox");
    let ids: Vec<Id> = h.read(|r| {
        r.conversations(inbox.as_str(), 0, 7)
            .unwrap()
            .rows
            .into_iter()
            .map(|c| c.email_id)
            .collect()
    });
    assert_eq!(ids.len(), 7);
    for id in &ids {
        h.server
            .mutate_from_elsewhere(ExternalChange::SetKeyword {
                email_id: id.to_string(),
                // A keyword no demo message has, so each call is a change.
                keyword: "rota".into(),
                value: true,
            })
            .unwrap();
    }
    let calls = h.transport.calls_named("Email/changes");
    let mark = h.events.mark();
    h.handle.send(Command::RefreshNow);
    h.synced_after(mark);
    for id in &ids {
        assert!(
            h.read(|r| r
                .email(id.as_str())
                .unwrap()
                .unwrap()
                .headers
                .keywords
                .contains("rota")),
            "{id} has the keyword"
        );
    }
    // Seven changes at two per call take at least four rounds.
    assert!(h.transport.calls_named("Email/changes") >= calls + 4);
    h.shutdown();
}
