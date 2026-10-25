//! mejl's terminal UI.
//!
//! This crate holds the UI's state, its update function, its views, the
//! keymap engine, mouse hit testing and themes. It does no I/O: it never
//! awaits, never touches the network and never writes files. The binary
//! (the "shell") owns the terminal, the tokio runtime and the store, and
//! talks to this crate through three types.
//!
//! # Architecture
//!
//! The UI follows the Elm architecture:
//!
//! ```text
//!   Event ──> update(&mut AppState, &dyn MailData, Event) ──> Vec<Cmd> ──> shell
//!                       │                                                   │
//!                       └─> render(&AppState, &dyn MailData, &mut Frame)    │
//!                                                                           │
//!   Msg  <──────────────────────── results from sync, store and workers <──┘
//! ```
//!
//! - [`Event`] is input: keys, mouse, paste, resize, focus, and [`Msg`]s
//!   from outside (sync state, push state, data changed, body fetched or
//!   rendered, toasts, errors, outbox results, frame timings, ticks).
//! - [`Cmd`] is everything the UI asks for: fetch or prefetch bodies,
//!   render a body off the UI thread, apply a [`Mutation`], open a URL,
//!   copy to the clipboard, sync now, save pane sizes, wake at a time,
//!   quit, and the requests of the screens below (drafts, sending, the
//!   external editor, attachments, server search, masked addresses, rules
//!   and settings).
//! - [`MailData`] is a synchronous, read-only view of the local store.
//!   Every method is a bounded, indexed read. The UI reads only the page of
//!   rows it is about to draw (in [`CHUNK_ROWS`] sized chunks, a few kept),
//!   so a 100k conversation mailbox costs the same as a small one.
//!
//! The shell's loop:
//!
//! 1. Block on one channel carrying input events and messages, then drain
//!    everything pending.
//! 2. Feed each item to [`update`]. Execute the returned [`Cmd`]s: local
//!    store work (mutations) right away, network work on the runtime.
//! 3. After running a mutation locally, feed `Msg::DataChanged` back into
//!    [`update`] before drawing (the optimistic contract, see [`event`]).
//! 4. Draw once with [`draw`], inside synchronized output, and measure the
//!    frame for `Msg::Perf`.
//!
//! # Rendering and hit testing
//!
//! [`render`] draws from state and returns the frame's [`HitMap`]: every
//! clickable element records its rectangle and a [`HitTarget`] while it is
//! drawn. [`draw`] stores the map in the state, and the next mouse event is
//! tested against it. Mouse targets turn into the same [`Action`]s keys
//! produce, so mouse and keyboard can never drift apart.
//!
//! # Keys
//!
//! [`Keymap`] holds per-[`Context`] binding tables built from a
//! [`Preset`] (Fastmail web shortcuts by default, or vim) with user
//! overrides from the `[keys]` TOML table ([`KeyOverrides`]). Prefixes such
//! as `g`, `.`, `*` never act on their own, so nothing waits on a timeout;
//! the which-key popup shows what can follow. [`Keymap::check`] reports
//! unreachable bindings, shadowed globals and keys that legacy terminal
//! encoding cannot deliver, for `mejl keys --check`.
//!
//! # Layout and themes
//!
//! [`layout::compute`] is a pure function of the terminal size and state:
//! three panes from 160 columns, two from 100, one below. [`Theme`] maps
//! semantic [`StyleName`]s to styles; the default `terminal` theme uses the
//! ANSI palette so the user's terminal colours apply.
//!
//! # Screens
//!
//! [`Screen`] is the mail screen (with search results shown as a list),
//! compose, search, Masked Email, rules and settings. Modal layers are
//! [`Overlay`]s: help, confirms, one-line prompts, and pickers (the command
//! palette, move and label pickers, the From picker, links, copy, actions
//! and Masked Email menus). The top overlay gets keys first. A focused text
//! field (compose fields and body, the search bar, filters, prompts) owns
//! every key except the ones its keymap context binds, so typing never
//! triggers a command.
//!
//! # The shell's side of each command
//!
//! Every [`Cmd`] is answered asynchronously, never by calling back into
//! the UI. What each one needs:
//!
//! | Cmd | The shell does | It answers with |
//! |---|---|---|
//! | `SaveDraft(m)` | Saves the draft locally (atomically), later to the server's Drafts | [`Msg::DraftSaved`] |
//! | `DeleteDraft(id)` | Deletes the local and server draft | `Msg::DataChanged { drafts: true }` |
//! | `Send(m)` | Queues an `EmailSubmission` held for [`UiConfig::undo_send`]; keeps the draft until final | [`Msg::Send`]: `Scheduled`, then `Sent` or `Failed` |
//! | `CancelSend(id)` | Cancels the held submission by draft id | [`Msg::Send`]: `Cancelled { message }` or `TooLate` |
//! | `OpenEditor { draft_id, text }` | Leaves the alternate screen, runs `$VISUAL`/`$EDITOR` on a temp file, comes back and redraws everything | [`Msg::EditorDone`] |
//! | `AttachFile { draft_id, path }` | Reads the file (expanding `~`), uploads a blob | [`Msg::Attached`] |
//! | `ServerSearch { query, limit }` | `Email/query` with `collapseThreads`, caches the rows | [`Msg::ServerSearch`] |
//! | `SaveSearch { name, query }` | Stores a saved search | `Msg::DataChanged { mailboxes: true }` |
//! | `CreateMailbox { .. }` | Creates the mailbox locally (temporary id) and queues `then_add` as a move or label into it | `Msg::DataChanged` |
//! | `MaskedCreate { .. }` | `MaskedEmail/set` create, state enabled | [`Msg::MaskedCreated`] and `DataChanged { masked: true }` |
//! | `MaskedSetState { id, state }` | Updates locally and queues the change; undoable with `Mutation::Undo` | `DataChanged { masked: true }` |
//! | `MaskedSetDescription { .. }` | Same, for the description | `DataChanged { masked: true }` |
//! | `SaveRule`, `DeleteRule`, `ReorderRules` | Writes `state.db` (rules are local only) | `DataChanged { rules: true }` |
//! | `RecordRuleRender { sieve, body_sha256 }` | Records the copy for the paste check | `DataChanged { rules: true }` |
//! | `ConfirmRulePaste { render_id }` | Marks that copy as confirmed on Fastmail | `DataChanged { rules: true }` |
//! | `SaveSetting(s)` | Writes the config file, rebuilds the whole [`UiConfig`] (theme, keymap with overrides) | [`Msg::Config`] |
//! | `Suspend` | Restores the terminal, stops the process (SIGTSTP), and after `fg` enters the TUI again and redraws everything | `Event::Resize` with the size then |
//! | `SendNowAndQuit(ids)` | Ends the undo send window of those sends, quits once they are sent or after a few seconds; a failed send cancels the quit | [`Msg::Send`] |
//!
//! Quitting with `q` while a sent message is still in its undo send window
//! asks first, in a picker: send now (the default, so Enter picks it), keep
//! it queued for the next start, cancel the send, or stay. Esc stays.
//! `QuitNow` never asks.
//!
//! `Mutation::SetMailbox` adds or removes one mailbox (a label) and
//! [`Keyword::Muted`] is `$muted`; both follow the optimistic contract in
//! [`event`]. The UI asks for wake-ups with [`Cmd::WakeAt`] for toasts,
//! the undo send countdown, draft autosave and a delayed mark-read;
//! [`AppState::next_wakeup`] gives the earliest one.
//!
//! # The shell's side of each read
//!
//! Besides the reads the mail screen needs, [`MailData`] has:
//!
//! - `search_local(query, limit)`: the store's FTS search, one hit per
//!   conversation with highlighted subject and snippet (byte ranges into
//!   the raw text) and the terms it could not run. Runs per keystroke.
//! - `drafts()`: resumable drafts, newest first, with the server email id
//!   once uploaded, so opening a row in Drafts resumes it.
//! - `rule_renders(limit)`: copies of the rule block, newest first.
//! - `dry_run_messages(limit)`: the newest cached messages with header
//!   facts for `mejl_sieve`'s matcher, bounded by `limit`.
//! - `masked_emails()` now carries cached message counts per address.
//! - `MessageSummary` carries Reply-To, Message-ID and References and
//!   `IdentityView` a text signature, for replies.
//!
//! # Testing
//!
//! [`MemoryData`] is an in-memory [`MailData`] with the fictional mailbox
//! from the mock-ups ([`MemoryData::demo`]), a generator for very large
//! mailboxes ([`MemoryData::synthetic`]) and read counters. A
//! [`ManualClock`] fixes "now", so relative dates and snapshots are
//! stable.

#![warn(missing_docs)]

pub mod action;
pub mod address;
mod body;
pub mod clock;
pub mod compose;
pub mod data;
pub mod dates;
pub mod event;
pub mod fuzzy;
pub mod hit;
pub mod input;
pub mod keymap;
pub mod layout;
pub mod masked;
pub mod perf;
pub mod picker;
mod reader;
pub mod rules;
pub mod search;
pub mod settings;
pub mod state;
pub mod text;
pub mod theme;
mod update;
mod view;

pub use action::{Action, Category};
pub use clock::{Clock, ManualClock, SystemClock};
pub use data::{
    AccountView, AttachmentView, BlobId, BodyView, ComposeKind, ContactView, ConversationRow,
    ConversationView, DraftId, DraftView, DryRunMessage, EmailId, Highlight, IdentityView,
    MailData, MailboxId, MailboxRole, MailboxView, MaskedEmailView, MaskedState, MemoryData,
    MessageSummary, NewEmail, OutgoingAttachment, OutgoingMessage, Page, Participant, ReadStats,
    RuleJson, RuleRenderView, SavedSearchView, SearchHit, SearchIssue, SearchIssueKind,
    SearchResults, ThreadId,
};

pub use event::{
    Cmd, DataChange, Event, FrameSample, Keyword, Msg, Mutation, OutboxOutcome, OutboxReport,
    PaneSizes, PushState, RenderRequest, SendOutcome, SendReport, Setting, SyncStatus, Target,
    TargetIds, ToastLevel, render_body,
};
pub use hit::{HitMap, HitTarget, Pane};
pub use keymap::{Context, Key, KeyOverrides, KeySeq, Keymap, KeymapError, Preset};
pub use layout::{LayoutMode, LayoutPref};
pub use state::{AppState, Density, DraftSync, Focus, Overlay, Screen, UiConfig};
pub use state::{Confirm, ConfirmAction, Prompt, PromptAction};
pub use theme::{StyleName, Theme};
pub use update::update;
pub use view::{RenderOutput, draw, render};

/// Rows per list chunk read from [`MailData::conversation_page`].
pub const CHUNK_ROWS: usize = state::CHUNK;
