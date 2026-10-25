//! Outbox entries, overlays, collapse, undo, holds and crash recovery.

mod common;

use std::collections::BTreeMap;

use common::*;
use jiff::{Timestamp, ToSpan};
use mejl_jmap::method::{ChangesResponse, GetResponse};
use mejl_jmap::types::{ContactCard, Mailbox, MaskedEmail, MaskedEmailState};
use mejl_jmap::{Id, Patch};
use mejl_store::{
    CancelOutcome, EmailPatchTarget, EnqueueOptions, Error, FlushOp, FlushOutcome, Mutation,
    UndoOutcome,
};
use serde_json::json;

fn read() -> Patch {
    Patch::new().keyword("$seen", true)
}

fn unread() -> Patch {
    Patch::new().keyword("$seen", false)
}

fn archive() -> Patch {
    Patch::new().mailbox(INBOX, false).mailbox(ARCHIVE, true)
}

fn patch_one(id: &str, patch: Patch) -> Mutation {
    Mutation::EmailPatch(vec![EmailPatchTarget::new(id, patch)])
}

fn undoable(label: &str) -> EnqueueOptions<'_> {
    EnqueueOptions {
        label: Some(label),
        undoable: true,
    }
}

fn later(minutes: i64) -> Timestamp {
    now().checked_add(minutes.minutes()).unwrap()
}

fn overlay_rows(f: &Fixture) -> i64 {
    f.cache()
        .query_row("SELECT count(*) FROM overlay", [], |r| r.get(0))
        .unwrap()
}

fn set_counts(f: &mut Fixture, id: &str, total: u64, unread: u64) {
    let mut changes = ChangesResponse::<Mailbox>::new("A1", "mb-1", "mb-2");
    changes.updated = vec![Id::from(id)];
    let partial: Mailbox = serde_json::from_value(json!({
        "id": id, "totalEmails": total, "unreadEmails": unread,
        "totalThreads": total, "unreadThreads": unread
    }))
    .unwrap();
    let mut batch = f.store.batch().unwrap();
    batch.apply_mailbox_changes(&changes, &[partial]).unwrap();
    batch.commit().unwrap();
}

fn inbox_fixture() -> Fixture {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("e1").at("2026-10-20T09:00:00Z").build(),
        email("e2").at("2026-10-21T09:00:00Z").build(),
    ]);
    set_counts(&mut f, INBOX, 2, 2);
    f
}

#[test]
fn overlay_shows_at_once_and_survives_stale_server_data() {
    let mut f = inbox_fixture();
    let reader = f.reader();
    let target = EmailPatchTarget::new("e1", read().merged(archive()));
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![target]),
            undoable("Archive"),
            now(),
        )
        .unwrap();

    let check_overlaid = |reader: &mejl_store::Reader| {
        let inbox = reader.conversations(INBOX, 0, 10).unwrap();
        assert_eq!(inbox.rows.len(), 1);
        assert_eq!(inbox.rows[0].email_id, "e2");
        let archived = reader.conversations(ARCHIVE, 0, 10).unwrap();
        assert_eq!(archived.rows[0].email_id, "e1");
        assert!(!archived.rows[0].is_unread());
        let boxes = reader.mailboxes().unwrap();
        let inbox = boxes.iter().find(|m| m.id == INBOX).unwrap();
        assert_eq!((inbox.total_emails, inbox.unread_emails), (1, 1));
        assert_eq!((inbox.total_threads, inbox.unread_threads), (1, 1));
        let archive = boxes.iter().find(|m| m.id == ARCHIVE).unwrap();
        assert_eq!((archive.total_emails, archive.unread_emails), (1, 0));
    };
    check_overlaid(&reader);

    // A changes batch from before the flush still has e1 unread in Inbox.
    f.changes(&[flags("e1", &[INBOX], &[])], &[]);
    check_overlaid(&reader);
    f.check_consistency();

    // Flush and succeed: the overlay stays until server data arrives.
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::EmailSet { update, destroy } = &unit.op else {
        panic!("expected Email/set, got {:?}", unit.op);
    };
    assert!(destroy.is_empty());
    assert_eq!(
        serde_json::to_value(&update[&Id::from("e1")]).unwrap(),
        json!({"keywords/$seen": true, "mailboxIds/mb-archive": true, "mailboxIds/mb-inbox": null})
    );
    assert!(
        f.store.next_flush(now(), 50).unwrap().is_none(),
        "nothing else ready"
    );
    let report = f
        .store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    assert_eq!(report.done, unit.seqs);
    check_overlaid(&reader);
    assert_eq!(overlay_rows(&f), 1);

    // The server now reports the change: the overlay is confirmed.
    f.changes(&[flags("e1", &[ARCHIVE], &["$seen"])], &[]);
    set_counts(&mut f, INBOX, 1, 1);
    set_counts(&mut f, ARCHIVE, 1, 0);
    check_overlaid(&reader);
    assert_eq!(overlay_rows(&f), 0);
    let has_overlay: i64 = f
        .cache()
        .query_row(
            "SELECT count(*) FROM email WHERE has_overlay = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_overlay, 0);
    let active: i64 = f
        .state()
        .query_row(
            "SELECT count(*) FROM outbox WHERE overlay_active = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active, 0);
    f.check_consistency();
}

#[test]
fn done_overlay_gives_way_to_later_server_changes() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    // Another device marked it unread again after our change.
    f.changes(&[flags("e1", &[INBOX], &[])], &[]);
    let row = f.reader().email("e1").unwrap().unwrap();
    assert!(
        !row.headers.row.seen,
        "server truth wins once the server reports the email"
    );
}

#[test]
fn permanent_failure_rolls_back() {
    let mut f = inbox_fixture();
    let reader = f.reader();
    f.store
        .enqueue(patch_one("e1", archive()), undoable("Archive"), now())
        .unwrap();
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 1);
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let report = f
        .store
        .finish_flush(
            &unit,
            FlushOutcome::Failed {
                error: "forbidden".into(),
            },
            now(),
        )
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].label.as_deref(), Some("Archive"));
    assert!(report.failed[0].entry_failed);
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 2);
    assert!(
        reader
            .conversations(ARCHIVE, 0, 10)
            .unwrap()
            .rows
            .is_empty()
    );
    let inbox = reader
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.id == INBOX)
        .unwrap();
    assert_eq!(inbox.total_emails, 2);
    assert_eq!(overlay_rows(&f), 0);
    assert_eq!(reader.outbox_summary(now()).unwrap().failed, 1);
    // Undo of a failed action has nothing to revert.
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::TooLate { .. })
    ));
    f.check_consistency();
}

#[test]
fn partial_rejection_rolls_back_only_rejected_emails() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![
                EmailPatchTarget::new("e1", read()),
                EmailPatchTarget::new("e2", read()),
            ]),
            Default::default(),
            now(),
        )
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let mut rejected = BTreeMap::new();
    rejected.insert(Id::from("e2"), "notFound".to_owned());
    let report = f
        .store
        .finish_flush(
            &unit,
            FlushOutcome::Applied {
                created_id: None,
                rejected,
            },
            now(),
        )
        .unwrap();
    assert_eq!(report.done, unit.seqs);
    assert_eq!(report.failed[0].failed_targets, vec![Id::from("e2")]);
    assert!(!report.failed[0].entry_failed);
    let reader = f.reader();
    assert!(reader.email("e1").unwrap().unwrap().headers.row.seen);
    assert!(!reader.email("e2").unwrap().unwrap().headers.row.seen);
}

#[test]
fn retry_waits_for_backoff() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert_eq!(unit.attempt, 1);
    f.store
        .finish_flush(
            &unit,
            FlushOutcome::Retry {
                error: "HTTP 503".into(),
                not_before: Some(later(1)),
            },
            now(),
        )
        .unwrap();
    assert!(
        f.reader().email("e1").unwrap().unwrap().headers.row.seen,
        "overlay kept"
    );
    assert!(f.store.next_flush(now(), 50).unwrap().is_none());
    let unit = f.store.next_flush(later(2), 50).unwrap().unwrap();
    assert_eq!(unit.attempt, 2);
    assert_eq!(f.reader().outbox_summary(now()).unwrap().pending, 1);
}

#[test]
fn repeated_patches_collapse_and_no_ops_drop() {
    let mut f = inbox_fixture();
    for patch in [read(), unread(), Patch::new().keyword("$flagged", true)] {
        f.store
            .enqueue(patch_one("e1", patch), Default::default(), now())
            .unwrap();
    }
    f.store
        .enqueue(patch_one("e2", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert_eq!(unit.seqs.len(), 4);
    let FlushOp::EmailSet { update, .. } = &unit.op else {
        panic!("expected Email/set");
    };
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({"e1": {"keywords/$flagged": true}, "e2": {"keywords/$seen": true}})
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();

    // Read then unread is nothing: no request at all.
    let a = f
        .store
        .enqueue(patch_one("e2", unread()), Default::default(), now())
        .unwrap();
    let b = f
        .store
        .enqueue(patch_one("e2", read()), Default::default(), now())
        .unwrap();
    assert!(f.store.next_flush(now(), 50).unwrap().is_none());
    let state = f.state();
    for seq in [a.seq, b.seq] {
        let status: String = state
            .query_row("SELECT status FROM outbox WHERE seq = ?1", [seq.0], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
    }
}

#[test]
fn a_new_reversal_of_a_done_change_is_not_dropped() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    // The server has $seen now, though the cache has not heard yet.
    f.store
        .enqueue(patch_one("e1", unread()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::EmailSet { update, .. } = &unit.op else {
        panic!("expected Email/set");
    };
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({"e1": {"keywords/$seen": null}})
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    // The net of both done entries matches cached server truth, so both
    // are confirmed at once.
    assert_eq!(overlay_rows(&f), 0);
}

#[test]
fn max_objects_limits_a_unit_and_order_is_kept_per_email() {
    let mut f = Fixture::new().with_mailboxes();
    let ids: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
    f.apply(&ids.iter().map(|id| email(id).build()).collect::<Vec<_>>());
    for id in &ids {
        f.store
            .enqueue(patch_one(id, read()), Default::default(), now())
            .unwrap();
    }
    let first = f.store.next_flush(now(), 2).unwrap().unwrap();
    assert_eq!(first.seqs.len(), 2);
    // A later entry for an email that is in flight waits for it.
    f.store
        .enqueue(patch_one("m0", unread()), Default::default(), now())
        .unwrap();
    let second = f.store.next_flush(now(), 10).unwrap().unwrap();
    let FlushOp::EmailSet { update, .. } = &second.op else {
        panic!("expected Email/set");
    };
    assert_eq!(update.len(), 3);
    assert!(!update.contains_key(&Id::from("m0")));
    assert!(f.store.next_flush(now(), 10).unwrap().is_none());
    f.store
        .finish_flush(&first, FlushOutcome::ok(), now())
        .unwrap();
    let third = f.store.next_flush(now(), 10).unwrap().unwrap();
    let FlushOp::EmailSet { update, .. } = &third.op else {
        panic!("expected Email/set");
    };
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({"m0": {"keywords/$seen": null}})
    );
}

#[test]
fn undo_reverts_and_collapses_while_pending() {
    let mut f = inbox_fixture();
    let reader = f.reader();
    let first = f
        .store
        .enqueue(patch_one("e1", read()), undoable("Mark read"), now())
        .unwrap();
    assert_eq!(reader.undo_peek().unwrap().as_deref(), Some("Mark read"));
    assert!(reader.email("e1").unwrap().unwrap().headers.row.seen);
    let outcome = f.store.undo(now()).unwrap().unwrap();
    let UndoOutcome::Reverted {
        label, original, ..
    } = outcome
    else {
        panic!("expected a revert, got {outcome:?}");
    };
    assert_eq!(label, "Mark read");
    assert_eq!(original, first.seq);
    assert!(!reader.email("e1").unwrap().unwrap().headers.row.seen);
    assert!(
        f.store.next_flush(now(), 50).unwrap().is_none(),
        "the pair collapsed to nothing"
    );
    assert!(
        f.store.undo(now()).unwrap().is_none(),
        "undo does not push itself"
    );
}

#[test]
fn undo_after_send_queues_the_inverse() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", archive()), undoable("Archive"), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.changes(&[flags("e1", &[ARCHIVE], &[])], &[]);
    let outcome = f.store.undo(now()).unwrap().unwrap();
    assert!(matches!(outcome, UndoOutcome::Reverted { .. }));
    assert_eq!(
        f.reader().conversations(INBOX, 0, 10).unwrap().rows.len(),
        2
    );
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::EmailSet { update, .. } = &unit.op else {
        panic!("expected Email/set");
    };
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({"e1": {"mailboxIds/mb-archive": null, "mailboxIds/mb-inbox": true}})
    );
}

#[test]
fn undo_stack_keeps_ten() {
    let mut f = inbox_fixture();
    for i in 0..12 {
        let patch = if i % 2 == 0 { read() } else { unread() };
        let label = format!("action {i}");
        f.store
            .enqueue(
                patch_one("e1", patch),
                EnqueueOptions {
                    label: Some(&label),
                    undoable: true,
                },
                now(),
            )
            .unwrap();
    }
    let mut labels = Vec::new();
    while let Some(outcome) = f.store.undo(now()).unwrap() {
        if let UndoOutcome::Reverted { label, .. } = outcome {
            labels.push(label);
        }
    }
    assert_eq!(labels.len(), mejl_store::UNDO_DEPTH);
    assert_eq!(labels[0], "action 11");
    assert_eq!(labels[9], "action 2");
}

#[test]
fn destroy_hides_and_is_not_undoable() {
    let mut f = inbox_fixture();
    let err = f
        .store
        .enqueue(
            Mutation::EmailDestroy(vec![Id::from("e1")]),
            undoable("Delete"),
            now(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotUndoable));

    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    f.store
        .enqueue(
            Mutation::EmailDestroy(vec![Id::from("e1")]),
            Default::default(),
            now(),
        )
        .unwrap();
    let reader = f.reader();
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 1);
    assert!(reader.conversation("t-e1").unwrap().is_empty());
    assert!(
        reader
            .search("e1", &Default::default())
            .unwrap()
            .hits
            .is_empty()
    );
    let inbox = reader
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.id == INBOX)
        .unwrap();
    assert_eq!((inbox.total_emails, inbox.unread_emails), (1, 1));

    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert_eq!(
        unit.op,
        FlushOp::EmailSet {
            update: BTreeMap::new(),
            destroy: vec![Id::from("e1")]
        }
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.changes(&[], &["e1"]);
    assert_eq!(overlay_rows(&f), 0);
    assert!(reader.email("e1").unwrap().is_none());
    f.check_consistency();
}

#[test]
fn held_sends_wait_and_can_be_cancelled() {
    let mut f = Fixture::new().with_mailboxes();
    let payload = json!({"subject": "Booking", "messageId": "local-1@mejl.example.org"});
    let held = f
        .store
        .enqueue(
            Mutation::EmailSubmit {
                payload: payload.clone(),
                hold_until: Some(now().checked_add(10.seconds()).unwrap()),
            },
            undoable("Send"),
            now(),
        )
        .unwrap();
    let reader = f.reader();
    let summary = reader.outbox_summary(now()).unwrap();
    assert_eq!((summary.pending, summary.held), (0, 1));
    let queued = reader.queued_sends(now()).unwrap();
    assert_eq!(queued[0].seq, held.seq);
    assert!(queued[0].hold_until.is_some());
    assert!(f.store.next_flush(now(), 50).unwrap().is_none());

    // Undo send inside the window cancels and hands back the payload.
    let outcome = f.store.undo(now()).unwrap().unwrap();
    assert_eq!(
        outcome,
        UndoOutcome::Cancelled {
            label: "Send".into(),
            seq: held.seq,
            payload: Some(payload.clone()),
        }
    );
    assert!(f.store.next_flush(later(5), 50).unwrap().is_none());

    // A second send goes out once its hold is over.
    let second = f
        .store
        .enqueue(
            Mutation::EmailSubmit {
                payload: payload.clone(),
                hold_until: Some(now().checked_add(10.seconds()).unwrap()),
            },
            Default::default(),
            now(),
        )
        .unwrap();
    let unit = f
        .store
        .next_flush(now().checked_add(11.seconds()).unwrap(), 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        unit.op,
        FlushOp::EmailSubmit {
            payload: payload.clone()
        }
    );
    assert_eq!(
        f.store.cancel(second.seq, now()).unwrap(),
        CancelOutcome::TooLate
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    assert!(reader.queued_sends(now()).unwrap().is_empty());

    // send_now releases a hold early.
    let third = f
        .store
        .enqueue(
            Mutation::EmailSubmit {
                payload,
                hold_until: Some(later(60)),
            },
            Default::default(),
            now(),
        )
        .unwrap();
    assert!(f.store.next_flush(now(), 50).unwrap().is_none());
    assert!(f.store.send_now(third.seq).unwrap());
    assert!(f.store.next_flush(now(), 50).unwrap().is_some());
    assert!(matches!(
        f.store.cancel(mejl_store::Seq(9999), now()),
        Err(Error::UnknownEntry(9999))
    ));
}

#[test]
fn held_send_does_not_block_triage() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(
            Mutation::EmailSubmit {
                payload: json!({}),
                hold_until: Some(later(1)),
            },
            Default::default(),
            now(),
        )
        .unwrap();
    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert!(matches!(unit.op, FlushOp::EmailSet { .. }));
}

#[test]
fn crash_recovery_resets_in_flight_and_keeps_overlays() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", archive()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert!(!unit.interrupted);
    // mejl dies here, before finish_flush.
    let mut f = f.reopen();
    assert_eq!(
        f.reader().conversations(INBOX, 0, 10).unwrap().rows.len(),
        1
    );
    let again = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert!(again.interrupted);
    assert_eq!(again.attempt, 2);
    assert_eq!(again.op, unit.op);
    f.check_consistency();
}

#[test]
fn deleting_cache_db_rebuilds_overlays_from_state_db() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", archive()), Default::default(), now())
        .unwrap();
    let Fixture { dir, store } = f;
    drop(store);
    for name in ["cache.db", "cache.db-wal", "cache.db-shm"] {
        let _ = std::fs::remove_file(dir.path().join("acct").join(name));
    }
    let mut f = Fixture {
        store: mejl_store::Store::open(dir.path().join("acct"), Default::default()).unwrap(),
        dir,
    }
    .with_mailboxes();
    // Server data arrives again; the pending archive is applied on top.
    f.apply(&[email("e1").build(), email("e2").build()]);
    let reader = f.reader();
    assert_eq!(reader.conversations(ARCHIVE, 0, 10).unwrap().rows.len(), 1);
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 1);
    f.check_consistency();
}

#[test]
fn new_mailbox_then_move_into_it() {
    let mut f = inbox_fixture();
    let reader = f.reader();
    let created = f
        .store
        .enqueue(
            Mutation::MailboxCreate(serde_json::from_value(json!({"name": "Receipts"})).unwrap()),
            undoable("New mailbox"),
            now(),
        )
        .unwrap();
    let temp = created.temp_id.clone().unwrap();
    assert!(temp.starts_with("local:"));
    f.store
        .enqueue(
            patch_one(
                "e1",
                Patch::new().mailbox(INBOX, false).mailbox(&temp, true),
            ),
            Default::default(),
            now(),
        )
        .unwrap();
    let pending = reader
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.pending)
        .unwrap();
    assert_eq!(pending.id, temp.as_str());
    assert_eq!(pending.name, "Receipts");
    assert_eq!(pending.local_messages, 1);
    assert_eq!(
        reader.conversations(&temp, 0, 10).unwrap().rows[0].email_id,
        "e1"
    );

    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::MailboxCreate {
        creation_id,
        mailbox: created_box,
    } = &unit.op
    else {
        panic!("expected the create first, got {:?}", unit.op);
    };
    assert_eq!(creation_id, &temp);
    assert_eq!(created_box.name.as_deref(), Some("Receipts"));
    assert!(
        f.store.next_flush(now(), 50).unwrap().is_none(),
        "the move waits for the create"
    );
    assert!(matches!(
        f.store.finish_flush(&unit, FlushOutcome::ok(), now()),
        Err(Error::Invalid(_))
    ));
    f.store
        .finish_flush(&unit, FlushOutcome::created("mb-receipts"), now())
        .unwrap();

    let move_unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let FlushOp::EmailSet { update, .. } = &move_unit.op else {
        panic!("expected Email/set");
    };
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({"e1": {"mailboxIds/mb-inbox": null, "mailboxIds/mb-receipts": true}})
    );
    assert_eq!(
        reader
            .conversations("mb-receipts", 0, 10)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(reader.conversations(&temp, 0, 10).unwrap().rows.is_empty());
    let pending = reader
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.pending)
        .unwrap();
    assert_eq!(pending.id, "mb-receipts");

    // The server's mailbox list now has it: the placeholder row goes.
    let mut batch = f.store.batch().unwrap();
    batch
        .upsert_mailboxes(&[mailbox("mb-receipts", "Receipts", None, None)])
        .unwrap();
    batch.commit().unwrap();
    let boxes = reader.mailboxes().unwrap();
    assert!(boxes.iter().all(|m| !m.pending));
    assert_eq!(boxes.iter().filter(|m| m.id == "mb-receipts").count(), 1);
    f.check_consistency();
}

#[test]
fn failed_mailbox_create_fails_moves_into_it() {
    let mut f = inbox_fixture();
    let created = f
        .store
        .enqueue(
            Mutation::MailboxCreate(serde_json::from_value(json!({"name": "Receipts"})).unwrap()),
            Default::default(),
            now(),
        )
        .unwrap();
    let temp = created.temp_id.unwrap();
    f.store
        .enqueue(
            patch_one(
                "e1",
                Patch::new().mailbox(INBOX, false).mailbox(&temp, true),
            ),
            EnqueueOptions {
                label: Some("Move"),
                undoable: false,
            },
            now(),
        )
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    let report = f
        .store
        .finish_flush(
            &unit,
            FlushOutcome::Failed {
                error: "alreadyExists".into(),
            },
            now(),
        )
        .unwrap();
    assert_eq!(report.failed.len(), 2);
    assert!(
        report
            .failed
            .iter()
            .any(|e| e.label.as_deref() == Some("Move"))
    );
    let reader = f.reader();
    assert_eq!(reader.conversations(INBOX, 0, 10).unwrap().rows.len(), 2);
    assert!(reader.mailboxes().unwrap().iter().all(|m| !m.pending));
    assert!(f.store.next_flush(now(), 50).unwrap().is_none());
    f.check_consistency();
}

fn masks(list: serde_json::Value) -> GetResponse<MaskedEmail> {
    serde_json::from_value(json!({"accountId": "A1", "state": "", "list": list, "notFound": []}))
        .unwrap()
}

#[test]
fn masked_email_update_and_create_overlays() {
    let mut f = Fixture::new();
    let mut batch = f.store.batch().unwrap();
    batch
        .replace_masked_emails(&masks(json!([
            {"id": "me1", "email": "kettle.fern4821@example.org", "state": "enabled", "description": "Larkspur shop"}
        ])))
        .unwrap();
    batch.commit().unwrap();
    let reader = f.reader();
    f.store
        .enqueue(
            Mutation::MaskedEmailUpdate {
                id: Id::from("me1"),
                patch: Patch::new().property("state", "disabled"),
                inverse: None,
            },
            undoable("Disable mask"),
            now(),
        )
        .unwrap();
    let row = reader.masked_emails().unwrap().remove(0);
    assert_eq!(row.masked.state, Some(MaskedEmailState::Disabled));
    assert!(row.pending);

    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert_eq!(
        unit.op,
        FlushOp::MaskedEmailUpdate {
            id: Id::from("me1"),
            patch: Patch::new().property("state", "disabled")
        }
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    assert_eq!(
        reader.masked_emails().unwrap()[0].masked.state,
        Some(MaskedEmailState::Disabled)
    );

    // Undo queues the inverse the store computed.
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::Reverted { .. })
    ));
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert_eq!(
        unit.op,
        FlushOp::MaskedEmailUpdate {
            id: Id::from("me1"),
            patch: Patch::new().property("state", "enabled")
        }
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();

    // A create shows as a pending row until the refresh lists it.
    f.store
        .enqueue(
            Mutation::MaskedEmailCreate(MaskedEmail::new_enabled("Lakeside cabins")),
            Default::default(),
            now(),
        )
        .unwrap();
    let rows = reader.masked_emails().unwrap();
    let new = rows
        .iter()
        .find(|r| r.masked.description.as_deref() == Some("Lakeside cabins"))
        .unwrap();
    assert!(new.pending && new.masked.email.is_none());
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    assert!(matches!(unit.op, FlushOp::MaskedEmailCreate { .. }));
    f.store
        .finish_flush(&unit, FlushOutcome::created("me9"), now())
        .unwrap();
    assert!(
        reader
            .masked_emails()
            .unwrap()
            .iter()
            .any(|r| r.masked.id == "me9" && r.pending)
    );

    let mut batch = f.store.batch().unwrap();
    batch
        .replace_masked_emails(&masks(json!([
            {"id": "me1", "email": "kettle.fern4821@example.org", "state": "enabled"},
            {"id": "me9", "email": "calm.heron@example.org", "state": "enabled", "description": "Lakeside cabins"}
        ])))
        .unwrap();
    batch.commit().unwrap();
    let rows = reader.masked_emails().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !r.pending));
    assert_eq!(overlay_rows(&f), 0);
}

#[test]
fn contact_create_shows_in_suggestions() {
    let mut f = Fixture::new();
    let card: ContactCard = serde_json::from_value(json!({
        "name": {"full": "Larkspur Support"},
        "emails": {"e": {"address": "help@shop.example.com"}}
    }))
    .unwrap();
    let enqueued = f
        .store
        .enqueue(
            Mutation::ContactCreate(card),
            undoable("Allow sender"),
            now(),
        )
        .unwrap();
    let s = f.reader().address_suggestions("lark", 5).unwrap();
    assert_eq!(s[0].email, "help@shop.example.com");
    assert_eq!(
        s[0].contact_id.as_ref().map(Id::as_str),
        enqueued.temp_id.as_deref()
    );
    // Undo before it was sent just cancels it.
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::Cancelled { .. })
    ));
    assert!(
        f.reader()
            .address_suggestions("lark", 5)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        f.store.enqueue(
            Mutation::ContactDestroy(Id::from("c1")),
            undoable("Delete"),
            now()
        ),
        Err(Error::NotUndoable)
    ));
}

#[test]
fn invalid_mutations_are_refused() {
    let mut f = inbox_fixture();
    let cases = vec![
        Mutation::EmailPatch(vec![]),
        Mutation::EmailPatch(vec![EmailPatchTarget::new("e1", Patch::new())]),
        Mutation::EmailPatch(vec![
            EmailPatchTarget::new("e1", read()),
            EmailPatchTarget::new("e1", unread()),
        ]),
        Mutation::EmailPatch(vec![EmailPatchTarget::new("", read())]),
        Mutation::EmailDestroy(vec![]),
        Mutation::MailboxCreate(Mailbox::default()),
        Mutation::MaskedEmailUpdate {
            id: Id::from("me1"),
            patch: Patch::new(),
            inverse: None,
        },
    ];
    for m in cases {
        let err = f
            .store
            .enqueue(m.clone(), Default::default(), now())
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{m:?} gave {err:?}");
    }
    assert_eq!(f.reader().outbox_summary(now()).unwrap().pending, 0);
}

#[test]
fn finishing_a_unit_twice_is_an_error() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    assert!(matches!(
        f.store.finish_flush(&unit, FlushOutcome::ok(), now()),
        Err(Error::UnknownEntry(_))
    ));
}

#[test]
fn prune_keeps_entries_the_undo_stack_needs() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(patch_one("e1", read()), undoable("Read"), now())
        .unwrap();
    f.store
        .enqueue(patch_one("e2", read()), Default::default(), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 50).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.changes(
        &[
            flags("e1", &[INBOX], &["$seen"]),
            flags("e2", &[INBOX], &["$seen"]),
        ],
        &[],
    );
    assert_eq!(f.store.prune_outbox(later(1)).unwrap(), 1);
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::Reverted { .. })
    ));
}

#[test]
fn unread_counts_follow_pending_reads() {
    let mut f = inbox_fixture();
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![
                EmailPatchTarget::new("e1", read()),
                EmailPatchTarget::new("e2", read()),
            ]),
            Default::default(),
            now(),
        )
        .unwrap();
    let inbox = f
        .reader()
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.id == INBOX)
        .unwrap();
    assert_eq!((inbox.unread_emails, inbox.unread_threads), (0, 0));
    assert_eq!((inbox.total_emails, inbox.total_threads), (2, 2));
}

trait Merge {
    fn merged(self, other: Patch) -> Patch;
}

impl Merge for Patch {
    fn merged(mut self, other: Patch) -> Patch {
        self.merge(other);
        self
    }
}

#[test]
fn malformed_units_are_refused() {
    let mut f = inbox_fixture();
    let empty = mejl_store::FlushUnit {
        seqs: Vec::new(),
        op: FlushOp::EmailSubmit { payload: json!({}) },
        attempt: 1,
        interrupted: false,
    };
    assert!(matches!(
        f.store.finish_flush(&empty, FlushOutcome::ok(), now()),
        Err(Error::Invalid(_))
    ));
    f.store
        .enqueue(
            Mutation::EmailSubmit {
                payload: json!({}),
                hold_until: None,
            },
            Default::default(),
            now(),
        )
        .unwrap();
    f.store
        .enqueue(
            Mutation::EmailSubmit {
                payload: json!({}),
                hold_until: None,
            },
            Default::default(),
            now(),
        )
        .unwrap();
    let a = f.store.next_flush(now(), 50).unwrap().unwrap();
    let b = f.store.next_flush(now(), 50).unwrap().unwrap();
    let mut both = a.clone();
    both.seqs.extend(b.seqs.iter().copied());
    assert!(matches!(
        f.store.finish_flush(&both, FlushOutcome::ok(), now()),
        Err(Error::Invalid(_))
    ));
    f.store.finish_flush(&a, FlushOutcome::ok(), now()).unwrap();
    f.store.finish_flush(&b, FlushOutcome::ok(), now()).unwrap();
}
