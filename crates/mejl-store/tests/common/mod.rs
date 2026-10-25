//! Helpers shared by the integration tests.

#![allow(dead_code)]

use jiff::Timestamp;
use mejl_jmap::method::{ChangesResponse, GetResponse};
use mejl_jmap::types::{Email, Mailbox};
use mejl_jmap::{Id, State};
use mejl_store::{Reader, Store, StoreOptions};
use rusqlite::Connection;
use serde_json::{Value, json};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub store: Store,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_options(StoreOptions::default())
    }

    pub fn with_options(options: StoreOptions) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(dir.path().join("acct"), options).expect("open store");
        Fixture { dir, store }
    }

    pub fn reader(&self) -> Reader {
        self.store.reader().expect("reader")
    }

    pub fn reopen(self) -> Self {
        let Fixture { dir, store } = self;
        drop(store);
        let store = Store::open(dir.path().join("acct"), StoreOptions::default()).expect("reopen");
        Fixture { dir, store }
    }

    pub fn cache(&self) -> Connection {
        Connection::open(self.dir.path().join("acct").join("cache.db")).expect("cache.db")
    }

    pub fn state(&self) -> Connection {
        Connection::open(self.dir.path().join("acct").join("state.db")).expect("state.db")
    }

    /// The standard mailboxes and two folders.
    pub fn with_mailboxes(mut self) -> Self {
        let mut batch = self.store.batch().unwrap();
        batch.replace_mailboxes(&mailboxes_get()).unwrap();
        batch.commit().unwrap();
        self
    }

    pub fn apply(&mut self, emails: &[Email]) {
        let mut batch = self.store.batch().unwrap();
        batch.upsert_emails(emails).unwrap();
        batch.commit().unwrap();
    }

    /// Applies an `Email/changes` batch with the given updated emails.
    pub fn changes(&mut self, updated: &[Email], destroyed: &[&str]) {
        let mut changes = ChangesResponse::<Email>::new("A1", "s0", "s1");
        changes.updated = updated.iter().map(|e| e.id.clone()).collect();
        changes.destroyed = destroyed.iter().map(|d| Id::from(*d)).collect();
        let mut batch = self.store.batch().unwrap();
        batch.apply_email_changes(&changes, updated).unwrap();
        batch.commit().unwrap();
    }

    /// Recounts every derived counter from scratch and compares it with
    /// what the writer maintained.
    pub fn check_consistency(&self) {
        let conn = self.cache();
        let drift: i64 = conn
            .query_row(
                "SELECT count(*) FROM (
                     SELECT l.mailbox_id FROM mailbox_local l
                     LEFT JOIN (SELECT mailbox_id, count(*) AS n FROM email_mailbox GROUP BY mailbox_id) m
                         ON m.mailbox_id = l.mailbox_id
                     LEFT JOIN (SELECT mailbox_id, count(*) AS n FROM conversation GROUP BY mailbox_id) c
                         ON c.mailbox_id = l.mailbox_id
                     WHERE l.messages != COALESCE(m.n, 0) OR l.conversations != COALESCE(c.n, 0))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(drift, 0, "mailbox_local counters drifted");
        let missing: i64 = conn
            .query_row(
                "SELECT count(*) FROM email_mailbox em
                 WHERE NOT EXISTS (SELECT 1 FROM mailbox_local l WHERE l.mailbox_id = em.mailbox_id)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(missing, 0, "membership without a counter row");
        let orphans: i64 = conn
            .query_row(
                "SELECT count(*) FROM conversation c WHERE NOT EXISTS (
                     SELECT 1 FROM email_mailbox em JOIN email e ON e.rid = em.email_rid
                     WHERE em.mailbox_id = c.mailbox_id AND e.thread_id = c.thread_id)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "conversation rows without messages");
        let fts: String = conn
            .query_row("SELECT count(*) FROM search_doc", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n.to_string())
            .unwrap();
        let emails: i64 = conn
            .query_row("SELECT count(*) FROM email", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            fts,
            emails.to_string(),
            "search_doc rows do not match emails"
        );
        conn.execute(
            "INSERT INTO email_fts (email_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .expect("FTS index matches its content table");
    }
}

pub fn ts(s: &str) -> Timestamp {
    s.parse().expect("timestamp")
}

pub fn now() -> Timestamp {
    ts("2026-10-23T14:27:00Z")
}

pub const INBOX: &str = "mb-inbox";
pub const ARCHIVE: &str = "mb-archive";
pub const SENT: &str = "mb-sent";
pub const TRASH: &str = "mb-trash";
pub const SPAM: &str = "mb-spam";
pub const DRAFTS: &str = "mb-drafts";
pub const GARDEN: &str = "mb-garden";
pub const PROJECTS: &str = "mb-projects";

pub fn mailbox(id: &str, name: &str, role: Option<&str>, parent: Option<&str>) -> Mailbox {
    serde_json::from_value(json!({
        "id": id,
        "name": name,
        "role": role,
        "parentId": parent,
        "sortOrder": 10,
        "totalEmails": 0,
        "unreadEmails": 0,
        "totalThreads": 0,
        "unreadThreads": 0,
        "isSubscribed": true,
        "myRights": {"mayReadItems": true, "mayAddItems": true, "mayRemoveItems": true,
                     "maySetSeen": true, "maySetKeywords": true, "mayCreateChild": true,
                     "mayRename": true, "mayDelete": true, "maySubmit": true}
    }))
    .unwrap()
}

pub fn mailboxes_get() -> GetResponse<Mailbox> {
    let list = vec![
        mailbox(INBOX, "Inbox", Some("inbox"), None),
        mailbox(ARCHIVE, "Archive", Some("archive"), None),
        mailbox(SENT, "Sent", Some("sent"), None),
        mailbox(DRAFTS, "Drafts", Some("drafts"), None),
        mailbox(TRASH, "Trash", Some("trash"), None),
        mailbox(SPAM, "Spam", Some("junk"), None),
        mailbox(PROJECTS, "Projects", None, None),
        mailbox(GARDEN, "Garden club", None, Some(PROJECTS)),
    ];
    serde_json::from_value(json!({
        "accountId": "A1",
        "state": "mb-1",
        "list": list,
        "notFound": []
    }))
    .unwrap()
}

/// A builder for list-property emails.
#[derive(Clone)]
pub struct E {
    pub value: Value,
}

pub fn email(id: &str) -> E {
    E {
        value: json!({
            "id": id,
            "threadId": format!("t-{id}"),
            "mailboxIds": {INBOX: true},
            "keywords": {},
            "receivedAt": "2026-10-20T09:00:00Z",
            "size": 1200,
            "from": [{"name": "Juniper Tran", "email": "juniper@example.org"}],
            "to": [{"name": "Mira Holt", "email": "mira@example.org"}],
            "cc": [],
            "subject": format!("Subject {id}"),
            "preview": format!("Preview of {id}"),
            "hasAttachment": false,
            "header:X-FM-Masked-Email:asText": null,
        }),
    }
}

impl E {
    pub fn thread(mut self, t: &str) -> Self {
        self.value["threadId"] = json!(t);
        self
    }
    pub fn at(mut self, at: &str) -> Self {
        self.value["receivedAt"] = json!(at);
        self
    }
    pub fn mailboxes(mut self, ids: &[&str]) -> Self {
        let map: serde_json::Map<String, Value> =
            ids.iter().map(|m| ((*m).to_owned(), json!(true))).collect();
        self.value["mailboxIds"] = Value::Object(map);
        self
    }
    pub fn keywords(mut self, kws: &[&str]) -> Self {
        let map: serde_json::Map<String, Value> =
            kws.iter().map(|k| ((*k).to_owned(), json!(true))).collect();
        self.value["keywords"] = Value::Object(map);
        self
    }
    pub fn seen(self) -> Self {
        self.keywords(&["$seen"])
    }
    pub fn from(mut self, name: &str, address: &str) -> Self {
        self.value["from"] = json!([{"name": name, "email": address}]);
        self
    }
    pub fn to(mut self, name: &str, address: &str) -> Self {
        self.value["to"] = json!([{"name": name, "email": address}]);
        self
    }
    pub fn subject(mut self, s: &str) -> Self {
        self.value["subject"] = json!(s);
        self
    }
    pub fn preview(mut self, s: &str) -> Self {
        self.value["preview"] = json!(s);
        self
    }
    pub fn attachment(mut self) -> Self {
        self.value["hasAttachment"] = json!(true);
        self
    }
    pub fn size(mut self, n: u64) -> Self {
        self.value["size"] = json!(n);
        self
    }
    pub fn masked(mut self, address: &str) -> Self {
        self.value["header:X-FM-Masked-Email:asText"] = json!(address);
        self
    }
    pub fn build(&self) -> Email {
        serde_json::from_value(self.value.clone()).expect("email")
    }
}

/// A flags-only email, as `Email/get` of `#updated` returns it.
pub fn flags(id: &str, mailboxes: &[&str], keywords: &[&str]) -> Email {
    let mb: serde_json::Map<String, Value> = mailboxes
        .iter()
        .map(|m| ((*m).to_owned(), json!(true)))
        .collect();
    let kw: serde_json::Map<String, Value> = keywords
        .iter()
        .map(|k| ((*k).to_owned(), json!(true)))
        .collect();
    serde_json::from_value(json!({"id": id, "mailboxIds": mb, "keywords": kw})).unwrap()
}

pub fn state(s: &str) -> State {
    State::from(s)
}
