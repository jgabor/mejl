//! The UI's read model.
//!
//! [`MailData`] is the only way the UI reads mail. It is synchronous and
//! must stay fast: every call is a bounded, indexed read of the local store
//! (architecture.md budgets 2 ms p99 for a page read). It never touches the
//! network. When something is not cached yet, a method returns `None` or an
//! empty value and the UI asks for it with a [`Cmd`](crate::Cmd).
//!
//! The view model structs here are plain owned data, decoupled from JMAP
//! wire types and from the SQLite schema. The binary implements
//! `MailData` over mejl-store's reader; tests and snapshots use
//! [`MemoryData`].
//!
//! The UI only ever asks for the rows it is about to draw:
//! [`MailData::conversation_page`] takes an offset and a limit, and the UI
//! caches a few small chunks keyed by offset. A 100k conversation mailbox
//! is never loaded as a whole.

mod fixture;
mod memory;

use std::fmt;

use jiff::Timestamp;

pub use memory::{MemoryData, NewEmail, ReadStats};

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub String);

        impl $name {
            /// Wraps an id string.
            pub fn new(id: impl Into<String>) -> Self {
                $name(id.into())
            }

            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                $name(id.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                $name(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// A JMAP mailbox id.
    MailboxId
);
id_type!(
    /// A JMAP thread id. The list shows one row per thread (conversation).
    ThreadId
);
id_type!(
    /// A JMAP email id.
    EmailId
);
id_type!(
    /// A JMAP blob id, used for attachments.
    BlobId
);
id_type!(
    /// mejl's local id for a draft. The UI makes one when compose opens
    /// (from the clock and a counter), so autosaves, sends and cancels of
    /// one message always carry the same id, even before the shell answers.
    DraftId
);

/// The special-use role of a mailbox (RFC 8621).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MailboxRole {
    /// The inbox.
    Inbox,
    /// Drafts.
    Drafts,
    /// Sent mail.
    Sent,
    /// The archive.
    Archive,
    /// Spam, called "junk" in JMAP.
    Junk,
    /// Trash.
    Trash,
}

impl MailboxRole {
    /// Sidebar order of role mailboxes, matching Fastmail's web app.
    pub const ORDER: [MailboxRole; 6] = [
        MailboxRole::Inbox,
        MailboxRole::Drafts,
        MailboxRole::Sent,
        MailboxRole::Archive,
        MailboxRole::Junk,
        MailboxRole::Trash,
    ];

    /// The display name used when a mailbox of this role has no name.
    pub fn default_name(self) -> &'static str {
        match self {
            MailboxRole::Inbox => "Inbox",
            MailboxRole::Drafts => "Drafts",
            MailboxRole::Sent => "Sent",
            MailboxRole::Archive => "Archive",
            MailboxRole::Junk => "Spam",
            MailboxRole::Trash => "Trash",
        }
    }
}

/// One mailbox for the sidebar and the top line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxView {
    /// Mailbox id.
    pub id: MailboxId,
    /// Parent mailbox for folders in a tree.
    pub parent: Option<MailboxId>,
    /// Display name (one path segment, not the full path).
    pub name: String,
    /// Special-use role, if any.
    pub role: Option<MailboxRole>,
    /// Nesting depth in the folder tree, 0 for top level.
    pub depth: u8,
    /// Sort order among siblings (JMAP `sortOrder`).
    pub sort_order: u32,
    /// Conversations in the mailbox.
    pub total_threads: u32,
    /// Conversations with at least one unread message.
    pub unread_threads: u32,
    /// Messages in the mailbox.
    pub total_emails: u32,
    /// Unread messages in the mailbox.
    pub unread_emails: u32,
}

/// A sender or recipient.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Participant {
    /// Display name, if the header had one.
    pub name: Option<String>,
    /// Email address.
    pub email: String,
}

impl Participant {
    /// A participant with a display name.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Participant {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    /// The name if present and not blank, else the address.
    pub fn display(&self) -> &str {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.email,
        }
    }
}

/// One row of the conversation list. Selection and cursor state are UI
/// state and are not part of the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationRow {
    /// The conversation.
    pub thread_id: ThreadId,
    /// Newest message of the conversation in this mailbox, used for
    /// prefetching its body.
    pub latest_email_id: EmailId,
    /// Sender of the newest message (or the recipients' first entry in
    /// Sent, as the store decides).
    pub from: Participant,
    /// Distinct participants, newest first.
    pub participants: Vec<Participant>,
    /// Subject of the conversation.
    pub subject: String,
    /// Preview text of the newest message (JMAP `preview`).
    pub preview: String,
    /// Received date of the newest message.
    pub date: Timestamp,
    /// Messages in the conversation.
    pub message_count: u32,
    /// At least one message in this mailbox is unread.
    pub unread: bool,
    /// At least one message is pinned (`$flagged`).
    pub flagged: bool,
    /// At least one message has an attachment.
    pub has_attachment: bool,
    /// At least one message was delivered to a Masked Email address.
    pub masked: bool,
}

/// A window of a longer list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    /// Index of `items[0]` in the whole list.
    pub offset: usize,
    /// The items, at most the requested limit.
    pub items: Vec<T>,
    /// Length of the whole list.
    pub total: usize,
}

impl<T> Page<T> {
    /// An empty page of an empty list.
    pub fn empty() -> Self {
        Page {
            offset: 0,
            items: Vec::new(),
            total: 0,
        }
    }
}

/// One message inside a conversation, without its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSummary {
    /// Message id.
    pub email_id: EmailId,
    /// Sender.
    pub from: Participant,
    /// To recipients.
    pub to: Vec<Participant>,
    /// Cc recipients.
    pub cc: Vec<Participant>,
    /// Received date.
    pub date: Timestamp,
    /// Preview text.
    pub preview: String,
    /// Not yet `$seen`.
    pub unread: bool,
    /// Pinned (`$flagged`).
    pub flagged: bool,
    /// Has at least one attachment.
    pub has_attachment: bool,
    /// Mailboxes the message is in.
    pub mailboxes: Vec<MailboxId>,
    /// Reply-To addresses. Replies go here instead of `from` when set.
    pub reply_to: Vec<Participant>,
    /// The Message-ID header, without angle brackets, for threading
    /// replies.
    pub message_id: Option<String>,
    /// The References header ids, without angle brackets, oldest first.
    pub references: Vec<String>,
}

/// A whole conversation for the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationView {
    /// The conversation.
    pub thread_id: ThreadId,
    /// Subject.
    pub subject: String,
    /// Messages, oldest first.
    pub messages: Vec<MessageSummary>,
}

/// An attachment of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentView {
    /// Blob to download.
    pub blob_id: BlobId,
    /// File name, sanitized by the UI before display.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Size in bytes.
    pub size: u64,
}

/// A cached message body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyView {
    /// Message id.
    pub email_id: EmailId,
    /// The `text/plain` part, if fetched and present.
    pub text: Option<String>,
    /// The `text/html` part, if fetched and present.
    pub html: Option<String>,
    /// Attachments, in message order.
    pub attachments: Vec<AttachmentView>,
    /// Raw headers as name and value pairs, for the full headers view.
    pub headers: Vec<(String, String)>,
}

/// A sending identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityView {
    /// JMAP identity id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Address. `*@domain` for a catch-all identity.
    pub email: String,
    /// Plain text signature (JMAP `textSignature`), without the `-- `
    /// separator. Empty for none.
    pub signature: String,
}

/// State of a Masked Email address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaskedState {
    /// Created but never used. Fastmail deletes it after 24 hours.
    Pending,
    /// Delivers mail.
    Enabled,
    /// Files mail in Trash.
    Disabled,
    /// Bounces mail.
    Deleted,
}

/// A Masked Email address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskedEmailView {
    /// Masked Email object id.
    pub id: String,
    /// The address.
    pub email: String,
    /// Current state.
    pub state: MaskedState,
    /// The site it was made for (`forDomain`).
    pub for_domain: String,
    /// User description.
    pub description: String,
    /// Creation time.
    pub created_at: Option<Timestamp>,
    /// Last mail received.
    pub last_message_at: Option<Timestamp>,
    /// Who made it, as set by the server.
    pub created_by: String,
    /// Cached messages delivered to this address.
    pub local_messages: u32,
    /// Of those, how many are in Spam.
    pub local_in_spam: u32,
}

/// A contact for address completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactView {
    /// Contact id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Addresses, preferred first.
    pub emails: Vec<String>,
}

/// The signed-in account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    /// Account owner's name.
    pub name: String,
    /// Primary address, shown in the top line.
    pub email: String,
}

/// A saved search, local to mejl.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSearchView {
    /// Local id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The query in Fastmail search syntax.
    pub query: String,
    /// Unread matches, when known.
    pub unread: Option<u32>,
}

/// A mejl rule as stored, kept opaque so the UI crate does not depend on
/// the rule model. The rules screen parses it with mejl-sieve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleJson {
    /// Rule id.
    pub id: String,
    /// The rule as JSON.
    pub json: String,
}

/// What compose is answering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ComposeKind {
    /// A new message.
    #[default]
    New,
    /// A reply to the sender.
    Reply,
    /// A reply to everyone.
    ReplyAll,
    /// A forward.
    Forward,
}

/// An attachment of an outgoing message, already uploaded as a blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingAttachment {
    /// The uploaded blob (or the original message's blob, when forwarding).
    pub blob_id: BlobId,
    /// File name.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Size in bytes.
    pub size: u64,
}

/// A message as compose holds it: the payload of [`Cmd::SaveDraft`] and
/// [`Cmd::Send`](crate::Cmd::Send), and what [`MailData::drafts`] returns.
///
/// [`Cmd::SaveDraft`]: crate::Cmd::SaveDraft
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// mejl's local draft id; stable for the life of the message.
    pub draft_id: DraftId,
    /// What this message answers.
    pub kind: ComposeKind,
    /// The JMAP identity to send with. When `from` is a Masked Email
    /// address this is the account's default identity, and the shell
    /// decides how to send as the mask.
    pub identity_id: Option<String>,
    /// The From address.
    pub from: Participant,
    /// To recipients, already checked by the UI.
    pub to: Vec<Participant>,
    /// Cc recipients.
    pub cc: Vec<Participant>,
    /// Bcc recipients.
    pub bcc: Vec<Participant>,
    /// Subject.
    pub subject: String,
    /// The plain text body, lines joined with `\n`.
    pub text_body: String,
    /// In-Reply-To message ids, without angle brackets.
    pub in_reply_to: Vec<String>,
    /// References message ids, without angle brackets, oldest first.
    pub references: Vec<String>,
    /// The message replied to or forwarded, so the shell can set
    /// `$answered` or `$forwarded` on it after sending.
    pub original: Option<EmailId>,
    /// Attachments.
    pub attachments: Vec<OutgoingAttachment>,
}

/// A saved draft that compose can resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftView {
    /// The draft.
    pub message: OutgoingMessage,
    /// The server copy in Drafts, once uploaded.
    pub server_email_id: Option<EmailId>,
    /// Last local save.
    pub updated_at: Timestamp,
}

/// Text with byte ranges to highlight, from search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Highlight {
    /// The text, unsanitised; the UI sanitises between the ranges.
    pub text: String,
    /// Byte ranges into `text` that matched. Ranges that are out of
    /// bounds or not on char boundaries are ignored.
    pub matches: Vec<std::ops::Range<usize>>,
}

/// One local search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// The conversation, as the list shows it.
    pub row: ConversationRow,
    /// A mailbox the matching message is in, used as the "from mailbox"
    /// when the user archives or moves a result.
    pub mailbox: Option<MailboxId>,
    /// The subject with matches marked.
    pub subject: Highlight,
    /// A snippet of the matching text with matches marked.
    pub snippet: Highlight,
}

/// Why part of a query was not run locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchIssueKind {
    /// The operator exists in Fastmail but the local index cannot run it.
    UnsupportedOperator,
    /// The operator is known but this value is not.
    UnsupportedValue,
    /// `in:` names no mailbox.
    UnknownMailbox,
    /// A date that is not YYYY-MM-DD, YYYY-MM or YYYY.
    BadDate,
    /// A size that is not a number with an optional k, m or g.
    BadSize,
    /// An operator with nothing after the colon.
    EmptyValue,
    /// OR between operators, which the local index cannot combine.
    MisplacedOr,
    /// Parentheses.
    Grouping,
}

/// A part of a query the local index skipped. The server search may still
/// run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchIssue {
    /// The term as typed.
    pub term: String,
    /// What is wrong.
    pub kind: SearchIssueKind,
}

/// Local search results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// Matches, best first, one per conversation.
    pub hits: Vec<SearchHit>,
    /// Terms the local index did not run.
    pub issues: Vec<SearchIssue>,
    /// True when message bodies were searched too (false when only
    /// headers are indexed).
    pub full_text: bool,
}

/// One copy of the rendered rule block that the user was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleRenderView {
    /// Render id, for [`Cmd::ConfirmRulePaste`](crate::Cmd::ConfirmRulePaste).
    pub id: i64,
    /// When it was copied.
    pub rendered_at: Timestamp,
    /// The block as copied.
    pub sieve: String,
    /// The block's body hash (mejl-sieve `RenderedBlock::body_sha256`).
    pub body_sha256: String,
    /// When the user confirmed the paste, if they did.
    pub confirmed_at: Option<Timestamp>,
}

/// A cached message with the header facts the rule dry run needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRunMessage {
    /// The message.
    pub email_id: EmailId,
    /// Its conversation.
    pub thread_id: ThreadId,
    /// Received date.
    pub date: Timestamp,
    /// Mailboxes it is in now.
    pub mailboxes: Vec<MailboxId>,
    /// Headers, addresses and size for mejl-sieve's matcher.
    pub facts: mejl_sieve::MessageFacts,
}

/// Fast, synchronous, local reads for the UI.
///
/// Implementations read from a local cache only. They must not block on
/// the network and must keep every call bounded: no method may scan a whole
/// mailbox on each call. Results reflect pending optimistic changes (the
/// outbox overlays), so a mutation is visible as soon as the shell has
/// applied it locally.
pub trait MailData {
    /// The signed-in account.
    fn account(&self) -> AccountView;

    /// Every mailbox, in any order. The UI sorts roles and builds the tree.
    fn mailboxes(&self) -> Vec<MailboxView>;

    /// Conversations in `mailbox`, newest first, starting at `offset`. At
    /// most `limit` rows are returned; `total` is the length of the list.
    fn conversation_page(
        &self,
        mailbox: &MailboxId,
        offset: usize,
        limit: usize,
    ) -> Page<ConversationRow>;

    /// Position of a conversation in `mailbox`'s list, so the cursor can
    /// stay on it when new mail arrives above it.
    fn conversation_index(&self, mailbox: &MailboxId, thread: &ThreadId) -> Option<usize>;

    /// A whole conversation without bodies, or `None` if unknown.
    fn conversation(&self, thread: &ThreadId) -> Option<ConversationView>;

    /// A message body, or `None` when it is not cached yet.
    fn message_body(&self, email: &EmailId) -> Option<BodyView>;

    /// Sending identities. Also used to show "(you)" and "me".
    fn identities(&self) -> Vec<IdentityView>;

    /// Every Masked Email address.
    fn masked_emails(&self) -> Vec<MaskedEmailView>;

    /// The Masked Email address equal to `address`, ignoring case.
    fn masked_for_address(&self, address: &str) -> Option<MaskedEmailView>;

    /// Contacts whose name or address starts with `prefix`, best first.
    fn contacts_prefix(&self, prefix: &str, limit: usize) -> Vec<ContactView>;

    /// Local search over cached mail in Fastmail search syntax, best
    /// first, at most `limit` conversations. Runs on every keystroke, so
    /// it must stay within a few milliseconds. Terms the local index
    /// cannot run are reported in `issues`, never silently dropped.
    fn search_local(&self, query: &str, limit: usize) -> SearchResults;

    /// The JMAP `Email/query` filter a server search for `query` would
    /// send, as compact JSON (`null` when it matches every message), for
    /// the search screen to show as the user types. `None` when the
    /// implementation cannot tell, and the screen then leaves it out. The
    /// filter must come from the code that builds the real request, never
    /// from a second parser. Runs on every keystroke, so it must be cheap.
    fn search_filter_preview(&self, query: &str) -> Option<String> {
        let _ = query;
        None
    }

    /// Saved searches for the sidebar.
    fn saved_searches(&self) -> Vec<SavedSearchView>;

    /// mejl's rules, as opaque JSON (a serialised `mejl_sieve::Rule`), in
    /// rule order.
    fn rules(&self) -> Vec<RuleJson>;

    /// Drafts that compose can resume, newest first.
    fn drafts(&self) -> Vec<DraftView>;

    /// The rule blocks copied so far, newest first, at most `limit`.
    fn rule_renders(&self, limit: usize) -> Vec<RuleRenderView>;

    /// The newest cached messages with their header facts, newest first,
    /// at most `limit`, for the rule dry run. Bounded and indexed like
    /// every other read.
    fn dry_run_messages(&self, limit: usize) -> Vec<DryRunMessage>;
}
