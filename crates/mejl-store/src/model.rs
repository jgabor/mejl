//! Rows the readers return and records the UI writes.

use std::collections::BTreeSet;
use std::ops::Range;

use jiff::Timestamp;
use mejl_jmap::types::{EmailAddress, EmailHeader, MailboxRights, MaskedEmail, Role};
use mejl_jmap::{Id, State};
use serde::{Deserialize, Serialize};

/// One page of a list, plus the number of rows in the whole list.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    /// The rows, at most the requested limit.
    pub rows: Vec<T>,
    /// The offset the page starts at.
    pub offset: usize,
    /// Rows in the whole list in the local cache.
    pub total: u64,
}

/// A mailbox with counts adjusted for pending local changes.
#[derive(Debug, Clone, PartialEq)]
pub struct MailboxRow {
    /// The mailbox id, or a `local:` placeholder while pending.
    pub id: Id,
    /// The parent mailbox, `None` at the top level.
    pub parent_id: Option<Id>,
    /// The display name.
    pub name: String,
    /// The role, such as inbox or trash.
    pub role: Option<Role>,
    /// The server's sort order; lower first.
    pub sort_order: u32,
    /// The server's count with pending local changes applied.
    pub total_emails: u64,
    /// The server's count with pending local changes applied.
    pub unread_emails: u64,
    /// The server's count with pending local changes applied.
    pub total_threads: u64,
    /// The server's count with pending local changes applied.
    pub unread_threads: u64,
    /// Messages in this mailbox in the local cache.
    pub local_messages: u64,
    /// Conversations in this mailbox in the local cache.
    pub local_conversations: u64,
    /// Whether the user subscribes to it.
    pub is_subscribed: bool,
    /// What the user may do with it, when the server sent rights.
    pub my_rights: Option<MailboxRights>,
    /// A mailbox created locally that the server has not confirmed yet. Its
    /// id is a local placeholder until then.
    pub pending: bool,
}

/// One conversation in a mailbox list: the newest message of a thread in
/// that mailbox, with summary flags over the thread's messages in it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRow {
    /// The thread.
    pub thread_id: Id,
    /// The newest message of the thread in this mailbox.
    pub email_id: Id,
    /// When the newest message in this mailbox arrived.
    pub received_at: Timestamp,
    /// Subject of the newest message.
    pub subject: String,
    /// Preview of the newest message.
    pub preview: String,
    /// `From` of the newest message.
    pub from: Vec<EmailAddress>,
    /// Distinct senders across the cached thread, oldest first, at most 8.
    pub participants: Vec<EmailAddress>,
    /// Messages of the thread in this mailbox.
    pub count_in_mailbox: u32,
    /// Messages of the thread in the cache, in any mailbox.
    pub thread_count: u32,
    /// Unread messages of the thread in this mailbox.
    pub unread_count: u32,
    /// Any message of the thread in this mailbox is pinned.
    pub flagged: bool,
    /// Any message of the thread in this mailbox has an attachment.
    pub has_attachment: bool,
    /// Any message of the thread in this mailbox came to a masked address.
    pub masked: bool,
}

impl ConversationRow {
    /// Whether any message in the conversation is unread.
    pub fn is_unread(&self) -> bool {
        self.unread_count > 0
    }
}

/// One message in a flat list or a search result.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRow {
    /// The email id.
    pub email_id: Id,
    /// Its thread.
    pub thread_id: Id,
    /// When it arrived.
    pub received_at: Timestamp,
    /// The subject, empty when it had none.
    pub subject: String,
    /// The server's plain text preview.
    pub preview: String,
    /// `From` addresses.
    pub from: Vec<EmailAddress>,
    /// `To` addresses.
    pub to: Vec<EmailAddress>,
    /// `Cc` addresses.
    pub cc: Vec<EmailAddress>,
    /// Size in bytes as the server reports it.
    pub size: u64,
    /// Read, with pending changes applied.
    pub seen: bool,
    /// Pinned, with pending changes applied.
    pub flagged: bool,
    /// Replied to.
    pub answered: bool,
    /// A draft.
    pub draft: bool,
    /// The server says it has an attachment.
    pub has_attachment: bool,
    /// The message came to a masked address (it has the masked header).
    pub masked: bool,
    /// The masked address it came to, when that mask is in the cache.
    pub masked_email_id: Option<Id>,
}

/// Every cached header-level property of a message.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageHeaders {
    /// The list row fields.
    pub row: MessageRow,
    /// `Bcc` addresses, known for sent mail.
    pub bcc: Vec<EmailAddress>,
    /// `Reply-To` addresses.
    pub reply_to: Vec<EmailAddress>,
    /// `Sender` addresses.
    pub sender: Vec<EmailAddress>,
    /// RFC 3339 with the sender's offset.
    pub sent_at: Option<String>,
    /// `Message-ID` values, without angle brackets.
    pub message_id: Vec<String>,
    /// `In-Reply-To` values.
    pub in_reply_to: Vec<String>,
    /// `References` values.
    pub references: Vec<String>,
    /// The masked address from the header, lowercased.
    pub masked_address: Option<String>,
    /// Keywords with pending local changes applied.
    pub keywords: BTreeSet<String>,
    /// Mailboxes with pending local changes applied.
    pub mailbox_ids: BTreeSet<Id>,
    /// Whether the body is in the cache.
    pub body_cached: bool,
}

/// A message with its body, if the body is cached.
#[derive(Debug, Clone, PartialEq)]
pub struct EmailDetail {
    /// Everything but the body.
    pub headers: MessageHeaders,
    /// The body, when cached.
    pub body: Option<Body>,
}

/// A fetched body.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    /// `textBody` parts in order, with their decoded values.
    pub text_parts: Vec<BodyPart>,
    /// `htmlBody` parts in order, with their decoded values.
    pub html_parts: Vec<BodyPart>,
    /// Attachment metadata in order.
    pub attachments: Vec<Attachment>,
    /// The raw header list, when it was fetched.
    pub headers: Vec<EmailHeader>,
    /// When the body was stored.
    pub fetched_at: Timestamp,
    /// When the user last opened it, which orders eviction.
    pub opened_at: Option<Timestamp>,
}

/// A text or HTML body part with its value.
#[derive(Debug, Clone, PartialEq)]
pub struct BodyPart {
    /// The part id from the server.
    pub part_id: Option<String>,
    /// The blob holding the raw part.
    pub blob_id: Option<Id>,
    /// Content type, such as `text/plain`.
    pub mime_type: Option<String>,
    /// The declared character set.
    pub charset: Option<String>,
    /// The decoded value. Empty when the server sent no value for the part.
    pub value: String,
    /// The server cut the value at `maxBodyValueBytes`.
    pub is_truncated: bool,
    /// The server could not decode the part cleanly.
    pub is_encoding_problem: bool,
}

/// Attachment metadata. The bytes are downloaded on demand.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// The part id.
    pub part_id: Option<String>,
    /// The blob to download.
    pub blob_id: Option<Id>,
    /// The file name, if given.
    pub name: Option<String>,
    /// Content type.
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: u64,
    /// `attachment` or `inline`.
    pub disposition: Option<String>,
    /// The Content-ID for inline images.
    pub cid: Option<String>,
}

/// An address offered by compose autocomplete.
#[derive(Debug, Clone, PartialEq)]
pub struct AddressSuggestion {
    /// The display name, if known.
    pub name: Option<String>,
    /// The address.
    pub email: String,
    /// The contact card it came from, if any.
    pub contact_id: Option<Id>,
    /// When mail was last sent to this address, if known.
    pub last_used: Option<Timestamp>,
    /// How many cached sent messages went to this address.
    pub use_count: u64,
}

/// A masked address with local statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskedEmailRow {
    /// Server data with pending local changes applied.
    pub masked: MaskedEmail,
    /// Cached messages that came to this address.
    pub local_messages: u64,
    /// Of those, how many are in the junk mailbox.
    pub local_in_spam: u64,
    /// The newest cached message that came to this address.
    pub last_seen: Option<Timestamp>,
    /// Created or changed locally and not confirmed by the server yet.
    pub pending: bool,
}

/// A cached query result window.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPage {
    /// The requested slice of cached ids.
    pub ids: Vec<Id>,
    /// The server's total, when it was asked for.
    pub total: Option<u64>,
    /// The `queryState` the ids belong to.
    pub query_state: State,
    /// Whether the query collapsed threads.
    pub collapse_threads: bool,
    /// Whether the server offers `/queryChanges` for it.
    pub can_calculate_changes: bool,
    /// How many ids are cached from position 0.
    pub cached: usize,
}

/// Text with highlighted ranges, from a local search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Highlighted {
    /// The plain text.
    pub text: String,
    /// Byte ranges of `text` that matched.
    pub matches: Vec<Range<usize>>,
}

/// A local draft. The store writes it in one transaction, so a crash keeps
/// either the old or the new version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// The caller's id for the draft, stable across saves.
    pub local_id: String,
    /// The server copy, once one was saved.
    pub server_email_id: Option<Id>,
    /// The identity to send as.
    pub identity_id: Option<Id>,
    /// Header fields.
    pub headers: DraftHeaders,
    /// The body text as typed.
    pub body: String,
    /// For example `markdown` or `plain`.
    pub format: String,
    /// Attachment references as the compose view keeps them.
    pub attachments: serde_json::Value,
    /// When the draft was first saved.
    pub created_at: Timestamp,
    /// When it was last saved.
    pub updated_at: Timestamp,
}

/// Header fields of a draft.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftHeaders {
    /// The sending identity or address.
    #[serde(default)]
    pub from: Option<EmailAddress>,
    /// `To` recipients.
    #[serde(default)]
    pub to: Vec<EmailAddress>,
    /// `Cc` recipients.
    #[serde(default)]
    pub cc: Vec<EmailAddress>,
    /// `Bcc` recipients.
    #[serde(default)]
    pub bcc: Vec<EmailAddress>,
    /// The subject.
    #[serde(default)]
    pub subject: String,
    /// The message being replied to or forwarded.
    #[serde(default)]
    pub in_reply_to_email_id: Option<Id>,
    /// Anything else the compose view wants to keep.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A mejl rule. The rule body is the UI's serialized rule model, kept as
/// opaque JSON so this crate does not depend on mejl-sieve.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleRecord {
    /// The rule id, stable across renames.
    pub id: String,
    /// The name shown in the rule list.
    pub name: String,
    /// Whether it renders into the Sieve block.
    pub enabled: bool,
    /// Lower sorts first. Rules render in this order.
    pub sort_order: i64,
    /// The serialized rule.
    pub rule_json: String,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// One rendered Sieve block in the render history.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleRender {
    /// The history entry id.
    pub id: i64,
    /// When it was rendered.
    pub rendered_at: Timestamp,
    /// The rendered block.
    pub sieve: String,
    /// The hash in the block's end marker.
    pub body_sha256: String,
    /// When the user said they pasted this block into Fastmail.
    pub user_confirmed_paste_at: Option<Timestamp>,
}

/// Counts for the status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxSummary {
    /// Entries waiting to be sent or being sent, not counting held sends.
    pub pending: u64,
    /// Sends inside their undo window.
    pub held: u64,
    /// Entries that failed and are not pruned yet.
    pub failed: u64,
}

/// A send waiting in the outbox.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedSend {
    /// The outbox entry.
    pub seq: crate::outbox::Seq,
    /// The label it was queued with.
    pub label: Option<String>,
    /// `None` once the hold is over or was released.
    pub hold_until: Option<Timestamp>,
    /// Being sent right now.
    pub in_flight: bool,
    /// Send attempts so far.
    pub attempts: u32,
    /// The last error, if an attempt failed.
    pub last_error: Option<String>,
}
