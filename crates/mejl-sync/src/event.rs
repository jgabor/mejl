//! What the engine reports to the caller's sink.

use std::collections::BTreeSet;

use jiff::Timestamp;
use mejl_jmap::Id;
use mejl_store::{RedoOutcome, Seq, UndoOutcome};

use crate::command::OutgoingMessage;

/// An event from the engine. The sink receives them in order from several
/// threads, so it should only forward them, for example into a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    /// The status changed. Each event is a full snapshot.
    Status(SyncStatus),
    /// Cached data changed. Re-read only what the scopes name. These are
    /// merged and sent at most once per
    /// [`EngineConfig::data_coalesce`](crate::EngineConfig::data_coalesce).
    DataChanged(DataChanges),
    /// The credentials were rejected (HTTP 401 or 403). The engine stops
    /// talking to the server until
    /// [`Command::SetToken`](crate::Command::SetToken) arrives.
    AuthFailed {
        /// What the server said.
        error: String,
    },
    /// A mutation command was queued and its overlay is in the store. Drop
    /// the caller's own pending overlay for `token` and re-read `changed`.
    /// `changed` is not repeated in a later [`SyncEvent::DataChanged`].
    Applied {
        /// The caller's token from the command.
        token: u64,
        /// The outbox entry.
        seq: Seq,
        /// The local placeholder id of a created object, such as
        /// `local:12`.
        temp_id: Option<String>,
        /// What the overlay changed.
        changed: DataChanges,
    },
    /// A command was refused before anything was queued.
    Rejected {
        /// The caller's token from the command.
        token: u64,
        /// Why, for a toast.
        error: String,
    },
    /// The answer to [`Command::Undo`](crate::Command::Undo). `None` when
    /// there was nothing to undo.
    Undone {
        /// The caller's token.
        token: u64,
        /// What the store did. A cancelled send carries its payload; decode
        /// it with [`decode_send_payload`](crate::decode_send_payload).
        outcome: Option<UndoOutcome>,
        /// What changed.
        changed: DataChanges,
    },
    /// The answer to [`Command::Redo`](crate::Command::Redo). `None` when
    /// there was nothing to redo.
    Redone {
        /// The caller's token.
        token: u64,
        /// What the store did.
        outcome: Option<RedoOutcome>,
        /// What changed.
        changed: DataChanges,
    },
    /// Outbox entries reached the server.
    OutboxDone {
        /// The entries.
        seqs: Vec<Seq>,
    },
    /// An outbox entry failed in whole or in part. Its overlay is gone, so
    /// the rows already show server truth again.
    OutboxFailed {
        /// The entry.
        seq: Seq,
        /// The label it was queued with.
        label: Option<String>,
        /// The first error, for the toast.
        error: String,
        /// Objects the server rejected.
        failed_targets: Vec<Id>,
        /// Every target failed.
        entry_failed: bool,
    },
    /// A send is held in the outbox until `hold_until` (undo send).
    SendQueued {
        /// The caller's token.
        token: u64,
        /// The outbox entry, for cancel and send now.
        seq: Seq,
        /// The client-made Message-ID, without angle brackets.
        message_id: String,
        /// When the hold ends and the send goes out.
        hold_until: Timestamp,
    },
    /// The answer to [`Command::CancelSend`](crate::Command::CancelSend).
    SendCancelled {
        /// The caller's token.
        token: u64,
        /// The entry.
        seq: Seq,
        /// The message, to reopen as a draft. `None` when it was too late
        /// to cancel.
        message: Option<Box<OutgoingMessage>>,
    },
    /// A send reached the server and the message is in Sent.
    Sent {
        /// The outbox entry.
        seq: Seq,
        /// The Message-ID, without angle brackets.
        message_id: String,
        /// The server id of the sent email, when known.
        email_id: Option<Id>,
    },
    /// A body is in the store.
    BodyReady {
        /// The email.
        email_id: Id,
    },
    /// A body could not be fetched.
    BodyFailed {
        /// The email.
        email_id: Id,
        /// Why.
        error: String,
    },
    /// The answer to [`Command::LoadMore`](crate::Command::LoadMore): the
    /// mailbox's cached query window now holds `cached` conversations.
    WindowLoaded {
        /// The mailbox.
        mailbox: Id,
        /// Conversations in the cached window.
        cached: u64,
        /// The server's total, if it sent one.
        total: Option<u64>,
    },
    /// The answer to [`Command::UploadAttachment`](crate::Command::UploadAttachment).
    Uploaded {
        /// The caller's token.
        token: u64,
        /// The blob id to attach.
        blob_id: Id,
        /// The file name.
        name: String,
        /// The media type sent.
        content_type: String,
        /// Bytes uploaded.
        size: u64,
    },
    /// An upload failed.
    UploadFailed {
        /// The caller's token.
        token: u64,
        /// Why.
        error: String,
    },
    /// The answer to [`Command::Search`](crate::Command::Search).
    SearchResults {
        /// The caller's token.
        token: u64,
        /// The results.
        results: SearchResults,
    },
    /// A server search failed.
    SearchFailed {
        /// The caller's token.
        token: u64,
        /// Why.
        error: String,
    },
    /// The answer to [`Command::Local`](crate::Command::Local).
    LocalDone {
        /// The caller's token.
        token: u64,
        /// What the write did, or why it failed.
        result: Result<crate::local::LocalOutcome, String>,
    },
    /// A masked address was created on the server.
    MaskedCreated {
        /// The outbox entry that created it.
        seq: Seq,
        /// The new mask's id.
        id: Id,
        /// The generated address.
        email: Option<String>,
    },
}

/// What changed in the cache, by scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataChanges {
    /// The mailbox list or its counts.
    pub mailboxes: bool,
    /// Mailboxes whose conversation lists changed.
    pub mailbox_lists: BTreeSet<Id>,
    /// Emails whose rows or details changed.
    pub emails: BTreeSet<Id>,
    /// Threads whose conversations changed.
    pub threads: BTreeSet<Id>,
    /// The Masked Email list.
    pub masked_emails: bool,
    /// Sending identities.
    pub identities: bool,
    /// Contacts.
    pub contacts: bool,
    /// Outbox entries, such as queued sends.
    pub outbox: bool,
    /// Too much changed to say: re-read everything.
    pub everything: bool,
}

impl DataChanges {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        !self.mailboxes
            && self.mailbox_lists.is_empty()
            && self.emails.is_empty()
            && self.threads.is_empty()
            && !self.masked_emails
            && !self.identities
            && !self.contacts
            && !self.outbox
            && !self.everything
    }

    /// Adds everything `other` names.
    pub fn merge(&mut self, other: DataChanges) {
        self.mailboxes |= other.mailboxes;
        self.mailbox_lists.extend(other.mailbox_lists);
        self.emails.extend(other.emails);
        self.threads.extend(other.threads);
        self.masked_emails |= other.masked_emails;
        self.identities |= other.identities;
        self.contacts |= other.contacts;
        self.outbox |= other.outbox;
        self.everything |= other.everything;
    }
}

/// A snapshot of the engine's state for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    /// Whether the server answered the last request. Offline, mutations
    /// still queue and are sent when the connection returns.
    pub online: bool,
    /// The push channel.
    pub push: PushState,
    /// When the last full sync pass finished.
    pub last_sync: Option<Timestamp>,
    /// Outbox entries waiting to be sent, not counting held sends.
    pub pending: usize,
    /// Sends held for undo.
    pub held_sends: usize,
    /// Outbox entries that failed.
    pub failed: usize,
    /// Local drafts whose newest version is not on the server yet. See
    /// [`Command::SaveDraftToServer`](crate::Command::SaveDraftToServer).
    pub drafts_pending: usize,
    /// The account has no Drafts mailbox, so drafts stay on this device.
    pub drafts_local_only: bool,
    /// Work in progress.
    pub activity: Activity,
    /// The last error, cleared by the next success.
    pub last_error: Option<String>,
    /// The credentials were rejected and the engine is waiting for new ones.
    pub auth_failed: bool,
}

impl Default for SyncStatus {
    fn default() -> Self {
        SyncStatus {
            online: false,
            push: PushState::Connecting,
            last_sync: None,
            pending: 0,
            held_sends: 0,
            failed: 0,
            drafts_pending: 0,
            drafts_local_only: false,
            activity: Activity::default(),
            last_error: None,
            auth_failed: false,
        }
    }
}

/// The state of the push channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushState {
    /// Opening the first connection.
    Connecting,
    /// Connected; changes arrive as they happen.
    Connected,
    /// The connection dropped and the engine is trying again.
    Reconnecting {
        /// Failures in a row so far.
        attempt: u32,
    },
    /// Push keeps failing, so the engine also polls.
    Polling,
    /// The network is down.
    Offline,
    /// Push is turned off in the configuration; the engine polls.
    Disabled,
}

/// Work in progress, for spinners.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// The sync pass running now, if any.
    pub syncing: Option<SyncPhase>,
    /// Body fetches in flight.
    pub bodies: usize,
    /// Server searches in flight.
    pub searches: usize,
    /// An outbox unit is being sent.
    pub flushing: bool,
    /// Uploads in flight.
    pub uploads: usize,
}

/// What a sync pass is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    /// Fetching the session.
    Session,
    /// First sync: mailboxes, identities and contacts.
    Mailboxes,
    /// First sync: the Inbox window.
    Inbox,
    /// First sync: the other role mailboxes.
    Folders,
    /// Fetching changes since the last sync.
    Changes,
    /// The server could not list changes, so a type is fetched again.
    Resync,
    /// Extending a mailbox window.
    LoadingMore,
}

/// Server search results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// Matching email ids in server order. Their list rows are in the
    /// store.
    pub ids: Vec<Id>,
    /// The server's total, if it sent one.
    pub total: Option<u64>,
    /// The position of the first id.
    pub position: u64,
    /// Highlighted matches, by email.
    pub snippets: Vec<Snippet>,
    /// Query parts the server cannot answer, such as `fromin:` or a
    /// mailbox name that does not exist. They were left out.
    pub unsupported: Vec<String>,
}

/// One email's search highlights, with matches marked by `<mark>` tags and
/// the rest HTML-escaped, as the server sent them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snippet {
    /// The email.
    pub email_id: Id,
    /// The subject with matches marked.
    pub subject: Option<String>,
    /// A body excerpt with matches marked.
    pub preview: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_and_empty() {
        let mut a = DataChanges::default();
        assert!(a.is_empty());
        let mut b = DataChanges {
            mailboxes: true,
            ..Default::default()
        };
        b.emails.insert(Id::from("e1"));
        a.merge(b);
        assert!(!a.is_empty());
        assert!(a.mailboxes);
        assert!(a.emails.contains("e1"));
        a.merge(DataChanges {
            everything: true,
            ..Default::default()
        });
        assert!(a.everything && a.mailboxes);
    }
}
