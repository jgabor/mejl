//! The read model and its pending overlay against a real store: hidden
//! rows shift pages, positions and totals; flags and counts follow the
//! snapshot; entries drop cleanly.

use jiff::tz::TimeZone;
use mejl::app::data::StoreData;
use mejl::app::mutate::{self, Plan};
use mejl_jmap::method::GetResponse;
use mejl_jmap::types::{Email, Mailbox};
use mejl_store::{Store, StoreOptions};
use mejl_ui::{self as ui, MailData, Mutation, Target, TargetIds};
use serde_json::json;

const INBOX: &str = "mb-inbox";
const ARCHIVE: &str = "mb-archive";

fn mailbox(id: &str, name: &str, role: &str, unread_threads: u64) -> Mailbox {
    serde_json::from_value(json!({
        "id": id, "name": name, "role": role, "sortOrder": 10,
        "totalEmails": 5, "unreadEmails": unread_threads,
        "totalThreads": 5, "unreadThreads": unread_threads, "isSubscribed": true
    }))
    .unwrap()
}

fn email(n: u32, seen: bool) -> Email {
    let keywords = if seen {
        json!({"$seen": true})
    } else {
        json!({})
    };
    serde_json::from_value(json!({
        "id": format!("e{n}"),
        "threadId": format!("t{n}"),
        "mailboxIds": {INBOX: true},
        "keywords": keywords,
        "receivedAt": format!("2026-10-2{n}T09:00:00Z"),
        "size": 1000,
        "from": [{"name": format!("Sender {n}"), "email": format!("s{n}@example.org")}],
        "to": [{"name": "Mira Holt", "email": "mira@example.org"}],
        "subject": format!("Subject {n}"),
        "preview": "",
        "hasAttachment": false,
    }))
    .unwrap()
}

fn setup() -> (tempfile::TempDir, Store, StoreData) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
    let mailboxes: GetResponse<Mailbox> = serde_json::from_value(json!({
        "accountId": "A1",
        "state": "m1",
        "list": [
            mailbox(INBOX, "Inbox", "inbox", 2),
            mailbox(ARCHIVE, "Archive", "archive", 0),
        ],
        "notFound": []
    }))
    .unwrap();
    let mut batch = store.batch().unwrap();
    batch.replace_mailboxes(&mailboxes).unwrap();
    // Newest first in the list: t5, t4, t3, t2, t1. t3 and t5 are unread.
    let emails: Vec<Email> = (1..=5).map(|n| email(n, n != 3 && n != 5)).collect();
    batch.upsert_emails(&emails).unwrap();
    batch.commit().unwrap();
    let data = StoreData::new(store.reader().unwrap(), "test", TimeZone::UTC);
    (dir, store, data)
}

fn inbox() -> ui::MailboxId {
    ui::MailboxId::new(INBOX)
}

fn threads(data: &StoreData, offset: usize, limit: usize) -> (Vec<String>, usize) {
    let page = data.conversation_page(&inbox(), offset, limit);
    (
        page.items
            .iter()
            .map(|r| r.thread_id.as_str().to_owned())
            .collect(),
        page.total,
    )
}

fn archive(thread: &str) -> Mutation {
    Mutation::Archive(Target {
        ids: TargetIds::Threads(vec![ui::ThreadId::new(thread)]),
        from_mailbox: Some(inbox()),
    })
}

fn queue(data: &mut StoreData, token: u64, m: &Mutation) {
    match mutate::plan(data, m, false).unwrap() {
        Plan::Queue { snapshots, .. } => data.pending_mut().add_threads(token, snapshots),
        Plan::Bulk { overlay, .. } => data.pending_mut().add_bulk(token, overlay),
        other => panic!("unexpected plan {other:?}"),
    }
}

#[test]
fn hidden_rows_shift_pages_positions_and_totals() {
    let (_dir, _store, mut data) = setup();
    assert_eq!(
        threads(&data, 0, 10),
        (
            vec!["t5", "t4", "t3", "t2", "t1"]
                .into_iter()
                .map(String::from)
                .collect(),
            5
        )
    );
    queue(&mut data, 1, &archive("t4"));
    queue(&mut data, 2, &archive("t2"));
    let (all, total) = threads(&data, 0, 10);
    assert_eq!(all, ["t5", "t3", "t1"]);
    assert_eq!(total, 3);
    // Pages at any offset see the same virtual list.
    assert_eq!(threads(&data, 1, 1).0, ["t3"]);
    assert_eq!(threads(&data, 2, 5).0, ["t1"]);
    assert!(threads(&data, 3, 5).0.is_empty());
    let pos = |t: &str| data.conversation_index(&inbox(), &ui::ThreadId::new(t));
    assert_eq!(pos("t5"), Some(0));
    assert_eq!(pos("t4"), None);
    assert_eq!(pos("t3"), Some(1));
    assert_eq!(pos("t1"), Some(2));
    // Counts move with the snapshots.
    let boxes = data.mailboxes();
    let get = |id: &str| boxes.iter().find(|m| m.id.as_str() == id).unwrap().clone();
    assert_eq!(get(INBOX).total_threads, 3);
    assert_eq!(get(ARCHIVE).total_threads, 7);
    // Dropping an entry brings its row back.
    assert_eq!(data.pending_mut().remove(1), Some(vec!["t4".to_owned()]));
    assert_eq!(threads(&data, 0, 10).0, ["t5", "t4", "t3", "t1"]);
}

#[test]
fn flags_and_the_reader_view_follow_the_overlay() {
    let (_dir, _store, mut data) = setup();
    let read = Mutation::SetKeyword {
        target: Target {
            ids: TargetIds::Emails(vec![ui::EmailId::new("e3")]),
            from_mailbox: Some(inbox()),
        },
        keyword: ui::Keyword::Seen,
        value: true,
    };
    let row = |data: &StoreData| {
        data.conversation_page(&inbox(), 0, 10)
            .items
            .into_iter()
            .find(|r| r.thread_id.as_str() == "t3")
            .unwrap()
    };
    assert!(row(&data).unread);
    queue(&mut data, 1, &read);
    assert!(!row(&data).unread);
    let conv = data.conversation(&ui::ThreadId::new("t3")).unwrap();
    assert!(!conv.messages[0].unread);
    let inbox_view = data
        .mailboxes()
        .into_iter()
        .find(|m| m.id.as_str() == INBOX)
        .unwrap();
    assert_eq!(inbox_view.unread_threads, 1);
    data.pending_mut().remove(1);
    assert!(row(&data).unread);
}

#[test]
fn select_all_moves_hide_everything_but_the_exceptions() {
    let (_dir, _store, mut data) = setup();
    let m = Mutation::Archive(Target {
        ids: TargetIds::AllInMailbox {
            mailbox: inbox(),
            except: vec![ui::ThreadId::new("t2"), ui::ThreadId::new("t5")],
        },
        from_mailbox: Some(inbox()),
    });
    queue(&mut data, 1, &m);
    assert_eq!(
        threads(&data, 0, 10),
        (vec!["t5".to_owned(), "t2".to_owned()], 2)
    );
    assert_eq!(threads(&data, 1, 10).0, ["t2"]);
    assert_eq!(
        data.conversation_index(&inbox(), &ui::ThreadId::new("t2")),
        Some(1)
    );
    assert_eq!(
        data.conversation_index(&inbox(), &ui::ThreadId::new("t3")),
        None
    );
}

#[test]
fn moves_that_change_nothing_are_refused_in_words() {
    let (_dir, _store, data) = setup();
    let m = Mutation::Archive(Target {
        ids: TargetIds::Threads(vec![ui::ThreadId::new("t1")]),
        from_mailbox: Some(ui::MailboxId::new(ARCHIVE)),
    });
    let err = mutate::plan(&data, &m, false).unwrap_err();
    assert!(
        err.contains("Already in Archive") || err.contains("Nothing"),
        "{err}"
    );
    let trash = Mutation::Trash(Target {
        ids: TargetIds::Threads(vec![ui::ThreadId::new("t1")]),
        from_mailbox: Some(inbox()),
    });
    let err = mutate::plan(&data, &trash, false).unwrap_err();
    assert!(err.contains("no Trash mailbox"), "{err}");
    let label = Mutation::SetMailbox {
        target: Target {
            ids: TargetIds::AllInMailbox {
                mailbox: inbox(),
                except: vec![],
            },
            from_mailbox: Some(inbox()),
        },
        mailbox: ui::MailboxId::new(ARCHIVE),
        member: true,
    };
    assert!(mutate::plan(&data, &label, false).is_err());
}

#[test]
fn destroys_are_never_undoable() {
    let (_dir, _store, data) = setup();
    let m = Mutation::DeletePermanently(Target {
        ids: TargetIds::Threads(vec![ui::ThreadId::new("t1")]),
        from_mailbox: Some(inbox()),
    });
    match mutate::plan(&data, &m, false).unwrap() {
        Plan::Queue {
            mutation,
            undoable,
            snapshots,
            ..
        } => {
            assert!(!undoable);
            assert!(
                matches!(mutation, mejl_store::Mutation::EmailDestroy(ref ids) if ids.len() == 1)
            );
            assert!(!snapshots[0].in_mailbox_after(INBOX));
        }
        other => panic!("{other:?}"),
    }
}

fn queue_with(data: &mut StoreData, token: u64, m: &Mutation, mark_read: bool) {
    match mutate::plan(data, m, mark_read).unwrap() {
        Plan::Queue { snapshots, .. } => data.pending_mut().add_threads(token, snapshots),
        Plan::Bulk { overlay, .. } => data.pending_mut().add_bulk(token, overlay),
        other => panic!("unexpected plan {other:?}"),
    }
}

fn archive_unread(data: &StoreData) -> u32 {
    data.mailboxes()
        .iter()
        .find(|m| m.id.as_str() == ARCHIVE)
        .map(|m| m.unread_threads)
        .unwrap()
}

#[test]
fn archive_marks_read_only_when_the_setting_is_on() {
    // t3 is unread: archived as it is, the Archive mailbox gains an unread.
    let (_dir, _store, mut data) = setup();
    queue_with(&mut data, 1, &archive("t3"), false);
    assert_eq!(archive_unread(&data), 1);

    let (_dir, _store, mut data) = setup();
    queue_with(&mut data, 1, &archive("t3"), true);
    assert_eq!(archive_unread(&data), 0);
    // Undo drops the entry, which brings the unread state back with the row.
    assert!(data.pending_mut().remove(1).is_some());
    assert_eq!(threads(&data, 0, 10).0, ["t5", "t4", "t3", "t2", "t1"]);
}

#[test]
fn bulk_archive_carries_the_mark_read_flag() {
    let all = Mutation::Archive(Target {
        ids: TargetIds::AllInMailbox {
            mailbox: inbox(),
            except: vec![],
        },
        from_mailbox: Some(inbox()),
    });
    let (_dir, _store, data) = setup();
    match mutate::plan(&data, &all, true).unwrap() {
        Plan::Bulk {
            action, overlay, ..
        } => {
            assert_eq!(action, mejl_sync::BulkAction::ArchiveRead);
            assert_eq!(overlay.seen, Some(true));
            assert!(overlay.hide);
        }
        other => panic!("unexpected plan {other:?}"),
    }
    match mutate::plan(&data, &all, false).unwrap() {
        Plan::Bulk {
            action, overlay, ..
        } => {
            assert_eq!(action, mejl_sync::BulkAction::Archive);
            assert_eq!(overlay.seen, None);
        }
        other => panic!("unexpected plan {other:?}"),
    }
}

#[test]
fn the_search_filter_preview_resolves_against_the_store() {
    let (_dir, _store, data) = setup();
    assert_eq!(
        data.search_filter_preview("in:archive from:juniper")
            .as_deref(),
        Some(r#"{"operator":"AND","conditions":[{"inMailbox":"mb-archive"},{"from":"juniper"}]}"#)
    );
    assert_eq!(
        data.search_filter_preview("after:2026-10-01").as_deref(),
        Some(r#"{"after":"2026-10-01T00:00:00Z"}"#)
    );
    // Every message: JMAP gets no filter.
    assert_eq!(data.search_filter_preview("").as_deref(), Some("null"));
    assert_eq!(
        data.search_filter_preview("in:nowhere").as_deref(),
        Some("null")
    );
}
