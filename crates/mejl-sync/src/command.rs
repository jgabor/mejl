//! What the caller can ask the engine to do.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use mejl_jmap::types::{EmailAddress, keyword};
use mejl_jmap::{Id, Patch, Secret};
use mejl_store::{MailboxRow, Mutation, Seq};
use serde::{Deserialize, Serialize};

/// A request to the engine. Send it with
/// [`SyncHandle::send`](crate::SyncHandle::send), which never blocks.
///
/// Commands with a `token` answer with an event that carries it, so the
/// caller can match results to its own pending state.
#[derive(Debug)]
pub enum Command {
    /// Sync now, and refetch the Masked Email list.
    RefreshNow,
    /// Use a new access token, for example after the user pasted one when
    /// [`SyncEvent::AuthFailed`](crate::SyncEvent::AuthFailed) came. Resumes
    /// all network work.
    SetToken(Secret),
    /// Queue a store mutation the caller built. Answers with
    /// [`SyncEvent::Applied`](crate::SyncEvent::Applied) or
    /// [`SyncEvent::Rejected`](crate::SyncEvent::Rejected).
    Mutate {
        /// The caller's token.
        token: u64,
        /// The change.
        mutation: Mutation,
        /// Shown for undo and in failure toasts, such as "Archive 3".
        label: Option<String>,
        /// Put it on the undo stack.
        undoable: bool,
    },
    /// Apply an action to every cached conversation in a mailbox except
    /// some, as one undoable action. The ids are collected on the store
    /// thread, never on the caller's.
    Bulk {
        /// The caller's token.
        token: u64,
        /// The mailbox the selection was made in.
        mailbox: Id,
        /// Conversations deselected after selecting all.
        except_threads: Vec<Id>,
        /// What to do.
        action: BulkAction,
        /// The undo label. A default such as "Archive 12" is made when
        /// `None`.
        label: Option<String>,
    },
    /// Undo the newest undoable action.
    Undo {
        /// The caller's token.
        token: u64,
    },
    /// Apply the newest undone action again.
    Redo {
        /// The caller's token.
        token: u64,
    },
    /// Send a message after an undo window. Answers with
    /// [`SyncEvent::SendQueued`](crate::SyncEvent::SendQueued).
    Send {
        /// The caller's token.
        token: u64,
        /// The message.
        message: Box<OutgoingMessage>,
        /// How long the send is held so it can be cancelled. Zero sends at
        /// once.
        undo_window: Duration,
    },
    /// Cancel a held send. Answers with
    /// [`SyncEvent::SendCancelled`](crate::SyncEvent::SendCancelled).
    CancelSend {
        /// The caller's token.
        token: u64,
        /// The send's outbox entry.
        seq: Seq,
    },
    /// End a send's hold so it goes out now.
    SendNow {
        /// The send's outbox entry.
        seq: Seq,
    },
    /// Save a copy of a local draft in the Drafts mailbox on the server,
    /// replacing the previous copy. Send it after the local save
    /// ([`LocalWrite::SaveDraft`](crate::LocalWrite::SaveDraft)); a draft
    /// that is not saved locally is ignored.
    ///
    /// Safe to send often: only the newest version is uploaded, one
    /// request at a time, failures are retried, and offline the upload
    /// waits for the connection. Progress shows in
    /// [`SyncStatus::drafts_pending`](crate::SyncStatus::drafts_pending)
    /// and [`SyncStatus::drafts_local_only`](crate::SyncStatus::drafts_local_only).
    /// Deleting the local draft, or sending it, destroys the server copy.
    SaveDraftToServer {
        /// The local draft.
        local_id: String,
        /// The draft as it would be sent. An empty From address is left
        /// out, and no recipients are needed.
        message: Box<OutgoingMessage>,
    },
    /// Read a local file and upload it as a blob. The file is read on a
    /// blocking task. Answers with
    /// [`SyncEvent::Uploaded`](crate::SyncEvent::Uploaded).
    UploadAttachment {
        /// The caller's token.
        token: u64,
        /// The file.
        path: PathBuf,
        /// The media type. Guessed from the file name when `None`.
        content_type: Option<String>,
    },
    /// Fetch a body at high priority, for the message the user opened.
    FetchBody {
        /// The email.
        email_id: Id,
    },
    /// Fetch bodies the user is likely to open next, at low priority. A new
    /// prefetch replaces any queued earlier one.
    Prefetch {
        /// Emails, most likely first.
        email_ids: Vec<Id>,
    },
    /// The user opened a body, so it is evicted from the cache last.
    TouchBody {
        /// The email.
        email_id: Id,
    },
    /// Extend a mailbox's cached window, when the user scrolls past it.
    LoadMore {
        /// The mailbox.
        mailbox: Id,
    },
    /// Search on the server.
    Search {
        /// The caller's token.
        token: u64,
        /// What to search for.
        request: SearchRequest,
    },
    /// Create a masked address. Needs the server, so it is refused offline.
    MaskedCreate {
        /// The caller's token.
        token: u64,
        /// The site the address is for, such as `https://shop.example.com`.
        for_domain: String,
        /// The user's note.
        description: String,
        /// The start of the address: 1 to 64 of `a-z`, `0-9` and `_`.
        email_prefix: Option<String>,
    },
    /// Enable, disable or delete a masked address. Undoable.
    MaskedSetState {
        /// The caller's token.
        token: u64,
        /// The mask.
        id: Id,
        /// The new state.
        state: MaskedState,
    },
    /// Change a masked address's description. Undoable.
    MaskedSetDescription {
        /// The caller's token.
        token: u64,
        /// The mask.
        id: Id,
        /// The new note.
        description: String,
    },
    /// Write local-only state (drafts, rules, UI values) on the store
    /// thread, in order with mutations. Answers with
    /// [`SyncEvent::LocalDone`](crate::SyncEvent::LocalDone).
    Local {
        /// The caller's token.
        token: u64,
        /// The write.
        write: crate::local::LocalWrite,
    },
    /// Stop. Prefer [`SyncHandle::shutdown`](crate::SyncHandle::shutdown),
    /// which also waits for the threads.
    Shutdown,
}

/// A bulk action on every conversation in a mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkAction {
    /// Move to the Archive mailbox.
    Archive,
    /// Move to the Archive mailbox and mark read. Undo restores each
    /// message's own read state.
    ArchiveRead,
    /// Move to Trash.
    Trash,
    /// Move to the junk mailbox and mark as junk.
    Spam,
    /// Move to another mailbox.
    MoveTo(Id),
    /// Mark read or unread.
    SetSeen(bool),
    /// Pin or unpin.
    SetFlagged(bool),
}

/// The ids of the mailboxes with a role, for building action patches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleMailboxes {
    /// The Inbox.
    pub inbox: Option<Id>,
    /// Archive.
    pub archive: Option<Id>,
    /// Trash.
    pub trash: Option<Id>,
    /// The junk (Spam) mailbox.
    pub junk: Option<Id>,
    /// Drafts.
    pub drafts: Option<Id>,
    /// Sent.
    pub sent: Option<Id>,
}

impl RoleMailboxes {
    /// Picks the role mailboxes out of a mailbox list.
    pub fn from_rows(rows: &[MailboxRow]) -> Self {
        let mut out = RoleMailboxes::default();
        for row in rows {
            let id = || Some(row.id.clone());
            match row.role.as_ref().map(|r| r.as_str()) {
                Some("inbox") => out.inbox = id(),
                Some("archive") => out.archive = id(),
                Some("trash") => out.trash = id(),
                Some("junk") => out.junk = id(),
                Some("drafts") => out.drafts = id(),
                Some("sent") => out.sent = id(),
                _ => {}
            }
        }
        out
    }
}

impl BulkAction {
    /// The patch that applies this action to an email that is in
    /// `source`. Moves remove the email from `source` and add the target;
    /// other mailboxes it is in are kept. `None` when the target mailbox
    /// does not exist, or when the action would move mail into the mailbox
    /// it is already in.
    ///
    /// The binary can use this for single-conversation actions too, so
    /// both paths mean the same thing.
    pub fn patch(&self, source: &Id, roles: &RoleMailboxes) -> Option<Patch> {
        let move_to = |target: &Option<Id>| -> Option<Patch> {
            let target = target.as_ref()?;
            if target == source {
                return None;
            }
            Some(
                Patch::new()
                    .mailbox(source.as_str(), false)
                    .mailbox(target.as_str(), true),
            )
        };
        match self {
            BulkAction::Archive => move_to(&roles.archive),
            BulkAction::ArchiveRead => {
                move_to(&roles.archive).map(|p| p.keyword(keyword::SEEN, true))
            }
            BulkAction::Trash => move_to(&roles.trash),
            BulkAction::Spam => move_to(&roles.junk).map(|p| {
                p.keyword(keyword::JUNK, true)
                    .keyword(keyword::NOT_JUNK, false)
            }),
            BulkAction::MoveTo(target) => move_to(&Some(target.clone())),
            BulkAction::SetSeen(value) => Some(Patch::new().keyword(keyword::SEEN, *value)),
            BulkAction::SetFlagged(value) => Some(Patch::new().keyword(keyword::FLAGGED, *value)),
        }
    }

    /// A default undo label for `count` messages, such as "Archive 12".
    pub fn label(&self, count: usize) -> String {
        let verb = match self {
            BulkAction::Archive | BulkAction::ArchiveRead => "Archive",
            BulkAction::Trash => "Trash",
            BulkAction::Spam => "Report spam",
            BulkAction::MoveTo(_) => "Move",
            BulkAction::SetSeen(true) => "Mark read",
            BulkAction::SetSeen(false) => "Mark unread",
            BulkAction::SetFlagged(true) => "Pin",
            BulkAction::SetFlagged(false) => "Unpin",
        };
        format!("{verb} {count}")
    }
}

/// A masked address state the user can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskedState {
    /// Receives mail.
    Enabled,
    /// Mail goes straight to Trash.
    Disabled,
    /// Mail bounces. Can be restored.
    Deleted,
}

impl MaskedState {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            MaskedState::Enabled => "enabled",
            MaskedState::Disabled => "disabled",
            MaskedState::Deleted => "deleted",
        }
    }
}

/// A server search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Fastmail-style search text, such as `from:juniper is:unread rota`.
    pub query: String,
    /// Only search this mailbox. An `in:` in the query also limits it.
    pub mailbox: Option<Id>,
    /// Where the page starts.
    pub position: u64,
    /// How many results.
    pub limit: u64,
    /// One result per thread.
    pub collapse_threads: bool,
}

impl SearchRequest {
    /// The first page of 50 results for `query`, one per message.
    pub fn new(query: impl Into<String>) -> Self {
        SearchRequest {
            query: query.into(),
            mailbox: None,
            position: 0,
            limit: 50,
            collapse_threads: false,
        }
    }
}

/// A message to send. It is stored in the outbox as JSON until it goes out,
/// so it survives a restart.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingMessage {
    /// The sending identity.
    pub identity_id: Id,
    /// The From address. The identity must allow it.
    pub from: EmailAddress,
    /// Recipients.
    #[serde(default)]
    pub to: Vec<EmailAddress>,
    /// Copy recipients.
    #[serde(default)]
    pub cc: Vec<EmailAddress>,
    /// Blind copy recipients.
    #[serde(default)]
    pub bcc: Vec<EmailAddress>,
    /// Reply-To addresses.
    #[serde(default)]
    pub reply_to: Vec<EmailAddress>,
    /// The subject.
    #[serde(default)]
    pub subject: String,
    /// The plain text body.
    #[serde(default)]
    pub text_body: String,
    /// An HTML alternative.
    #[serde(default)]
    pub html_body: Option<String>,
    /// Message-IDs this replies to, without angle brackets.
    #[serde(default)]
    pub in_reply_to: Vec<String>,
    /// The thread's Message-IDs, without angle brackets.
    #[serde(default)]
    pub references: Vec<String>,
    /// Uploaded attachments.
    #[serde(default)]
    pub attachments: Vec<OutgoingAttachment>,
    /// The local draft to delete once the message is sent.
    #[serde(default)]
    pub draft_local_id: Option<String>,
}

// Bodies and addresses are private; logs only ever see the shape.
impl fmt::Debug for OutgoingMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutgoingMessage")
            .field("identity_id", &self.identity_id)
            .field(
                "recipients",
                &(self.to.len() + self.cc.len() + self.bcc.len()),
            )
            .field("text_bytes", &self.text_body.len())
            .field("html", &self.html_body.is_some())
            .field("attachments", &self.attachments.len())
            .field("reply", &!self.in_reply_to.is_empty())
            .finish_non_exhaustive()
    }
}

/// An uploaded file to attach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingAttachment {
    /// From [`SyncEvent::Uploaded`](crate::SyncEvent::Uploaded).
    pub blob_id: Id,
    /// The file name.
    pub name: String,
    /// The media type.
    pub content_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles() -> RoleMailboxes {
        RoleMailboxes {
            inbox: Some("in".into()),
            archive: Some("ar".into()),
            trash: Some("tr".into()),
            junk: Some("ju".into()),
            drafts: None,
            sent: None,
        }
    }

    fn json(p: Option<Patch>) -> serde_json::Value {
        serde_json::to_value(p.expect("a patch")).unwrap()
    }

    #[test]
    fn action_patches() {
        let inbox = Id::from("in");
        assert_eq!(
            json(BulkAction::Archive.patch(&inbox, &roles())),
            serde_json::json!({"mailboxIds/in": null, "mailboxIds/ar": true})
        );
        assert_eq!(
            json(BulkAction::Spam.patch(&inbox, &roles())),
            serde_json::json!({
                "mailboxIds/in": null, "mailboxIds/ju": true,
                "keywords/$junk": true, "keywords/$notjunk": null
            })
        );
        assert_eq!(
            json(BulkAction::SetFlagged(true).patch(&inbox, &roles())),
            serde_json::json!({"keywords/$flagged": true})
        );
        assert_eq!(
            json(BulkAction::SetSeen(false).patch(&inbox, &roles())),
            serde_json::json!({"keywords/$seen": null})
        );
        assert_eq!(
            json(BulkAction::MoveTo("x".into()).patch(&inbox, &roles())),
            serde_json::json!({"mailboxIds/in": null, "mailboxIds/x": true})
        );
        assert_eq!(
            json(BulkAction::ArchiveRead.patch(&inbox, &roles())),
            serde_json::json!({
                "mailboxIds/in": null, "mailboxIds/ar": true, "keywords/$seen": true
            })
        );
        assert!(
            BulkAction::ArchiveRead
                .patch(&Id::from("ar"), &roles())
                .is_none()
        );
        assert_eq!(BulkAction::ArchiveRead.label(2), "Archive 2");
        // Archiving from Archive, or with no Archive mailbox, does nothing.
        assert!(
            BulkAction::Archive
                .patch(&Id::from("ar"), &roles())
                .is_none()
        );
        assert!(
            BulkAction::Archive
                .patch(&inbox, &RoleMailboxes::default())
                .is_none()
        );
        assert_eq!(BulkAction::Archive.label(3), "Archive 3");
        assert_eq!(BulkAction::SetSeen(false).label(1), "Mark unread 1");
        assert_eq!(MaskedState::Disabled.as_str(), "disabled");
    }

    #[test]
    fn outgoing_debug_hides_content() {
        let m = OutgoingMessage {
            identity_id: "I1".into(),
            from: EmailAddress::with_name("Mira Holt", "mira@example.org"),
            to: vec![EmailAddress::new("juniper@example.org")],
            cc: vec![],
            bcc: vec![],
            reply_to: vec![],
            subject: "Secret plans".into(),
            text_body: "the body text".into(),
            html_body: None,
            in_reply_to: vec![],
            references: vec![],
            attachments: vec![],
            draft_local_id: None,
        };
        let shown = format!("{m:?}");
        assert!(!shown.contains("Secret"), "{shown}");
        assert!(!shown.contains("body text"), "{shown}");
        assert!(!shown.contains("juniper"), "{shown}");
        let round: OutgoingMessage =
            serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(round, m);
    }
}
