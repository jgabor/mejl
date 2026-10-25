//! Redo, and the reads the sync engine makes on the store thread.

mod common;

use std::collections::BTreeMap;

use common::*;
use mejl_jmap::{Id, Patch};
use mejl_store::{
    EmailPatchTarget, EnqueueOptions, FlushOutcome, Mutation, RedoOutcome, UNDO_DEPTH, UndoOutcome,
};

fn archive(id: &str) -> Mutation {
    Mutation::EmailPatch(vec![EmailPatchTarget::new(
        id,
        Patch::new().mailbox(INBOX, false).mailbox(ARCHIVE, true),
    )])
}

fn undoable(label: &str) -> EnqueueOptions<'_> {
    EnqueueOptions {
        label: Some(label),
        undoable: true,
    }
}

fn inbox_ids(f: &Fixture) -> Vec<String> {
    f.reader()
        .messages(INBOX, 0, 100)
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.email_id.into_inner())
        .collect()
}

fn fixture_with(ids: &[&str]) -> Fixture {
    let mut f = Fixture::new().with_mailboxes();
    let emails: Vec<_> = ids.iter().map(|id| email(id).build()).collect();
    f.apply(&emails);
    f
}

#[test]
fn undo_then_redo_round_trips() {
    let mut f = fixture_with(&["e1"]);
    f.store
        .enqueue(archive("e1"), undoable("Archive"), now())
        .unwrap();
    assert!(inbox_ids(&f).is_empty());
    assert_eq!(f.store.redo_peek().unwrap(), None);

    let undone = f.store.undo(now()).unwrap();
    assert!(matches!(undone, Some(UndoOutcome::Reverted { .. })));
    assert_eq!(inbox_ids(&f), ["e1"]);
    assert_eq!(f.store.redo_peek().unwrap().as_deref(), Some("Archive"));
    assert_eq!(f.reader().redo_peek().unwrap().as_deref(), Some("Archive"));
    assert_eq!(f.store.undo_peek().unwrap(), None);

    let redone = f.store.redo(now()).unwrap();
    let Some(RedoOutcome::Reapplied { label, seq, .. }) = redone else {
        panic!("expected a reapply, got {redone:?}");
    };
    assert_eq!(label, "Archive");
    assert!(inbox_ids(&f).is_empty());
    // The redone action is back on the undo stack under its own label.
    assert_eq!(f.store.undo_peek().unwrap().as_deref(), Some("Archive"));
    assert_eq!(f.store.redo_peek().unwrap(), None);

    // And it can be undone and redone again.
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::Reverted { original, .. }) if original == seq
    ));
    assert_eq!(inbox_ids(&f), ["e1"]);
    assert!(matches!(
        f.store.redo(now()).unwrap(),
        Some(RedoOutcome::Reapplied { .. })
    ));
    assert!(inbox_ids(&f).is_empty());
    f.check_consistency();
}

#[test]
fn everything_pending_collapses_to_one_net_request() {
    let mut f = fixture_with(&["e1"]);
    f.store
        .enqueue(archive("e1"), undoable("Archive"), now())
        .unwrap();
    f.store.undo(now()).unwrap();
    f.store.redo(now()).unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().expect("a unit");
    let mejl_store::FlushOp::EmailSet { update, destroy } = &unit.op else {
        panic!("expected an email set");
    };
    assert!(destroy.is_empty());
    let patch = update.get("e1").expect("e1 is patched");
    assert_eq!(
        serde_json::to_value(patch).unwrap(),
        serde_json::json!({format!("mailboxIds/{INBOX}"): null, format!("mailboxIds/{ARCHIVE}"): true})
    );
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    assert!(f.store.next_flush(now(), 100).unwrap().is_none());
}

#[test]
fn redo_on_empty_stack_is_none() {
    let mut f = fixture_with(&["e1"]);
    assert_eq!(f.store.redo(now()).unwrap(), None);
    f.store
        .enqueue(archive("e1"), undoable("Archive"), now())
        .unwrap();
    assert_eq!(f.store.redo(now()).unwrap(), None);
}

#[test]
fn a_new_action_clears_redo() {
    let mut f = fixture_with(&["e1", "e2"]);
    f.store
        .enqueue(archive("e1"), undoable("Archive e1"), now())
        .unwrap();
    f.store.undo(now()).unwrap();
    assert!(f.store.redo_peek().unwrap().is_some());
    // Even an action that is not itself undoable starts a new history.
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![EmailPatchTarget::new(
                "e2",
                Patch::new().keyword("$seen", true),
            )]),
            EnqueueOptions::default(),
            now(),
        )
        .unwrap();
    assert_eq!(f.store.redo_peek().unwrap(), None);
    assert_eq!(f.store.redo(now()).unwrap(), None);
}

#[test]
fn redo_is_capped_at_the_undo_depth() {
    let ids: Vec<String> = (0..UNDO_DEPTH + 3).map(|i| format!("e{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut f = fixture_with(&refs);
    for id in &refs {
        f.store
            .enqueue(archive(id), undoable(&format!("Archive {id}")), now())
            .unwrap();
    }
    let mut undone = 0;
    while let Some(outcome) = f.store.undo(now()).unwrap() {
        assert!(matches!(outcome, UndoOutcome::Reverted { .. }));
        undone += 1;
    }
    assert_eq!(undone, UNDO_DEPTH);
    let mut redone = Vec::new();
    while let Some(outcome) = f.store.redo(now()).unwrap() {
        let RedoOutcome::Reapplied { label, .. } = outcome else {
            panic!("expected a reapply");
        };
        redone.push(label);
    }
    assert_eq!(redone.len(), UNDO_DEPTH);
    // Newest undo first: the oldest undone action comes back first.
    assert_eq!(redone.first().map(String::as_str), Some("Archive e3"));
    assert_eq!(redone.last().map(String::as_str), Some("Archive e12"));
    f.check_consistency();
}

#[test]
fn redo_after_the_undo_failed_is_too_late() {
    let mut f = fixture_with(&["e1"]);
    f.store
        .enqueue(archive("e1"), undoable("Archive"), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.store.undo(now()).unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    f.store
        .finish_flush(
            &unit,
            FlushOutcome::Failed {
                error: "forbidden".into(),
            },
            now(),
        )
        .unwrap();
    assert!(matches!(
        f.store.redo(now()).unwrap(),
        Some(RedoOutcome::TooLate { label, .. }) if label == "Archive"
    ));
    assert_eq!(f.store.redo(now()).unwrap(), None);
}

#[test]
fn redo_skips_targets_the_server_rejected() {
    let mut f = fixture_with(&["e1", "e2"]);
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![
                EmailPatchTarget::new("e1", Patch::new().keyword("$flagged", true)),
                EmailPatchTarget::new("e2", Patch::new().keyword("$flagged", true)),
            ]),
            undoable("Pin 2"),
            now(),
        )
        .unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.store.undo(now()).unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    let mut rejected = BTreeMap::new();
    rejected.insert(Id::from("e2"), "notFound".to_owned());
    f.store
        .finish_flush(
            &unit,
            FlushOutcome::Applied {
                created_id: None,
                rejected,
            },
            now(),
        )
        .unwrap();
    let Some(RedoOutcome::Reapplied { seq, .. }) = f.store.redo(now()).unwrap() else {
        panic!("expected a reapply");
    };
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    assert_eq!(unit.seqs, [seq]);
    let mejl_store::FlushOp::EmailSet { update, .. } = &unit.op else {
        panic!("expected an email set");
    };
    assert_eq!(update.keys().map(Id::as_str).collect::<Vec<_>>(), ["e1"]);
}

#[test]
fn undo_of_a_send_cancels_and_is_not_redoable() {
    let mut f = fixture_with(&[]);
    f.store
        .enqueue(
            Mutation::EmailSubmit {
                payload: serde_json::json!({"subject": "Hi"}),
                hold_until: Some(ts("2026-10-23T15:00:00Z")),
            },
            undoable("Send"),
            now(),
        )
        .unwrap();
    assert!(matches!(
        f.store.undo(now()).unwrap(),
        Some(UndoOutcome::Cancelled {
            payload: Some(_),
            ..
        })
    ));
    assert_eq!(f.store.redo_peek().unwrap(), None);
}

#[test]
fn prune_keeps_entries_the_redo_stack_needs() {
    let mut f = fixture_with(&["e1"]);
    f.store
        .enqueue(archive("e1"), undoable("Archive"), now())
        .unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    f.store.undo(now()).unwrap();
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    f.store
        .finish_flush(&unit, FlushOutcome::ok(), now())
        .unwrap();
    // Confirm both overlays so the entries are prunable apart from the
    // stacks.
    f.changes(&[flags("e1", &[INBOX], &[])], &[]);
    f.store.prune_outbox(ts("2027-01-01T00:00:00Z")).unwrap();
    assert!(matches!(
        f.store.redo(now()).unwrap(),
        Some(RedoOutcome::Reapplied { .. })
    ));
}

#[test]
fn state_db_migrates_from_version_one() {
    let f = fixture_with(&[]);
    let dir = f.dir;
    drop(f.store);
    let path = dir.path().join("acct").join("state.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        // Undo versions 2 and 3, so the file looks like a version 1 one.
        conn.execute_batch(
            "DROP TABLE redo; DROP TABLE draft_trash;
             ALTER TABLE draft DROP COLUMN upload_json;
             ALTER TABLE draft DROP COLUMN upload_rev;
             ALTER TABLE draft DROP COLUMN uploaded_rev;
             ALTER TABLE draft DROP COLUMN upload_in_flight;
             ALTER TABLE draft DROP COLUMN server_message_id;
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let mut store =
        mejl_store::Store::open(dir.path().join("acct"), Default::default()).expect("migrates");
    assert_eq!(store.redo(now()).unwrap(), None);
    assert!(store.draft_uploads().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
}

#[test]
fn mailbox_email_ids_follow_effective_membership() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("e1").thread("t1").at("2026-10-20T09:00:00Z").build(),
        email("e2").thread("t1").at("2026-10-20T10:00:00Z").build(),
        email("e3").thread("t3").at("2026-10-20T11:00:00Z").build(),
        email("e4")
            .thread("t4")
            .mailboxes(&[ARCHIVE])
            .at("2026-10-20T12:00:00Z")
            .build(),
    ]);
    let ids = |f: &Fixture, except: &[&str]| -> Vec<String> {
        let except: Vec<Id> = except.iter().map(|t| Id::from(*t)).collect();
        f.store
            .mailbox_email_ids(INBOX, &except)
            .unwrap()
            .into_iter()
            .map(Id::into_inner)
            .collect()
    };
    assert_eq!(ids(&f, &[]), ["e3", "e2", "e1"]);
    assert_eq!(ids(&f, &["t1"]), ["e3"]);
    assert!(ids(&f, &["t1", "t3"]).is_empty());
    // A pending move out of the Inbox and a pending delete both count.
    f.store
        .enqueue(archive("e3"), EnqueueOptions::default(), now())
        .unwrap();
    f.store
        .enqueue(
            Mutation::EmailDestroy(vec![Id::from("e1")]),
            EnqueueOptions::default(),
            now(),
        )
        .unwrap();
    assert_eq!(ids(&f, &[]), ["e2"]);
    assert!(
        f.store
            .mailbox_email_ids("no-such-mailbox", &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn membership_and_cache_listing_reads() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("e1").thread("t1").build(),
        email("e2").thread("t2").mailboxes(&[INBOX, GARDEN]).build(),
    ]);
    let all = [Id::from("e1"), Id::from("e2"), Id::from("nope")];
    let members = f.store.email_mailboxes(&all).unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[&Id::from("e1")], [Id::from(INBOX)]);
    let mut e2: Vec<_> = members[&Id::from("e2")].clone();
    e2.sort();
    assert_eq!(e2, [Id::from(GARDEN), Id::from(INBOX)]);

    assert_eq!(f.store.missing_emails(&all).unwrap(), [Id::from("nope")]);
    assert!(f.store.missing_emails(&[]).unwrap().is_empty());

    let mut cached = f.store.cached_email_ids().unwrap();
    cached.sort();
    assert_eq!(cached, [Id::from("e1"), Id::from("e2")]);
    assert_eq!(
        f.store.cached_thread_ids().unwrap(),
        [Id::from("t1"), Id::from("t2")]
    );
}

#[test]
fn next_flush_time_reports_holds_and_backoff() {
    let mut f = fixture_with(&["e1"]);
    assert_eq!(f.store.next_flush_time().unwrap(), None);
    let hold = ts("2026-10-23T14:27:10Z");
    f.store
        .enqueue(
            Mutation::EmailSubmit {
                payload: serde_json::json!({}),
                hold_until: Some(hold),
            },
            EnqueueOptions::default(),
            now(),
        )
        .unwrap();
    assert_eq!(f.store.next_flush_time().unwrap(), Some(hold));

    f.store
        .enqueue(archive("e1"), EnqueueOptions::default(), now())
        .unwrap();
    // A ready entry is due at once.
    assert!(f.store.next_flush_time().unwrap().unwrap() <= now());
    let unit = f.store.next_flush(now(), 100).unwrap().unwrap();
    let later = ts("2026-10-23T14:27:05Z");
    f.store
        .finish_flush(
            &unit,
            FlushOutcome::Retry {
                error: "offline".into(),
                not_before: Some(later),
            },
            now(),
        )
        .unwrap();
    assert_eq!(f.store.next_flush_time().unwrap(), Some(later));
}

#[test]
fn stack_targets_name_the_next_undo_and_redo() {
    let mut f = fixture_with(&["e1", "e2"]);
    assert!(f.store.undo_targets().unwrap().is_empty());
    assert!(f.store.redo_targets().unwrap().is_empty());
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![
                EmailPatchTarget::new("e2", Patch::new().keyword("$seen", true)),
                EmailPatchTarget::new("e1", Patch::new().keyword("$seen", true)),
            ]),
            undoable("Mark read 2"),
            now(),
        )
        .unwrap();
    assert_eq!(
        f.store.undo_targets().unwrap(),
        [Id::from("e1"), Id::from("e2")]
    );
    f.store.undo(now()).unwrap();
    assert!(f.store.undo_targets().unwrap().is_empty());
    assert_eq!(
        f.store.redo_targets().unwrap(),
        [Id::from("e1"), Id::from("e2")]
    );
}
