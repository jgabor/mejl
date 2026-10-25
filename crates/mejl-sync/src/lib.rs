//! The mejl sync engine.
//!
//! The engine keeps the local store current with the server and sends the
//! user's changes. The UI never waits for it: it reads the store through
//! its own [`Reader`](mejl_store::Reader) and talks to the engine through a
//! non-blocking [`SyncHandle`].
//!
//! # Threads
//!
//! - `mejl-store` owns the [`Store`](mejl_store::Store) and does every
//!   write: applying server data, queuing mutations, recording flush
//!   results. Jobs run one at a time in the order they were sent.
//! - `mejl-sync` runs the engine's own tokio runtime: session, sync, push,
//!   the outbox flusher, bodies, search and Masked Email. Network results
//!   go to the store thread as jobs.
//! - The caller's UI thread sends [`Command`]s and receives [`SyncEvent`]s
//!   through the sink it passed to [`Engine::start`].
//!
//! # Same-frame feedback
//!
//! A mutation's overlay lands in the store one store-thread round trip
//! after the command. The caller keeps its own short-lived overlay for the
//! command's token and drops it when [`SyncEvent::Applied`] or
//! [`SyncEvent::Rejected`] arrives with that token. `Applied` carries the
//! scope to re-read, so there is no frame in between.
//!
//! # What the engine does
//!
//! - Session: fetched at start and again when a response reports a new
//!   `sessionState`. 401 and 403 stop all network work and send
//!   [`SyncEvent::AuthFailed`] until [`Command::SetToken`] or
//!   [`Command::ResumeAuth`].
//! - First sync: mailboxes, identities and contacts, then the Inbox window
//!   so it is readable first, then the other role mailboxes.
//! - Later syncs: `/changes` for every type and `Email/queryChanges` for
//!   cached windows, packed into few requests within the session limits.
//!   `cannotCalculateChanges` refetches that type. Masked Email is
//!   refetched whole, since it has no reliable change feed.
//! - Push: EventSource with an idle timeout, reconnect with backoff and
//!   jitter, a sync after every connect, and polling when push keeps
//!   failing.
//! - Outbox: units from the store, split at `maxObjectsInSet`, with
//!   transient failures retried and permanent ones rolled back and
//!   reported.
//! - Confirmation: data requested before a flush result was recorded is
//!   never applied, because it could confirm an overlay with old data.
//! - Sending: a client-side undo hold, a client-made Message-ID, and a
//!   lookup by that id before any resend, so a send is never duplicated.
//! - Drafts: server copies in the Drafts mailbox, newest version only, one
//!   upload at a time; a sent or deleted draft's copies are destroyed.
//! - Offline: mutations still queue; a backoff loop restores the
//!   connection and the outbox flushes.
//!
//! Tokens and message bodies are never logged.

#![warn(missing_docs)]

mod apply;
mod backoff;
mod bodies;
mod command;
mod config;
mod drafts;
mod engine;
mod error;
mod event;
mod flush;
mod local;
mod masked;
mod mutate;
mod push;
mod search;
mod send;
mod shared;
mod status;
mod store_thread;
mod sync;
mod upload;

pub use apply::mailbox_query_key;
pub use command::{
    BulkAction, Command, MacroPatch, MaskedState, OutgoingAttachment, OutgoingMessage,
    RoleMailboxes, SearchRequest,
};
pub use config::{Clock, EngineConfig, SystemClock};
pub use drafts::DRAFT_COPY_ERROR;
pub use engine::{Engine, SyncHandle};
pub use error::StartError;
pub use event::{
    Activity, DataChanges, PushState, SearchResults, Snippet, SyncEvent, SyncPhase, SyncStatus,
};
pub use local::{LocalOutcome, LocalWrite};
pub use search::filter_preview;
pub use send::decode_send_payload;

#[cfg(test)]
mod tests {
    #[test]
    fn handle_bounds() {
        fn send_sync<T: Send + Sync>() {}
        fn send<T: Send>() {}
        send_sync::<crate::SyncHandle>();
        send::<crate::Command>();
        send_sync::<crate::SyncEvent>();
    }
}
