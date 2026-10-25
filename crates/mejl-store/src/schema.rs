//! Schemas, migrations and connection setup for `cache.db` and `state.db`.
//!
//! Both files use `PRAGMA user_version` as the schema version.
//!
//! - `cache.db` holds nothing that cannot be fetched again, so any version
//!   mismatch, or a file that is not a database at all, drops and recreates
//!   it.
//! - `state.db` holds user intent and migrates in place through the numbered
//!   steps in [`STATE_MIGRATIONS`]. A version newer than this build is an
//!   error, never a reason to drop data.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags};

use crate::error::{Error, Result};

/// The cache database's file name inside an account directory. It can be
/// deleted at any time; mejl fetches its contents again.
pub const CACHE_FILE: &str = "cache.db";
/// The state database's file name inside an account directory. It holds
/// user intent (outbox, drafts, rules) and must not be deleted lightly.
pub const STATE_FILE: &str = "state.db";
/// Both store files, cache first. SQLite keeps `-wal` and `-shm` files next
/// to each one.
pub const STORE_FILES: [&str; 2] = [CACHE_FILE, STATE_FILE];

/// Bump on any change to [`CACHE_SCHEMA`]. Old caches are then rebuilt.
pub(crate) const CACHE_VERSION: i64 = 1;

/// The whole cache schema. See the crate docs for how the tables relate.
pub(crate) const CACHE_SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE sync_state (type TEXT PRIMARY KEY, state TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE mailbox (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL DEFAULT '',
    role TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    total_emails INTEGER NOT NULL DEFAULT 0,
    unread_emails INTEGER NOT NULL DEFAULT 0,
    total_threads INTEGER NOT NULL DEFAULT 0,
    unread_threads INTEGER NOT NULL DEFAULT 0,
    is_subscribed INTEGER NOT NULL DEFAULT 0,
    json TEXT NOT NULL
) WITHOUT ROWID;

-- Server truth lives in srv_keywords_json and srv_mailbox_ids_json. The hot
-- flag columns, keywords_json and email_mailbox hold the effective state:
-- server truth with pending outbox patches applied.
CREATE TABLE email (
    rid INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    thread_id TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    subject TEXT NOT NULL DEFAULT '',
    preview TEXT NOT NULL DEFAULT '',
    from_json TEXT,
    to_json TEXT,
    cc_json TEXT,
    bcc_json TEXT,
    reply_to_json TEXT,
    sender_json TEXT,
    sent_at TEXT,
    message_id_json TEXT,
    in_reply_to_json TEXT,
    references_json TEXT,
    has_attachment INTEGER NOT NULL DEFAULT 0,
    srv_keywords_json TEXT NOT NULL DEFAULT '{}',
    srv_mailbox_ids_json TEXT NOT NULL DEFAULT '{}',
    keywords_json TEXT NOT NULL DEFAULT '{}',
    is_seen INTEGER NOT NULL DEFAULT 0,
    is_flagged INTEGER NOT NULL DEFAULT 0,
    is_answered INTEGER NOT NULL DEFAULT 0,
    is_draft INTEGER NOT NULL DEFAULT 0,
    masked_header TEXT,
    masked_email_id TEXT,
    has_overlay INTEGER NOT NULL DEFAULT 0,
    is_hidden INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX email_by_thread ON email (thread_id, received_at);
CREATE INDEX email_by_masked_id ON email (masked_email_id) WHERE masked_email_id IS NOT NULL;
CREATE INDEX email_with_masked_header ON email (masked_header) WHERE masked_header IS NOT NULL;
CREATE INDEX email_with_overlay ON email (has_overlay) WHERE has_overlay = 1;

CREATE TABLE email_mailbox (
    mailbox_id TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    email_rid INTEGER NOT NULL REFERENCES email (rid) ON DELETE CASCADE,
    PRIMARY KEY (mailbox_id, received_at, email_rid)
) WITHOUT ROWID;
CREATE INDEX email_mailbox_by_email ON email_mailbox (email_rid);

-- One row per thread and mailbox, kept up to date by the writer, so a page
-- of conversations is one range scan.
CREATE TABLE conversation (
    mailbox_id TEXT NOT NULL,
    latest_at INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    latest_rid INTEGER NOT NULL,
    count_in_mailbox INTEGER NOT NULL,
    thread_count INTEGER NOT NULL,
    unread_count INTEGER NOT NULL,
    any_flagged INTEGER NOT NULL,
    any_attachment INTEGER NOT NULL,
    any_masked INTEGER NOT NULL,
    participants_json TEXT NOT NULL,
    PRIMARY KEY (mailbox_id, latest_at, thread_id)
) WITHOUT ROWID;
CREATE INDEX conversation_by_thread ON conversation (thread_id);
-- The primary key holds whole rows, so skipping a deep offset over it reads
-- every skipped row. This narrow copy of the key makes the skip cheap.
CREATE INDEX conversation_order ON conversation (mailbox_id, latest_at, thread_id);

CREATE TABLE mailbox_local (
    mailbox_id TEXT PRIMARY KEY,
    messages INTEGER NOT NULL DEFAULT 0,
    conversations INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- How pending local changes move the server's mailbox counts, per thread.
CREATE TABLE count_delta (
    thread_id TEXT NOT NULL,
    mailbox_id TEXT NOT NULL,
    d_total_emails INTEGER NOT NULL,
    d_unread_emails INTEGER NOT NULL,
    d_total_threads INTEGER NOT NULL,
    d_unread_threads INTEGER NOT NULL,
    PRIMARY KEY (thread_id, mailbox_id)
) WITHOUT ROWID;

CREATE TABLE thread (id TEXT PRIMARY KEY, email_ids_json TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE body_meta (
    email_rid INTEGER PRIMARY KEY REFERENCES email (rid) ON DELETE CASCADE,
    headers_json TEXT,
    fetched_at INTEGER NOT NULL,
    opened_at INTEGER,
    bytes INTEGER NOT NULL
);
CREATE INDEX body_lru ON body_meta (coalesce(opened_at, fetched_at), email_rid);
CREATE TABLE body_part (
    email_rid INTEGER NOT NULL REFERENCES body_meta (email_rid) ON DELETE CASCADE,
    kind INTEGER NOT NULL,
    idx INTEGER NOT NULL,
    part_id TEXT,
    blob_id TEXT,
    mime_type TEXT,
    charset TEXT,
    value TEXT NOT NULL,
    is_truncated INTEGER NOT NULL,
    is_encoding_problem INTEGER NOT NULL,
    PRIMARY KEY (email_rid, kind, idx)
) WITHOUT ROWID;
CREATE TABLE attachment (
    email_rid INTEGER NOT NULL REFERENCES body_meta (email_rid) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    part_id TEXT,
    blob_id TEXT,
    name TEXT,
    mime_type TEXT,
    size INTEGER NOT NULL,
    disposition TEXT,
    cid TEXT,
    PRIMARY KEY (email_rid, idx)
) WITHOUT ROWID;

-- The rowid is the email's rid. It is an explicit INTEGER PRIMARY KEY so
-- VACUUM cannot renumber it under the external-content index.
CREATE TABLE search_doc (
    rowid INTEGER PRIMARY KEY,
    subject TEXT,
    from_text TEXT,
    to_text TEXT,
    preview TEXT,
    body_text TEXT
);
CREATE VIRTUAL TABLE email_fts USING fts5 (
    subject, from_text, to_text, preview, body_text,
    content = 'search_doc', content_rowid = 'rowid',
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);
CREATE TRIGGER search_doc_ai AFTER INSERT ON search_doc BEGIN
    INSERT INTO email_fts (rowid, subject, from_text, to_text, preview, body_text)
    VALUES (new.rowid, new.subject, new.from_text, new.to_text, new.preview, new.body_text);
END;
CREATE TRIGGER search_doc_ad AFTER DELETE ON search_doc BEGIN
    INSERT INTO email_fts (email_fts, rowid, subject, from_text, to_text, preview, body_text)
    VALUES ('delete', old.rowid, old.subject, old.from_text, old.to_text, old.preview, old.body_text);
END;
CREATE TRIGGER search_doc_au AFTER UPDATE ON search_doc BEGIN
    INSERT INTO email_fts (email_fts, rowid, subject, from_text, to_text, preview, body_text)
    VALUES ('delete', old.rowid, old.subject, old.from_text, old.to_text, old.preview, old.body_text);
    INSERT INTO email_fts (rowid, subject, from_text, to_text, preview, body_text)
    VALUES (new.rowid, new.subject, new.from_text, new.to_text, new.preview, new.body_text);
END;

CREATE TABLE query_cache (
    query_key TEXT PRIMARY KEY,
    query_state TEXT NOT NULL,
    collapse_threads INTEGER NOT NULL,
    can_calculate_changes INTEGER NOT NULL,
    total INTEGER,
    ids_blob BLOB NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE identity (id TEXT PRIMARY KEY, pos INTEGER NOT NULL, json TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE contact (
    id TEXT PRIMARY KEY,
    uid TEXT,
    kind TEXT,
    name TEXT,
    name_words TEXT NOT NULL DEFAULT '',
    json TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE contact_address (
    contact_id TEXT NOT NULL REFERENCES contact (id) ON DELETE CASCADE,
    address_lc TEXT NOT NULL,
    address TEXT NOT NULL,
    pref INTEGER NOT NULL,
    PRIMARY KEY (contact_id, address_lc)
) WITHOUT ROWID;
CREATE INDEX contact_address_by_address ON contact_address (address_lc);

-- Addresses mail was sent to, from cached messages in the Sent mailbox.
CREATE TABLE correspondent (
    address_lc TEXT PRIMARY KEY,
    address TEXT NOT NULL,
    name TEXT,
    name_words TEXT NOT NULL DEFAULT '',
    last_used_at INTEGER NOT NULL,
    use_count INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE masked_email (
    id TEXT PRIMARY KEY,
    email_lc TEXT,
    state TEXT,
    last_message_at INTEGER,
    json TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX masked_by_address ON masked_email (email_lc);

-- A mirror of the outbox entries whose overlay is active, so every cache
-- transaction can re-apply them without reading state.db. Rebuilt from
-- state.db on every open.
CREATE TABLE overlay (
    target_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT,
    done INTEGER NOT NULL DEFAULT 0,
    result_id TEXT,
    PRIMARY KEY (target_id, seq)
) WITHOUT ROWID;
CREATE INDEX overlay_by_seq ON overlay (seq);
"#;

const STATE_V1: &str = r#"
CREATE TABLE outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    hold_until INTEGER,
    not_before INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    interrupted INTEGER NOT NULL DEFAULT 0,
    overlay_active INTEGER NOT NULL DEFAULT 0,
    payload_json TEXT,
    label TEXT,
    temp_id TEXT,
    result_id TEXT,
    undo_of INTEGER,
    finished_at INTEGER
);
CREATE INDEX outbox_by_status ON outbox (status, seq);

CREATE TABLE outbox_target (
    seq INTEGER NOT NULL REFERENCES outbox (seq) ON DELETE CASCADE,
    target_id TEXT NOT NULL,
    patch_json TEXT,
    inverse_json TEXT,
    state TEXT NOT NULL DEFAULT 'active',
    error TEXT,
    PRIMARY KEY (seq, target_id)
) WITHOUT ROWID;
CREATE INDEX outbox_target_by_target ON outbox_target (target_id);

CREATE TABLE undo (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    outbox_seq INTEGER NOT NULL REFERENCES outbox (seq) ON DELETE CASCADE,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE draft (
    local_id TEXT PRIMARY KEY,
    server_email_id TEXT,
    identity_id TEXT,
    headers_json TEXT NOT NULL,
    body TEXT NOT NULL,
    format TEXT NOT NULL,
    attachments_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE rule (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    sort_order INTEGER NOT NULL,
    rule_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE rule_render (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rendered_at INTEGER NOT NULL,
    sieve TEXT NOT NULL,
    body_sha256 TEXT NOT NULL,
    user_confirmed_paste_at INTEGER
);

CREATE TABLE ui_state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL) WITHOUT ROWID;
"#;

/// Version 2: the redo stack, the mirror of `undo`.
const STATE_V2: &str = r#"
CREATE TABLE redo (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    outbox_seq INTEGER NOT NULL REFERENCES outbox (seq) ON DELETE CASCADE,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
"#;

/// Version 3: server copies of drafts. A draft row keeps the newest message
/// to upload with a revision counter, the revision the server has, whether
/// an upload was sent without its answer being recorded, and the
/// Message-ID every copy of the draft shares. `draft_trash` lists server
/// copies to destroy: by email id, or by Message-ID for copies whose id was
/// never learned.
const STATE_V3: &str = r#"
ALTER TABLE draft ADD COLUMN upload_json TEXT;
ALTER TABLE draft ADD COLUMN upload_rev INTEGER NOT NULL DEFAULT 0;
ALTER TABLE draft ADD COLUMN uploaded_rev INTEGER NOT NULL DEFAULT 0;
ALTER TABLE draft ADD COLUMN upload_in_flight INTEGER NOT NULL DEFAULT 0;
ALTER TABLE draft ADD COLUMN server_message_id TEXT;
CREATE TABLE draft_trash (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email_id TEXT,
    message_id TEXT,
    created_at INTEGER NOT NULL,
    CHECK (email_id IS NOT NULL OR message_id IS NOT NULL)
);
"#;

/// Numbered `state.db` migrations. Entry `n` takes the file from version
/// `n` to `n + 1`. Append only: never edit a step that has shipped.
pub(crate) const STATE_MIGRATIONS: &[&str] = &[STATE_V1, STATE_V2, STATE_V3];

pub(crate) fn state_version() -> i64 {
    i64::try_from(STATE_MIGRATIONS.len()).unwrap_or(i64::MAX)
}

#[derive(Clone, Copy)]
pub(crate) enum Durability {
    /// Fast commits that may lose the last transactions on power loss.
    Normal,
    /// Commits survive power loss.
    Full,
}

pub(crate) fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

fn configure_writer(conn: &Connection, durability: Durability, busy: Duration) -> Result<()> {
    conn.busy_timeout(busy)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(%mode, "SQLite did not switch to WAL mode");
    }
    conn.execute_batch(match durability {
        Durability::Normal => "PRAGMA synchronous = NORMAL;",
        Durability::Full => "PRAGMA synchronous = FULL;",
    })?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -32000;",
    )?;
    conn.set_prepared_statement_cache_capacity(256);
    Ok(())
}

/// Applies every migration step after the file's current version, each in
/// its own transaction.
pub(crate) fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let current = user_version(conn)?;
    let supported = i64::try_from(migrations.len()).unwrap_or(i64::MAX);
    if current > supported {
        return Err(Error::StateTooNew {
            found: current,
            supported,
        });
    }
    let start = usize::try_from(current).unwrap_or(0);
    for (index, step) in migrations.iter().enumerate().skip(start) {
        let tx = conn.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(
            None,
            "user_version",
            i64::try_from(index + 1).unwrap_or(i64::MAX),
        )?;
        tx.commit()?;
        tracing::info!(version = index + 1, "migrated state.db");
    }
    Ok(())
}

pub(crate) fn open_state(path: &Path, busy: Duration) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    configure_writer(&conn, Durability::Full, busy)?;
    migrate(&mut conn, STATE_MIGRATIONS)?;
    Ok(conn)
}

enum CacheOpen {
    Ready(Connection),
    Rebuild(String),
}

/// Opens `cache.db`, creating it, or deleting and recreating it when its
/// schema does not match or it is not a database.
pub(crate) fn open_cache(path: &Path, busy: Duration) -> Result<Connection> {
    match try_open_cache(path, busy)? {
        CacheOpen::Ready(conn) => Ok(conn),
        CacheOpen::Rebuild(reason) => {
            tracing::warn!(%reason, path = %path.display(), "rebuilding cache.db");
            remove_db_files(path)?;
            match try_open_cache(path, busy)? {
                CacheOpen::Ready(conn) => Ok(conn),
                CacheOpen::Rebuild(reason) => Err(Error::Corrupt(format!(
                    "cache.db could not be rebuilt: {reason}"
                ))),
            }
        }
    }
}

fn is_damaged(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    )
}

fn try_open_cache(path: &Path, busy: Duration) -> Result<CacheOpen> {
    let conn = Connection::open(path)?;
    if let Err(err) = configure_writer(&conn, Durability::Normal, busy) {
        if let Error::Sqlite(inner) = &err
            && is_damaged(inner)
        {
            return Ok(CacheOpen::Rebuild(inner.to_string()));
        }
        return Err(err);
    }
    let version = match user_version(&conn) {
        Ok(v) => v,
        Err(Error::Sqlite(inner)) if is_damaged(&inner) => {
            return Ok(CacheOpen::Rebuild(inner.to_string()));
        }
        Err(err) => return Err(err),
    };
    if version == CACHE_VERSION {
        return Ok(CacheOpen::Ready(conn));
    }
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
        [],
        |r| r.get(0),
    )?;
    if version != 0 || tables != 0 {
        return Ok(CacheOpen::Rebuild(format!(
            "schema version {version}, expected {CACHE_VERSION}"
        )));
    }
    conn.execute_batch(&format!(
        "BEGIN; {CACHE_SCHEMA} PRAGMA user_version = {CACHE_VERSION}; COMMIT;"
    ))?;
    Ok(CacheOpen::Ready(conn))
}

pub(crate) fn remove_db_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let file = PathBuf::from(file);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::Io { path: file, source }),
        }
    }
    Ok(())
}

/// A connection for a reader. It is opened read-write so SQLite can use the
/// WAL index normally, then locked with `query_only`, so it can never
/// change data.
pub(crate) fn open_reader(
    path: &Path,
    expected_version: i64,
    busy: Duration,
) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|err| {
        if err.sqlite_error_code() == Some(ErrorCode::CannotOpen) {
            Error::NotInitialised(path.to_owned())
        } else {
            Error::Sqlite(err)
        }
    })?;
    conn.busy_timeout(busy)?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA cache_size = -16000;")?;
    conn.set_prepared_statement_cache_capacity(64);
    if user_version(&conn)? != expected_version {
        return Err(Error::NotInitialised(path.to_owned()));
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_schema_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(user_version(&conn).unwrap(), CACHE_VERSION);
        conn.execute("INSERT INTO meta (key, value) VALUES ('k', 'v')", [])
            .unwrap();
        drop(conn);
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        let v: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "v");
    }

    #[test]
    fn cache_with_other_version_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        conn.execute("INSERT INTO meta (key, value) VALUES ('k', 'v')", [])
            .unwrap();
        conn.pragma_update(None, "user_version", CACHE_VERSION + 7)
            .unwrap();
        drop(conn);
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(user_version(&conn).unwrap(), CACHE_VERSION);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn garbage_cache_file_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        std::fs::write(&path, b"this is not a database, just some bytes that are long enough to fill a header page........").unwrap();
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(user_version(&conn).unwrap(), CACHE_VERSION);
    }

    #[test]
    fn foreign_database_in_place_of_cache_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE other (x);")
            .unwrap();
        let conn = open_cache(&path, Duration::from_secs(1)).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'other'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn state_migrates_in_place_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let v1 = "CREATE TABLE note (id INTEGER PRIMARY KEY, text TEXT);";
        let v2 = "ALTER TABLE note ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;";
        let mut conn = Connection::open(&path).unwrap();
        migrate(&mut conn, &[v1]).unwrap();
        conn.execute("INSERT INTO note (text) VALUES ('keep me')", [])
            .unwrap();
        drop(conn);

        let mut conn = Connection::open(&path).unwrap();
        migrate(&mut conn, &[v1, v2]).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 2);
        let (text, pinned): (String, i64) = conn
            .query_row("SELECT text, pinned FROM note", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((text.as_str(), pinned), ("keep me", 0));

        // Running again is a no-op.
        migrate(&mut conn, &[v1, v2]).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 2);
    }

    #[test]
    fn v3_adds_server_draft_columns_and_keeps_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut conn = Connection::open(&path).unwrap();
        migrate(&mut conn, &STATE_MIGRATIONS[..2]).unwrap();
        conn.execute(
            "INSERT INTO draft (local_id, server_email_id, identity_id, headers_json, body, format,
                 attachments_json, created_at, updated_at)
             VALUES ('d1', 'M1', NULL, '{}', 'Hi', 'plain', '[]', 1, 2)",
            [],
        )
        .unwrap();
        drop(conn);

        let mut conn = Connection::open(&path).unwrap();
        migrate(&mut conn, STATE_MIGRATIONS).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 3);
        let row: (
            String,
            String,
            Option<String>,
            i64,
            i64,
            i64,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT body, server_email_id, upload_json, upload_rev, uploaded_rev,
                     upload_in_flight, server_message_id FROM draft WHERE local_id = 'd1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            ("Hi".into(), "M1".into(), None, 0, 0, 0, None),
            "the draft survives with nothing to upload"
        );
        // A trash entry needs something to find the copy by.
        assert!(
            conn.execute(
                "INSERT INTO draft_trash (email_id, message_id, created_at) VALUES (NULL, NULL, 1)",
                [],
            )
            .is_err()
        );
        conn.execute(
            "INSERT INTO draft_trash (email_id, created_at) VALUES ('M1', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn newer_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let err = migrate(&mut conn, STATE_MIGRATIONS).unwrap_err();
        assert!(matches!(err, Error::StateTooNew { found: 99, .. }));
    }

    #[test]
    fn failed_migration_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut conn = Connection::open(&path).unwrap();
        let ok = "CREATE TABLE a (x);";
        let bad = "CREATE TABLE b (x); THIS IS NOT SQL;";
        assert!(migrate(&mut conn, &[ok, bad]).is_err());
        assert_eq!(user_version(&conn).unwrap(), 1);
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn reader_needs_an_initialised_store() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_reader(
            &dir.path().join(CACHE_FILE),
            CACHE_VERSION,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotInitialised(_)));
    }
}
