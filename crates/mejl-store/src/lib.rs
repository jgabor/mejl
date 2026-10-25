//! The local SQLite store for one mejl account.
//!
//! An account directory holds two files:
//!
//! - `cache.db`: everything fetched from the server (session, state
//!   strings, mailboxes, emails, threads, identities, contacts, masked
//!   addresses, bodies, the search index, query windows). It can be deleted
//!   at any time; a schema change simply rebuilds it.
//! - `state.db`: user intent (the outbox, the undo stack, local drafts,
//!   rules and their render history, UI state). It migrates in place and is
//!   never dropped.
//!
//! Both run in WAL mode, so readers never wait for the writer. `cache.db`
//! commits with `synchronous = NORMAL`, `state.db` with `FULL`.
//!
//! # Threads
//!
//! - The store thread owns the [`Store`]. Server data goes in through
//!   [`SyncBatch`]es, one transaction per sync batch, and user actions
//!   through [`Store::enqueue`].
//! - The UI thread reads through a [`Reader`], opened from a cloneable
//!   [`ReaderFactory`]. Reads are small indexed queries: a page of 60
//!   conversations in a 100k-message mailbox is one range scan over a
//!   table the writer keeps up to date.
//!
//! # Optimistic changes
//!
//! See [`outbox`] for how pending changes are overlaid on server truth and
//! how they are confirmed or rolled back. The [`overlay`] module has the
//! patch rules, which the UI can also use on its in-memory rows for
//! same-frame feedback.
//!
//! The API is synchronous. Nothing here touches the network.

#![warn(missing_docs)]

mod batch;
mod codec;
mod derive;
mod error;
mod model;
pub mod outbox;
pub mod overlay;
mod read;
mod schema;
pub mod search;
mod server_draft;
mod store;

pub use batch::{
    MASKED_EMAIL_HEADER, MASKED_EMAIL_HEADER_ALT, MaskedDiff, QueryChangesOutcome, SyncBatch,
    UpsertReport, type_name,
};
pub use error::{Error, Result};
pub use model::{
    AddressSuggestion, Attachment, Body, BodyPart, ConversationRow, Draft, DraftHeaders,
    EmailDetail, Highlighted, MailboxRow, MaskedEmailRow, MessageHeaders, MessageRow,
    OutboxSummary, Page, QueryPage, QueuedSend, RuleRecord, RuleRender,
};
pub use outbox::{
    CancelOutcome, EmailPatchTarget, EnqueueOptions, Enqueued, FailedEntry, FinishReport, FlushOp,
    FlushOutcome, FlushUnit, Mutation, RedoOutcome, Seq, UNDO_DEPTH, UndoOutcome,
};
pub use overlay::EmailFlags;
pub use read::{Reader, ReaderFactory};
pub use schema::{CACHE_FILE, STATE_FILE, STORE_FILES};
pub use search::{IssueKind, QueryIssue, SearchHit, SearchOptions, SearchResults};
pub use server_draft::{DraftTrash, DraftUpload, DraftUploadDone};
pub use store::{Store, StoreOptions};

#[cfg(test)]
mod tests {
    // The store thread owns the Store and the UI thread a Reader, so both
    // must move between threads; the factory is shared.
    #[test]
    fn thread_bounds() {
        fn send<T: Send>() {}
        fn send_sync<T: Send + Sync>() {}
        send::<crate::Store>();
        send::<crate::Reader>();
        send_sync::<crate::ReaderFactory>();
        send_sync::<crate::Error>();
    }
}
