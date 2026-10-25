//! What goes into the UI and what comes out of it.
//!
//! - [`Event`] is everything the UI reacts to: terminal input and
//!   [`Msg`]s from the outside world (sync engine, store, workers, the
//!   shell's frame timer).
//! - [`Cmd`] is everything the UI asks the outside world to do. The UI
//!   never performs I/O itself.
//!
//! # The optimistic mutation contract
//!
//! 1. A triage key produces `Cmd::Mutate(m)`.
//! 2. The shell applies `m` to the local store at once (an outbox entry
//!    plus an overlay on cached rows) and only then queues the JMAP call.
//! 3. The shell feeds `Msg::DataChanged` back into
//!    [`update`](crate::update) before it draws the next frame.
//! 4. `update` re-reads the visible page through
//!    [`MailData`](crate::MailData), and the frame shows the result.
//!
//! So a mutation is visible in the same frame as the key press, without the
//! UI keeping its own copy of mail state. When the server later rejects the
//! change, the shell drops the overlay, sends `Msg::DataChanged` again and
//! reports the failure with `Msg::Outbox`.

use std::time::Duration;

use crossterm::event::{KeyEvent, KeyEventKind, MouseEvent};
use jiff::Timestamp;
use mejl_render::RenderedBody;

use crate::data::{
    BlobId, ConversationRow, DraftId, EmailId, MailboxId, MaskedEmailView, MaskedState,
    OutgoingAttachment, OutgoingMessage, RuleJson, ThreadId,
};
use crate::keymap::Preset;
use crate::state::{Density, UiConfig};

/// An input to [`update`](crate::update).
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A key press or repeat. Release events are ignored.
    Key(KeyEvent),
    /// A mouse event. The shell should coalesce motion to one per frame.
    Mouse(MouseEvent),
    /// Bracketed paste. Never dispatched as keys.
    Paste(String),
    /// The terminal was resized to `(columns, rows)`.
    Resize(u16, u16),
    /// The terminal gained (`true`) or lost focus.
    Focus(bool),
    /// A message from outside the UI.
    Msg(Msg),
}

impl Event {
    /// Converts a crossterm event. Returns `None` for events the UI does
    /// not use, such as key releases.
    pub fn from_crossterm(event: crossterm::event::Event) -> Option<Event> {
        use crossterm::event::Event as C;
        match event {
            C::Key(k) if k.kind == KeyEventKind::Release => None,
            C::Key(k) => Some(Event::Key(k)),
            C::Mouse(m) => Some(Event::Mouse(m)),
            C::Paste(s) => Some(Event::Paste(s)),
            C::Resize(w, h) => Some(Event::Resize(w, h)),
            C::FocusGained => Some(Event::Focus(true)),
            C::FocusLost => Some(Event::Focus(false)),
        }
    }
}

/// Results and notices from outside the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// Sync engine progress.
    Sync(SyncStatus),
    /// Push connection state.
    Push(PushState),
    /// Local data changed: after a local mutation, a sync, or a rollback.
    DataChanged(DataChange),
    /// A body requested with [`Cmd::FetchBody`] or [`Cmd::Prefetch`] is
    /// now in the cache.
    BodyFetched(EmailId),
    /// A body could not be fetched.
    BodyFailed {
        /// The message.
        email_id: EmailId,
        /// What went wrong, in plain words.
        error: String,
    },
    /// A body rendered off the UI thread, answering [`Cmd::RenderBody`] or
    /// a prefetch.
    BodyRendered {
        /// The message.
        email_id: EmailId,
        /// Whether this rendering is of the HTML part by explicit request.
        prefer_html: bool,
        /// The rendering.
        body: Box<RenderedBody>,
    },
    /// Show a short notice in the status line.
    Toast {
        /// The notice.
        text: String,
        /// How it should look.
        level: ToastLevel,
    },
    /// Show an error in the status line.
    Error(String),
    /// The outcome of a queued mutation or send.
    Outbox(OutboxReport),
    /// Timing of one frame, measured by the shell.
    Perf(FrameSample),
    /// Time passed. Sent when a [`Cmd::WakeAt`] deadline is reached and
    /// once a second while the status clock is visible.
    Tick,
    /// A [`Cmd::SaveDraft`] finished.
    DraftSaved {
        /// The draft.
        draft_id: DraftId,
        /// What went wrong, if the local save failed.
        error: Option<String>,
    },
    /// Progress of a [`Cmd::Send`].
    Send(SendReport),
    /// The external editor from [`Cmd::OpenEditor`] closed.
    EditorDone {
        /// The draft whose body was edited.
        draft_id: DraftId,
        /// The new body, or what went wrong (the editor could not start,
        /// exited with an error, or the file could not be read).
        result: Result<String, String>,
    },
    /// A file from [`Cmd::AttachFile`] was uploaded, or could not be.
    Attached {
        /// The draft.
        draft_id: DraftId,
        /// The path as typed.
        path: String,
        /// The uploaded attachment, or what went wrong.
        result: Result<OutgoingAttachment, String>,
    },
    /// Results of a [`Cmd::ServerSearch`]. The rows must be readable
    /// through [`MailData`](crate::MailData) (cached) before this is sent,
    /// so opening a result works offline afterwards.
    ServerSearch {
        /// The query searched, so stale answers can be ignored.
        query: String,
        /// Conversations found, best first, or what went wrong.
        result: Result<Vec<ConversationRow>, String>,
    },
    /// A [`Cmd::MaskedCreate`] finished.
    MaskedCreated {
        /// The draft that asked for it, if compose did.
        draft_id: Option<DraftId>,
        /// The new address, or what went wrong.
        result: Result<MaskedEmailView, String>,
    },
    /// Settings changed, after a [`Cmd::SaveSetting`] or a config reload.
    /// The shell builds the whole config (theme, keymap with the user's
    /// overrides) and the UI applies it at once.
    Config(Box<UiConfig>),
}

/// The state of a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReport {
    /// The draft being sent.
    pub draft_id: DraftId,
    /// What happened.
    pub outcome: SendOutcome,
}

/// Steps and endings of a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// Queued; it goes out at `at` unless cancelled before.
    Scheduled {
        /// When the hold ends.
        at: Timestamp,
    },
    /// The server accepted the submission.
    Sent,
    /// [`Cmd::CancelSend`] stopped it in time. Compose reopens with the
    /// message.
    Cancelled {
        /// The message, to reopen as a draft.
        message: Box<OutgoingMessage>,
    },
    /// [`Cmd::CancelSend`] came too late; the message went out.
    TooLate,
    /// The server refused it, or it could not be sent.
    Failed {
        /// What went wrong, in plain words.
        error: String,
        /// The message, kept so nothing typed is lost.
        message: Box<OutgoingMessage>,
    },
}

/// A setting the settings screen can change. The shell writes it to the
/// config file and answers with [`Msg::Config`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Theme by name.
    Theme(String),
    /// Key preset.
    Preset(Preset),
    /// List row density.
    Density(Density),
    /// Show the HTML part when a message has both.
    PreferHtml(bool),
    /// How long a sent message waits so it can be cancelled.
    UndoSend(Duration),
    /// Load remote images (the reader never loads them itself; this is
    /// passed to the browser view).
    RemoteImages(bool),
    /// How long a message is shown before it is marked read. Zero marks
    /// it read on open.
    MarkReadDelay(Duration),
    /// Archiving also marks the mail read.
    MarkReadOnArchive(bool),
}

/// Which local data changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataChange {
    /// Mailbox names, tree or counts changed.
    pub mailboxes: bool,
    /// These conversations changed (membership, keywords, new messages).
    pub threads: Vec<ThreadId>,
    /// These messages changed.
    pub emails: Vec<EmailId>,
    /// Anything may have changed, for example after a full resync.
    pub all: bool,
    /// Masked Email addresses changed.
    pub masked: bool,
    /// mejl's rules or their copy history changed.
    pub rules: bool,
    /// Drafts changed.
    pub drafts: bool,
}

impl DataChange {
    /// A change that invalidates everything.
    pub fn everything() -> Self {
        DataChange {
            mailboxes: true,
            all: true,
            masked: true,
            rules: true,
            drafts: true,
            ..DataChange::default()
        }
    }
}

/// Sync engine state for the status line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SyncStatus {
    /// Not started yet; the UI shows cached data.
    #[default]
    Starting,
    /// A sync request is in flight.
    Syncing,
    /// Caught up with the server.
    Synced,
    /// No connection. Local changes wait in the outbox.
    Offline {
        /// Outbox entries not yet sent.
        pending: usize,
    },
    /// The last sync failed.
    Failed {
        /// What went wrong, in plain words.
        error: String,
    },
}

/// Push (EventSource) connection state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PushState {
    /// Not connected yet.
    #[default]
    Connecting,
    /// Connected and receiving state changes.
    Connected,
    /// Push is off; the engine polls instead.
    Off,
}

/// How a toast looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastLevel {
    /// Neutral information.
    #[default]
    Info,
    /// Something worked.
    Success,
    /// Worth a look.
    Warning,
    /// Something failed. The text must say so in words, not only colour.
    Error,
}

/// Result of an outbox entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxReport {
    /// What the entry did, for example "Archive 2 conversations".
    pub label: String,
    /// How it ended.
    pub outcome: OutboxOutcome,
}

/// How an outbox entry ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboxOutcome {
    /// The server accepted it.
    Done,
    /// The server rejected it and the local change was rolled back.
    Failed {
        /// What went wrong, in plain words.
        error: String,
    },
}

/// Timing of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSample {
    /// From reading the input event to `Backend::flush` returning.
    pub input_to_flush: Duration,
}

/// A request to the outside world.
///
/// Each variant is a plain value the shell can execute without calling
/// back into the UI. The crate docs list each one's contract and the
/// [`Msg`] that answers it.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// Fetch a message body the reader needs now.
    FetchBody(EmailId),
    /// Fetch, and ideally pre-render, bodies the user is likely to open
    /// next, nearest first. Low priority, may be dropped.
    Prefetch(Vec<EmailId>),
    /// Render a body off the UI thread and answer with
    /// [`Msg::BodyRendered`]. Use [`render_body`].
    RenderBody(RenderRequest),
    /// Apply a mutation locally now, then queue it for the server.
    Mutate(Mutation),
    /// Open a URL with the system handler.
    OpenUrl(String),
    /// Put text on the clipboard (OSC 52, or a local fallback).
    CopyToClipboard(String),
    /// Open a sanitized copy of the message in the browser, with remote
    /// content still blocked.
    OpenInBrowser(EmailId),
    /// Open an attachment with the system handler.
    OpenAttachment {
        /// The message.
        email_id: EmailId,
        /// The attachment blob.
        blob_id: BlobId,
        /// Its file name.
        name: String,
    },
    /// Sync now instead of waiting for push or the poll timer.
    RefreshNow,
    /// Persist pane sizes to the state file.
    SaveLayout(PaneSizes),
    /// Send [`Msg::Tick`] at or after this time, for toast expiry. The UI
    /// has no timers of its own.
    WakeAt(Timestamp),
    /// Leave the application.
    Quit,
    /// Stop the process as Ctrl+Z does in a shell, with the terminal
    /// restored while it is stopped. Once the shell resumes it, enter the
    /// TUI again and redraw everything, feeding `Event::Resize` with the
    /// terminal's size then.
    Suspend,
    /// End the undo send window of these held messages now, then quit
    /// once each is sent, or after a few seconds at most. A send that fails
    /// in that time cancels the quit, so the user sees why.
    SendNowAndQuit(Vec<DraftId>),
    /// Save a draft locally (and later to the server's Drafts). Answer
    /// with [`Msg::DraftSaved`]. Sent after a pause in typing, on leaving
    /// compose and on Ctrl-s.
    SaveDraft(Box<OutgoingMessage>),
    /// Delete a draft, locally and on the server.
    DeleteDraft(DraftId),
    /// Queue a message for sending, held for the undo send window. Answer
    /// with [`Msg::Send`]: `Scheduled`, then `Sent` or `Failed`. The draft
    /// should be kept until the send is final.
    Send(Box<OutgoingMessage>),
    /// Stop a queued send. Answer with [`Msg::Send`]: `Cancelled` with the
    /// message, or `TooLate`.
    CancelSend(DraftId),
    /// Suspend the terminal and edit `text` in `$VISUAL` or `$EDITOR`.
    /// Answer with [`Msg::EditorDone`], then redraw everything.
    OpenEditor {
        /// The draft.
        draft_id: DraftId,
        /// The body to start from.
        text: String,
    },
    /// Read a local file and upload it as a blob. Answer with
    /// [`Msg::Attached`].
    AttachFile {
        /// The draft.
        draft_id: DraftId,
        /// The path as typed (the shell expands `~`).
        path: String,
    },
    /// Search on the server (JMAP Email/query with collapseThreads), cache
    /// what it finds, and answer with [`Msg::ServerSearch`].
    ServerSearch {
        /// The query in Fastmail search syntax.
        query: String,
        /// Most conversations wanted.
        limit: usize,
    },
    /// Save a search for the sidebar. Send `Msg::DataChanged` with
    /// `mailboxes: true` afterwards so the sidebar reloads.
    SaveSearch {
        /// Display name.
        name: String,
        /// The query.
        query: String,
    },
    /// Create a mailbox, optionally moving or labelling conversations into
    /// it in the same step (the store's temporary mailbox id lets the move
    /// queue before the create returns). Answer with `Msg::DataChanged`.
    CreateMailbox {
        /// Name of the new mailbox.
        name: String,
        /// Parent mailbox, `None` for top level.
        parent: Option<MailboxId>,
        /// Conversations to put in the new mailbox.
        then_add: Option<Target>,
        /// When true, `then_add` is a move (leave the current mailbox);
        /// when false it adds a label.
        then_move: bool,
    },
    /// Create a Masked Email address, enabled, so it does not expire.
    /// Answer with [`Msg::MaskedCreated`] and `Msg::DataChanged` with
    /// `masked: true`.
    MaskedCreate {
        /// The site it is for.
        for_domain: String,
        /// A description.
        description: String,
        /// Compose asked for it, to send from it.
        draft_id: Option<DraftId>,
    },
    /// Enable, disable or delete a Masked Email address. Apply locally,
    /// then send `Msg::DataChanged` with `masked: true`.
    MaskedSetState {
        /// Masked Email id.
        id: String,
        /// The new state.
        state: MaskedState,
    },
    /// Change a Masked Email address's description.
    MaskedSetDescription {
        /// Masked Email id.
        id: String,
        /// The new description.
        description: String,
    },
    /// Save one rule (insert or replace by id). The JSON is a serialised
    /// `mejl_sieve::Rule`. Send `Msg::DataChanged` with `rules: true`.
    SaveRule(RuleJson),
    /// Delete a rule by id.
    DeleteRule(String),
    /// Put the rules in this order (all ids, all or nothing).
    ReorderRules(Vec<String>),
    /// Record that the rule block was copied for pasting, for the "last
    /// copied" history.
    RecordRuleRender {
        /// The whole block.
        sieve: String,
        /// Its body hash.
        body_sha256: String,
    },
    /// The user pasted their server script back and it matched this copy.
    ConfirmRulePaste {
        /// From [`RuleRenderView::id`](crate::data::RuleRenderView).
        render_id: i64,
    },
    /// Change a setting. Answer with [`Msg::Config`].
    SaveSetting(Setting),
}

/// Parameters for [`Cmd::RenderBody`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderRequest {
    /// The message.
    pub email_id: EmailId,
    /// Target width in cells.
    pub width: usize,
    /// Render HTML even when a usable plain part exists.
    pub prefer_html: bool,
}

/// Pane sizes, as dragged by the user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneSizes {
    /// Sidebar width in cells, in the three pane layout.
    pub sidebar: u16,
    /// Share of the list in the space shared by list and reader, from 0.0
    /// to 1.0. `None` means the automatic split.
    pub list_ratio: Option<f32>,
}

impl Default for PaneSizes {
    fn default() -> Self {
        PaneSizes {
            sidebar: 22,
            list_ratio: None,
        }
    }
}

/// What a mutation acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetIds {
    /// Whole conversations.
    Threads(Vec<ThreadId>),
    /// Single messages, for example the focused message in the reader.
    Emails(Vec<EmailId>),
    /// Every conversation in a mailbox except some, after `* a`. The shell
    /// expands this in the store, so the UI never collects 100k ids.
    AllInMailbox {
        /// The mailbox.
        mailbox: MailboxId,
        /// Conversations deselected after selecting all.
        except: Vec<ThreadId>,
    },
}

/// The targets of a mutation and the mailbox it was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The ids.
    pub ids: TargetIds,
    /// The mailbox being viewed. Archive and move remove membership from
    /// this mailbox.
    pub from_mailbox: Option<MailboxId>,
}

/// A keyword the UI can set or clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    /// `$seen`: read.
    Seen,
    /// `$flagged`: pinned.
    Flagged,
    /// `$muted`: replies to the conversation skip the inbox.
    Muted,
}

/// A change to mail, applied optimistically by the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// Move to the Archive mailbox.
    Archive(Target),
    /// Move to Trash. Undoable, never confirmed.
    Trash(Target),
    /// Report as spam and move to the Junk mailbox.
    ReportSpam(Target),
    /// Move to a mailbox.
    MoveTo {
        /// What to move.
        target: Target,
        /// Where to.
        mailbox: MailboxId,
    },
    /// Set or clear a keyword.
    SetKeyword {
        /// What to change.
        target: Target,
        /// Which keyword.
        keyword: Keyword,
        /// `true` sets it, `false` clears it.
        value: bool,
    },
    /// Add a label (`member: true`) or remove one: change membership of
    /// one mailbox without leaving others.
    SetMailbox {
        /// What to change.
        target: Target,
        /// The mailbox used as a label.
        mailbox: MailboxId,
        /// Add or remove.
        member: bool,
    },
    /// Destroy permanently. Only offered in Trash and Spam, after a
    /// confirm, because it cannot be undone.
    DeletePermanently(Target),
    /// Undo the last undoable mutation.
    Undo,
    /// Redo the last undone mutation.
    Redo,
}

/// Turns a render request into a body, for the shell's worker thread.
///
/// The shell calls this on `spawn_blocking` for [`Cmd::RenderBody`] and for
/// prefetched bodies, then sends [`Msg::BodyRendered`].
pub fn render_body(body: &crate::BodyView, width: usize, prefer_html: bool) -> RenderedBody {
    let options = crate::body::render_options(width, prefer_html);
    mejl_render::render_auto(body.text.as_deref(), body.html.as_deref(), &options)
}
