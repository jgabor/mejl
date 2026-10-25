//! The outbox flusher against the fake: applying, splitting, rejections,
//! undo and redo, and bulk actions.

mod common;

use std::time::Duration;

use common::*;
use mejl_fake::{Address, ExternalChange, FakeConfig, FakeServer, IncomingMessage, Limits};
use mejl_jmap::{Id, Patch};
use mejl_store::{EmailPatchTarget, Mutation, RedoOutcome, UndoOutcome};
use mejl_sync::{BulkAction, Command, SyncEvent};

fn archive_patch(inbox: &Id, archive: &Id) -> Patch {
    Patch::new()
        .mailbox(inbox.as_str(), false)
        .mailbox(archive.as_str(), true)
}

fn quiet_setup() -> Setup {
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    setup
}

fn first_inbox_email(h: &Harness) -> Id {
    let inbox = h.mailbox("inbox");
    h.read(|r| {
        r.conversations(inbox.as_str(), 0, 1).unwrap().rows[0]
            .email_id
            .clone()
    })
}

fn in_mailbox(h: &Harness, email: &Id, mailbox: &Id) -> bool {
    h.server_email(email.as_str())["mailboxIds"][mailbox.as_str()] == true
}

#[test]
fn archive_reaches_the_server_and_settles() {
    let h = Harness::demo();
    h.synced();
    let inbox = h.mailbox("inbox");
    let archive = h.mailbox("archive");
    let email = first_inbox_email(&h);
    h.handle.send(Command::Mutate {
        token: 1,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            email.clone(),
            archive_patch(&inbox, &archive),
        )]),
        label: Some("Archive".into()),
        undoable: true,
    });
    let applied = h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 1, .. })
    });
    let SyncEvent::Applied { seq, changed, .. } = applied else {
        panic!("expected applied");
    };
    // The scope names both lists, so the caller re-reads them at once.
    assert!(changed.emails.contains(&email));
    assert!(changed.mailbox_lists.contains(&inbox));
    assert!(changed.mailbox_lists.contains(&archive));
    assert!(changed.mailboxes);
    // The overlay is in the store before the server has it.
    assert!(
        !h.read(|r| r.conversations(inbox.as_str(), 0, 500).unwrap())
            .rows
            .iter()
            .any(|c| c.email_id == email)
    );
    h.events.wait(
        "done",
        |e| matches!(e, SyncEvent::OutboxDone { seqs } if seqs.contains(&seq)),
    );
    assert!(in_mailbox(&h, &email, &archive));
    assert!(!in_mailbox(&h, &email, &inbox));
    // After the confirming sync the row stays put and nothing is pending.
    h.events.wait(
        "nothing pending",
        |e| matches!(e, SyncEvent::Status(s) if s.pending == 0),
    );
    h.wait_store("row in Archive", |r| {
        r.conversations(archive.as_str(), 0, 500)
            .unwrap()
            .rows
            .iter()
            .any(|c| c.email_id == email)
    });
    h.shutdown();
}

#[test]
fn big_units_are_split_at_max_objects_in_set() {
    let server = FakeServer::new(FakeConfig {
        now: Some(now()),
        limits: Limits {
            max_objects_in_set: 3,
            max_calls_in_request: 4,
            ..Limits::default()
        },
        ..FakeConfig::default()
    });
    for i in 0..13 {
        server
            .deliver_incoming(
                IncomingMessage::new(
                    Address::new("Ada Abara", "ada@example.com"),
                    format!("Note {i}"),
                )
                .text("hi"),
            )
            .unwrap();
    }
    let h = Harness::start(server, quiet_setup());
    h.synced();
    let inbox = h.mailbox("inbox");
    h.handle.send(Command::Bulk {
        token: 2,
        mailbox: inbox.clone(),
        except_threads: Vec::new(),
        action: BulkAction::SetSeen(true),
        label: None,
    });
    h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 2, .. })
    });
    h.wait_server("all read on the server", |s| {
        server_ids_in(s, inbox.as_str())
            .iter()
            .all(|id| h.server_email(id)["keywords"]["$seen"] == true)
    });
    // Thirteen objects at three per call is five calls, which take two
    // requests at four calls each.
    assert_eq!(h.transport.calls_named("Email/set"), 5);
    assert_eq!(h.transport.requests_with("Email/set"), 2);
    h.shutdown();
}

#[test]
fn destroy_of_a_vanished_email_counts_as_done() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let email = first_inbox_email(&h);
    h.server
        .mutate_from_elsewhere(ExternalChange::DestroyEmail {
            email_id: email.to_string(),
        })
        .unwrap();
    h.handle.send(Command::Mutate {
        token: 3,
        mutation: Mutation::EmailDestroy(vec![email.clone()]),
        label: Some("Delete forever".into()),
        undoable: false,
    });
    let SyncEvent::Applied { seq, .. } = h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 3, .. })
    }) else {
        panic!("expected applied");
    };
    h.events.wait(
        "done",
        |e| matches!(e, SyncEvent::OutboxDone { seqs } if seqs.contains(&seq)),
    );
    assert!(
        !h.events
            .any(|e| matches!(e, SyncEvent::OutboxFailed { .. }))
    );
    h.shutdown();
}

#[test]
fn permanent_failure_rolls_back_and_reports() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let inbox = h.mailbox("inbox");
    let email = first_inbox_email(&h);
    h.handle.send(Command::Mutate {
        token: 4,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            email.clone(),
            archive_patch(&inbox, &Id::from("no-such-mailbox")),
        )]),
        label: Some("Move to nowhere".into()),
        undoable: true,
    });
    h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 4, .. })
    });
    let failed = h
        .events
        .wait("failed", |e| matches!(e, SyncEvent::OutboxFailed { .. }));
    let SyncEvent::OutboxFailed {
        label,
        failed_targets,
        entry_failed,
        ..
    } = failed
    else {
        panic!("expected a failure");
    };
    assert_eq!(label.as_deref(), Some("Move to nowhere"));
    assert_eq!(failed_targets, std::slice::from_ref(&email));
    assert!(entry_failed);
    // The overlay is gone: the row is back in the Inbox.
    h.wait_store("row back in the Inbox", |r| {
        r.conversations(inbox.as_str(), 0, 500)
            .unwrap()
            .rows
            .iter()
            .any(|c| c.email_id == email)
    });
    assert!(in_mailbox(&h, &email, &inbox));
    h.shutdown();
}

#[test]
fn mutation_rejected_by_the_store_reports_the_token() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    h.handle.send(Command::Mutate {
        token: 5,
        mutation: Mutation::EmailPatch(Vec::new()),
        label: None,
        undoable: true,
    });
    h.events.wait("rejected", |e| {
        matches!(e, SyncEvent::Rejected { token: 5, .. })
    });
    h.shutdown();
}

#[test]
fn undo_and_redo_round_trip_through_the_server() {
    let h = Harness::demo();
    h.synced();
    let inbox = h.mailbox("inbox");
    let archive = h.mailbox("archive");
    let email = first_inbox_email(&h);
    h.handle.send(Command::Mutate {
        token: 6,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            email.clone(),
            archive_patch(&inbox, &archive),
        )]),
        label: Some("Archive".into()),
        undoable: true,
    });
    h.wait_server("archived", |_| in_mailbox(&h, &email, &archive));

    h.handle.send(Command::Undo { token: 7 });
    let undone = h.events.wait("undone", |e| {
        matches!(e, SyncEvent::Undone { token: 7, .. })
    });
    let SyncEvent::Undone {
        outcome, changed, ..
    } = undone
    else {
        panic!("expected undone");
    };
    assert!(matches!(outcome, Some(UndoOutcome::Reverted { ref label, .. }) if label == "Archive"));
    assert!(changed.mailbox_lists.contains(&inbox));
    assert!(changed.mailbox_lists.contains(&archive));
    h.wait_server("back in the Inbox", |_| {
        in_mailbox(&h, &email, &inbox) && !in_mailbox(&h, &email, &archive)
    });

    h.handle.send(Command::Redo { token: 8 });
    let redone = h.events.wait("redone", |e| {
        matches!(e, SyncEvent::Redone { token: 8, .. })
    });
    assert!(matches!(
        redone,
        SyncEvent::Redone {
            outcome: Some(RedoOutcome::Reapplied { .. }),
            ..
        }
    ));
    h.wait_server("archived again", |_| {
        in_mailbox(&h, &email, &archive) && !in_mailbox(&h, &email, &inbox)
    });

    // Nothing left to redo.
    h.handle.send(Command::Redo { token: 9 });
    assert!(matches!(
        h.events.wait("empty redo", |e| matches!(
            e,
            SyncEvent::Redone { token: 9, .. }
        )),
        SyncEvent::Redone { outcome: None, .. }
    ));
    h.shutdown();
}

#[test]
fn bulk_archive_honours_the_except_list() {
    let server = FakeServer::demo(now());
    server
        .call(
            "Mailbox/set",
            serde_json::json!({"create": {"e": {"name": "Empty"}}}),
        )
        .unwrap();
    let h = Harness::start(server, quiet_setup());
    h.synced();
    let inbox = h.mailbox("inbox");
    let archive = h.mailbox("archive");
    let threads = server_threads_in(&h.server, inbox.as_str());
    assert!(threads.len() > 3);
    let (keep_thread, _) = threads[1].clone();
    let keep: Vec<String> = threads
        .iter()
        .filter(|(t, _)| *t == keep_thread)
        .map(|(_, e)| e.clone())
        .collect();
    h.handle.send(Command::Bulk {
        token: 10,
        mailbox: inbox.clone(),
        except_threads: vec![Id::from(keep_thread.as_str())],
        action: BulkAction::Archive,
        label: None,
    });
    let SyncEvent::Applied { changed, .. } = h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 10, .. })
    }) else {
        panic!("expected applied");
    };
    assert!(changed.mailbox_lists.contains(&inbox));
    assert!(changed.mailbox_lists.contains(&archive));
    // Locally only the kept conversation is left.
    let left = h.read(|r| r.conversations(inbox.as_str(), 0, 500).unwrap());
    assert_eq!(left.rows.len(), 1);
    assert_eq!(left.rows[0].thread_id.as_str(), keep_thread);
    h.wait_server("only the kept thread in the Inbox", |s| {
        let mut now: Vec<String> = server_ids_in(s, inbox.as_str());
        now.sort();
        let mut want = keep.clone();
        want.sort();
        now == want
    });
    // One undoable action with the default label.
    assert!(
        h.read(|r| r.undo_peek().unwrap())
            .is_some_and(|l| l.starts_with("Archive "))
    );
    h.handle.send(Command::Undo { token: 11 });
    h.wait_server("all back", |s| {
        server_ids_in(s, inbox.as_str()).len() == threads.len()
    });

    // Nothing to do in an empty mailbox.
    let empty = h.mailbox("Empty");
    h.handle.send(Command::Bulk {
        token: 12,
        mailbox: empty,
        except_threads: Vec::new(),
        action: BulkAction::SetSeen(true),
        label: None,
    });
    h.events.wait("nothing to change", |e| {
        matches!(e, SyncEvent::Rejected { token: 12, .. })
    });
    h.shutdown();
}

#[test]
fn a_response_from_before_a_recorded_flush_is_fetched_again() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let inbox = h.mailbox("inbox");
    let archive = h.mailbox("archive");
    let email = first_inbox_email(&h);
    let before = h.transport.calls_named("Email/changes");

    // A sync whose answer reflects the server before the archive...
    *h.transport.hold.lock().unwrap() = Some("Email/changes".into());
    let mark = h.events.mark();
    h.handle.send(Command::RefreshNow);
    let deadline = std::time::Instant::now() + WAIT;
    while !h
        .transport
        .holding
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the sync was not held"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // ...arrives after the archive was sent and recorded.
    h.handle.send(Command::Mutate {
        token: 20,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            email.clone(),
            archive_patch(&inbox, &archive),
        )]),
        label: Some("Archive".into()),
        undoable: true,
    });
    h.events.wait_after(mark, "flush recorded", |e| {
        matches!(e, SyncEvent::OutboxDone { .. })
    });
    let released = h.events.mark();
    h.transport.release();
    h.synced_after(mark);
    // The held answer was dropped and asked for again.
    assert!(h.transport.calls_named("Email/changes") >= before + 2);
    // Exactly one pass finished after the release: the stale one stored
    // nothing and never counted as a sync. Had it been applied, the pass
    // the flush asked for would have finished as well.
    std::thread::sleep(Duration::from_millis(200));
    let finished: std::collections::BTreeSet<_> = h.events.all()[released..]
        .iter()
        .filter_map(|e| match e {
            SyncEvent::Status(s) => s.last_sync,
            _ => None,
        })
        .collect();
    let before_release = h.events.all()[..released]
        .iter()
        .rev()
        .find_map(|e| match e {
            SyncEvent::Status(s) => s.last_sync,
            _ => None,
        });
    let new_passes = finished
        .iter()
        .filter(|t| Some(**t) != before_release)
        .count();
    assert_eq!(new_passes, 1, "{finished:?}");
    let archived = |r: &mejl_store::Reader| {
        r.conversations(archive.as_str(), 0, 500)
            .unwrap()
            .rows
            .iter()
            .any(|c| c.email_id == email)
    };
    assert!(h.read(archived));
    assert!(in_mailbox(&h, &email, &archive));
    h.shutdown();
}

#[test]
fn mail_moves_into_a_mailbox_created_in_the_same_breath() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let inbox = h.mailbox("inbox");
    let email = first_inbox_email(&h);
    let mailbox = mejl_jmap::types::Mailbox {
        name: Some("Receipts 2026".into()),
        ..Default::default()
    };
    h.handle.send(Command::Mutate {
        token: 30,
        mutation: Mutation::MailboxCreate(mailbox),
        label: Some("New mailbox".into()),
        undoable: false,
    });
    let SyncEvent::Applied {
        temp_id: Some(temp),
        changed,
        ..
    } = h.events.wait("created locally", |e| {
        matches!(e, SyncEvent::Applied { token: 30, .. })
    })
    else {
        panic!("expected a placeholder id");
    };
    assert!(changed.mailboxes);
    h.handle.send(Command::Mutate {
        token: 31,
        mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
            email.clone(),
            Patch::new()
                .mailbox(inbox.as_str(), false)
                .mailbox(&temp, true),
        )]),
        label: Some("Move".into()),
        undoable: true,
    });
    h.events.wait("move applied", |e| {
        matches!(e, SyncEvent::Applied { token: 31, .. })
    });
    h.wait_server("moved into the new mailbox", |s| {
        s.mailbox_id("Receipts 2026")
            .is_some_and(|id| h.server_email(email.as_str())["mailboxIds"][id.as_str()] == true)
    });
    let real = h.mailbox("Receipts 2026");
    h.wait_store("the real mailbox replaces the placeholder", |r| {
        let rows = r.mailboxes().unwrap();
        rows.iter().any(|m| m.id == real && !m.pending)
            && !rows.iter().any(|m| m.id.as_str() == temp)
    });
    h.shutdown();
}

#[test]
fn contact_writes_fail_cleanly_without_server_support() {
    let h = Harness::start(FakeServer::demo(now()), quiet_setup());
    h.synced();
    let card: mejl_jmap::types::ContactCard =
        serde_json::from_value(serde_json::json!({"kind": "individual"})).unwrap();
    h.handle.send(Command::Mutate {
        token: 32,
        mutation: Mutation::ContactCreate(card),
        label: Some("Allow sender".into()),
        undoable: false,
    });
    let SyncEvent::OutboxFailed { label, error, .. } = h
        .events
        .wait("failed", |e| matches!(e, SyncEvent::OutboxFailed { .. }))
    else {
        panic!("expected a failure");
    };
    assert_eq!(label.as_deref(), Some("Allow sender"));
    assert!(error.contains("unknownMethod"), "{error}");
    h.shutdown();
}
