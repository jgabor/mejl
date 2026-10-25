//! The store's error type.

use std::path::PathBuf;

/// Everything that can go wrong in the store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A value could not be encoded to or decoded from JSON.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The account directory could not be created or a file in it removed.
    #[error("could not prepare {path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What the OS reported.
        #[source]
        source: std::io::Error,
    },

    /// `state.db` was written by a newer mejl. It holds user intent, so the
    /// store refuses to touch it rather than drop or downgrade it.
    #[error("state.db has schema version {found}, but this build only knows up to {supported}")]
    StateTooNew {
        /// The file's version.
        found: i64,
        /// The newest version this build can migrate to.
        supported: i64,
    },

    /// A reader was opened on a directory the store has not set up yet, or
    /// on files from a different schema version.
    #[error("no usable store at {0}; open it with Store::open first")]
    NotInitialised(PathBuf),

    /// Stored data did not have the expected shape.
    #[error("corrupt stored data: {0}")]
    Corrupt(String),

    /// The caller passed something the store cannot act on.
    #[error("invalid input: {0}")]
    Invalid(String),

    /// No outbox entry has this sequence number, or it is not in the state
    /// the call needs.
    #[error("outbox entry {0} not found or not in the expected state")]
    UnknownEntry(i64),

    /// The mutation has no inverse and cannot be put on the undo stack.
    #[error("this kind of change cannot be undone")]
    NotUndoable,
}

/// The store's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
