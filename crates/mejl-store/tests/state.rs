//! Drafts, rules, render history and UI state in state.db.

mod common;

use common::*;
use mejl_jmap::Id;
use mejl_jmap::types::EmailAddress;
use mejl_store::{Draft, DraftHeaders, DraftUploadDone, Error, RuleRecord};
use serde_json::json;

fn draft(id: &str, body: &str, at: &str) -> Draft {
    Draft {
        local_id: id.into(),
        server_email_id: None,
        identity_id: Some(Id::from("I1")),
        headers: DraftHeaders {
            from: Some(EmailAddress::with_name("Mira Holt", "mira@example.org")),
            to: vec![EmailAddress::new("bookings@cabins.example.net")],
            subject: "Booking for 14 to 16 November".into(),
            ..Default::default()
        },
        body: body.into(),
        format: "markdown".into(),
        attachments: json!([{"path": "/tmp/floorplan.pdf", "size": 212000}]),
        created_at: ts("2026-10-23T10:00:00Z"),
        updated_at: ts(at),
    }
}

#[test]
fn drafts_save_replace_and_delete() {
    let mut f = Fixture::new();
    f.store
        .save_draft(&draft("d1", "Hi,", "2026-10-23T10:00:01Z"))
        .unwrap();
    f.store
        .save_draft(&draft("d2", "Other", "2026-10-23T10:05:00Z"))
        .unwrap();
    f.store
        .save_draft(&draft(
            "d1",
            "Hi,\n\nWe would like to book 🏡",
            "2026-10-23T10:10:00Z",
        ))
        .unwrap();
    let reader = f.reader();
    let all = reader.drafts().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].local_id, "d1", "most recently changed first");
    assert_eq!(all[0].body, "Hi,\n\nWe would like to book 🏡");
    assert_eq!(
        all[0].created_at,
        ts("2026-10-23T10:00:00Z"),
        "created_at is kept"
    );
    assert_eq!(
        all[0],
        draft(
            "d1",
            "Hi,\n\nWe would like to book 🏡",
            "2026-10-23T10:10:00Z"
        )
    );

    assert!(
        f.store
            .set_draft_server_id("d1", Some(&Id::from("M77")))
            .unwrap()
    );
    assert_eq!(
        reader.draft("d1").unwrap().unwrap().server_email_id,
        Some(Id::from("M77"))
    );
    assert!(!f.store.set_draft_server_id("nope", None).unwrap());
    assert!(f.store.delete_draft("d2", now()).unwrap());
    assert!(!f.store.delete_draft("d2", now()).unwrap());
    assert!(reader.draft("d2").unwrap().is_none());
    assert!(matches!(
        f.store.save_draft(&draft("", "x", "2026-10-23T10:00:00Z")),
        Err(Error::Invalid(_))
    ));

    // Drafts survive the cache being thrown away.
    let f = f.reopen();
    assert_eq!(f.reader().drafts().unwrap().len(), 1);
}

#[test]
fn a_save_keeps_the_server_copy_the_engine_recorded() {
    let mut f = Fixture::new();
    f.store
        .save_draft(&draft("d1", "Hi,", "2026-10-23T10:00:01Z"))
        .unwrap();
    f.store
        .set_draft_server_id("d1", Some(&Id::from("M2")))
        .unwrap();
    // The caller read the row before the engine recorded M2.
    let mut stale = draft("d1", "Hi, again", "2026-10-23T10:00:05Z");
    stale.server_email_id = Some(Id::from("M1"));
    f.store.save_draft(&stale).unwrap();
    let saved = f.reader().draft("d1").unwrap().unwrap();
    assert_eq!(saved.server_email_id, Some(Id::from("M2")));
    assert_eq!(saved.body, "Hi, again");

    // A draft opened from the server brings its id along.
    let mut opened = draft("d9", "From the web", "2026-10-23T10:00:05Z");
    opened.server_email_id = Some(Id::from("W1"));
    f.store.save_draft(&opened).unwrap();
    assert_eq!(
        f.reader().draft("d9").unwrap().unwrap().server_email_id,
        Some(Id::from("W1"))
    );
}

#[test]
fn server_draft_uploads_coalesce_and_record_the_sent_revision() {
    let mut f = Fixture::new();
    assert!(
        !f.store
            .queue_draft_upload("d1", &json!({"subject": "no draft"}))
            .unwrap(),
        "no local draft, nothing to upload"
    );
    f.store
        .save_draft(&draft("d1", "Hi,", "2026-10-23T10:00:01Z"))
        .unwrap();
    assert!(f.store.draft_uploads().unwrap().is_empty());
    f.store.queue_draft_upload("d1", &json!({"v": 1})).unwrap();
    f.store.queue_draft_upload("d1", &json!({"v": 2})).unwrap();
    let up = f.store.draft_uploads().unwrap();
    assert_eq!(up.len(), 1, "one entry per draft");
    assert_eq!((up[0].rev, &up[0].payload), (2, &json!({"v": 2})));
    assert!(!up[0].uncertain);
    assert_eq!(up[0].message_id, None);

    let mid = f
        .store
        .begin_draft_upload("d1", "first@example.org")
        .unwrap();
    assert_eq!(mid.as_deref(), Some("first@example.org"));
    // A later upload keeps the draft's Message-ID.
    let again = f
        .store
        .begin_draft_upload("d1", "other@example.org")
        .unwrap();
    assert_eq!(again.as_deref(), Some("first@example.org"));
    assert!(f.store.draft_uploads().unwrap()[0].uncertain);
    assert_eq!(
        f.store.begin_draft_upload("nope", "x@example.org").unwrap(),
        None
    );

    // A newer revision arrives while revision 2 is in flight.
    f.store.queue_draft_upload("d1", &json!({"v": 3})).unwrap();
    let still_there = f
        .store
        .finish_draft_upload(
            &DraftUploadDone {
                local_id: "d1".into(),
                rev: 2,
                uploaded: true,
                server_email_id: Some(Id::from("M2")),
                trash: vec![],
            },
            now(),
        )
        .unwrap();
    assert!(still_there);
    let up = f.store.draft_uploads().unwrap();
    assert_eq!(up.len(), 1, "revision 3 is still to go");
    assert_eq!(up[0].rev, 3);
    assert_eq!(up[0].server_email_id, Some(Id::from("M2")));
    assert!(!up[0].uncertain);

    f.store
        .finish_draft_upload(
            &DraftUploadDone {
                local_id: "d1".into(),
                rev: 3,
                uploaded: true,
                server_email_id: Some(Id::from("M3")),
                trash: vec![Id::from("M2")],
            },
            now(),
        )
        .unwrap();
    assert!(f.store.draft_uploads().unwrap().is_empty());
    let trash = f.store.draft_trash(10).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].email_id, Some(Id::from("M2")));
    f.store.clear_draft_trash(&[trash[0].id]).unwrap();
    assert!(f.store.draft_trash(10).unwrap().is_empty());
}

#[test]
fn deleting_a_draft_trashes_its_server_copies() {
    let mut f = Fixture::new();
    f.store
        .save_draft(&draft("d1", "Hi,", "2026-10-23T10:00:01Z"))
        .unwrap();
    f.store.queue_draft_upload("d1", &json!({"v": 1})).unwrap();
    f.store.begin_draft_upload("d1", "d1@example.org").unwrap();
    f.store
        .set_draft_server_id("d1", Some(&Id::from("M1")))
        .unwrap();
    assert!(f.store.delete_draft("d1", now()).unwrap());
    let trash = f.store.draft_trash(10).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].email_id, Some(Id::from("M1")));
    assert_eq!(trash[0].message_id.as_deref(), Some("d1@example.org"));
    assert!(f.store.draft_uploads().unwrap().is_empty());

    // An upload that lands after the delete trashes the copy it made.
    let still_there = f
        .store
        .finish_draft_upload(
            &DraftUploadDone {
                local_id: "d1".into(),
                rev: 1,
                uploaded: true,
                server_email_id: Some(Id::from("M2")),
                trash: vec![],
            },
            now(),
        )
        .unwrap();
    assert!(!still_there);
    let ids: Vec<Option<Id>> = f
        .store
        .draft_trash(10)
        .unwrap()
        .into_iter()
        .map(|t| t.email_id)
        .collect();
    assert!(ids.contains(&Some(Id::from("M2"))), "{ids:?}");

    // The trash survives a restart.
    f.store
        .trash_draft_copies(&[Id::from("M9")], &["lost@example.org"], now())
        .unwrap();
    let f = f.reopen();
    assert_eq!(f.store.draft_trash(10).unwrap().len(), 4);
    assert_eq!(f.store.draft_trash(2).unwrap().len(), 2);
    // A draft without a server copy leaves nothing behind.
    let mut f = f;
    f.store
        .save_draft(&draft("d2", "Local", "2026-10-23T10:00:01Z"))
        .unwrap();
    f.store.delete_draft("d2", now()).unwrap();
    assert_eq!(f.store.draft_trash(10).unwrap().len(), 4);
}

fn rule(id: &str, order: i64) -> RuleRecord {
    RuleRecord {
        id: id.into(),
        name: format!("Rule {id}"),
        enabled: true,
        sort_order: order,
        rule_json: json!({"id": id, "match_mode": "all"}).to_string(),
        updated_at: now(),
    }
}

#[test]
fn rules_order_enable_and_delete() {
    let mut f = Fixture::new();
    for (id, order) in [("receipts", 2), ("garden", 1), ("news", 3)] {
        f.store.save_rule(&rule(id, order)).unwrap();
    }
    let reader = f.reader();
    let order = |r: &mejl_store::Reader| -> Vec<String> {
        r.rules().unwrap().into_iter().map(|r| r.id).collect()
    };
    assert_eq!(order(&reader), vec!["garden", "receipts", "news"]);

    f.store.reorder_rules(&["news", "garden"], now()).unwrap();
    assert_eq!(order(&reader), vec!["news", "garden", "receipts"]);
    assert!(matches!(
        f.store.reorder_rules(&["missing"], now()),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        order(&reader),
        vec!["news", "garden", "receipts"],
        "a failed reorder changes nothing"
    );

    assert!(f.store.set_rule_enabled("news", false, now()).unwrap());
    assert!(!reader.rules().unwrap()[0].enabled);
    assert!(!f.store.set_rule_enabled("missing", false, now()).unwrap());

    let mut renamed = rule("garden", 1);
    renamed.name = "Garden club ✿".into();
    f.store.save_rule(&renamed).unwrap();
    assert!(
        reader
            .rules()
            .unwrap()
            .iter()
            .any(|r| r.name == "Garden club ✿")
    );
    assert!(f.store.delete_rule("news").unwrap());
    assert_eq!(order(&reader), vec!["garden", "receipts"]);
}

#[test]
fn render_history() {
    let mut f = Fixture::new();
    let first = f
        .store
        .record_rule_render(
            "# mejl rules v1\n",
            &"a".repeat(64),
            ts("2026-10-20T10:00:00Z"),
        )
        .unwrap();
    let second = f
        .store
        .record_rule_render(
            "# mejl rules v1\nif true {}\n",
            &"b".repeat(64),
            ts("2026-10-21T10:00:00Z"),
        )
        .unwrap();
    assert!(
        f.store
            .confirm_rule_paste(first, ts("2026-10-20T10:05:00Z"))
            .unwrap()
    );
    assert!(!f.store.confirm_rule_paste(999, now()).unwrap());
    let renders = f.reader().rule_renders(10).unwrap();
    assert_eq!(renders.len(), 2);
    assert_eq!(renders[0].id, second);
    assert_eq!(renders[0].user_confirmed_paste_at, None);
    assert_eq!(
        renders[1].user_confirmed_paste_at,
        Some(ts("2026-10-20T10:05:00Z"))
    );
    assert_eq!(renders[1].body_sha256, "a".repeat(64));
    assert_eq!(f.reader().rule_renders(1).unwrap().len(), 1);
}

#[test]
fn ui_values() {
    let mut f = Fixture::new();
    f.store.set_ui_value("last_mailbox", INBOX, now()).unwrap();
    f.store.set_ui_value("pane_ratio", "0.35", now()).unwrap();
    f.store.set_ui_value("pane_ratio", "0.40", now()).unwrap();
    let reader = f.reader();
    assert_eq!(
        reader.ui_value("pane_ratio").unwrap().as_deref(),
        Some("0.40")
    );
    assert_eq!(
        f.store.ui_value("last_mailbox").unwrap().as_deref(),
        Some(INBOX)
    );
    assert!(f.store.delete_ui_value("pane_ratio").unwrap());
    assert_eq!(reader.ui_value("pane_ratio").unwrap(), None);
}

#[test]
fn newer_state_db_is_refused_and_left_alone() {
    let f = Fixture::new();
    let dir = f.dir.path().join("acct");
    let Fixture { dir: tmp, store } = f;
    drop(store);
    let conn = rusqlite::Connection::open(dir.join("state.db")).unwrap();
    conn.pragma_update(None, "user_version", 42).unwrap();
    drop(conn);
    let err = mejl_store::Store::open(&dir, Default::default()).unwrap_err();
    assert!(matches!(err, Error::StateTooNew { found: 42, .. }));
    let conn = rusqlite::Connection::open(dir.join("state.db")).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 42);
    drop(tmp);
}

#[cfg(unix)]
#[test]
fn account_directory_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let mode = std::fs::metadata(f.dir.path().join("acct"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}
