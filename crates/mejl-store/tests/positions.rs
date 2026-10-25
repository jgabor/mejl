//! Conversation positions and recent messages, for the UI's read model.

mod common;

use common::*;
use mejl_store::{CACHE_FILE, EmailPatchTarget, EnqueueOptions, Mutation, STATE_FILE, STORE_FILES};

fn three_threads() -> Fixture {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("e1").thread("t1").at("2026-10-20T09:00:00Z").build(),
        email("e2").thread("t2").at("2026-10-21T09:00:00Z").build(),
        email("e3").thread("t3").at("2026-10-22T09:00:00Z").build(),
        // A second message of t1 in Archive only, newer than all.
        email("e4")
            .thread("t1")
            .at("2026-10-23T09:00:00Z")
            .mailboxes(&[ARCHIVE])
            .build(),
    ]);
    f
}

#[test]
fn position_matches_the_page_order() {
    let f = three_threads();
    let r = f.reader();
    let page = r.conversations(INBOX, 0, 10).unwrap();
    let order: Vec<&str> = page.rows.iter().map(|c| c.thread_id.as_str()).collect();
    assert_eq!(order, ["t3", "t2", "t1"]);
    for (i, t) in order.iter().enumerate() {
        assert_eq!(r.conversation_position(INBOX, t).unwrap(), Some(i), "{t}");
    }
    assert_eq!(r.conversation_position(ARCHIVE, "t1").unwrap(), Some(0));
    assert_eq!(r.conversation_position(ARCHIVE, "t2").unwrap(), None);
    assert_eq!(r.conversation_position(INBOX, "nope").unwrap(), None);
}

#[test]
fn position_ties_break_like_the_page() {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("a").thread("ta").at("2026-10-20T09:00:00Z").build(),
        email("b").thread("tb").at("2026-10-20T09:00:00Z").build(),
        email("c").thread("tc").at("2026-10-20T09:00:00Z").build(),
    ]);
    let r = f.reader();
    let page = r.conversations(INBOX, 0, 10).unwrap();
    for (i, row) in page.rows.iter().enumerate() {
        assert_eq!(
            r.conversation_position(INBOX, row.thread_id.as_str())
                .unwrap(),
            Some(i)
        );
    }
}

#[test]
fn position_follows_pending_moves() {
    let mut f = three_threads();
    let patch = mejl_jmap::Patch::new()
        .mailbox(INBOX, false)
        .mailbox(ARCHIVE, true);
    f.store
        .enqueue(
            Mutation::EmailPatch(vec![EmailPatchTarget::new("e3", patch)]),
            EnqueueOptions {
                label: Some("Archive"),
                undoable: true,
            },
            now(),
        )
        .unwrap();
    let r = f.reader();
    assert_eq!(r.conversation_position(INBOX, "t3").unwrap(), None);
    assert_eq!(r.conversation_position(INBOX, "t2").unwrap(), Some(0));
}

#[test]
fn recent_messages_are_newest_first_and_bounded() {
    let f = three_threads();
    let r = f.reader();
    let recent = r.recent_messages(3).unwrap();
    let ids: Vec<&str> = recent.iter().map(|m| m.row.email_id.as_str()).collect();
    assert_eq!(ids, ["e4", "e3", "e2"]);
    assert!(recent[0].mailbox_ids.contains(ARCHIVE));
    assert_eq!(r.recent_messages(0).unwrap().len(), 0);
    assert_eq!(r.recent_messages(50).unwrap().len(), 4);
}

#[test]
fn recent_messages_skip_pending_deletes() {
    let mut f = three_threads();
    f.store
        .enqueue(
            Mutation::EmailDestroy(vec![mejl_jmap::Id::from("e4")]),
            EnqueueOptions {
                label: Some("Delete"),
                undoable: false,
            },
            now(),
        )
        .unwrap();
    let ids: Vec<String> = f
        .reader()
        .recent_messages(10)
        .unwrap()
        .into_iter()
        .map(|m| m.row.email_id.to_string())
        .collect();
    assert_eq!(ids, ["e3", "e2", "e1"]);
}

#[test]
fn file_names_are_exported() {
    let f = Fixture::new();
    let dir = f.dir.path().join("acct");
    assert_eq!(STORE_FILES, [CACHE_FILE, STATE_FILE]);
    for name in STORE_FILES {
        assert!(dir.join(name).is_file(), "{name}");
    }
}
