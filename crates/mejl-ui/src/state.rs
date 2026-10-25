//! Application state.
//!
//! [`AppState`] is everything the UI remembers between frames. It holds no
//! mail of its own beyond small caches: the visible list chunks, the open
//! conversation and rendered bodies. Mail truth stays behind
//! [`MailData`].
//!
//! [`Screen`] says which screen is shown. Each screen other than mail keeps
//! its own state in an `Option` field of [`AppState`] (compose, search,
//! Masked Email, rules, settings), so leaving a screen and coming back can
//! keep what was typed. Modal pieces (help, confirms, prompts, the palette
//! and every picker) are [`Overlay`]s; the top one gets the keys.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::action::Action;
use crate::body::BodyCache;
use crate::clock::Clock;
use crate::compose::ComposeState;
use crate::data::{
    AccountView, ConversationRow, ConversationView, EmailId, MailData, MailboxId, MailboxRole,
    MailboxView, Page, SavedSearchView, ThreadId,
};
use crate::data::{DraftId, OutgoingMessage};
use crate::event::{PaneSizes, PushState, SyncStatus, Target, ToastLevel};
use crate::hit::HitMap;
use crate::input::LineInput;
use crate::keymap::{Context, Keymap, Pending};
use crate::layout::{LayoutInput, LayoutPref, MailLayout, Splitter, compute};
use crate::masked::MaskedScreen;
use crate::perf::PerfStats;
use crate::picker::Picker;
use crate::reader::ReaderDoc;
use crate::rules::RulesScreen;
use crate::search::{ResultsList, SearchState};
use crate::settings::SettingsScreen;
use crate::theme::Theme;

/// UI settings. The shell builds them from config, and sends a new copy
/// with `Msg::Config` when settings change.
#[derive(Clone, Debug, PartialEq)]
pub struct UiConfig {
    /// Key bindings.
    pub keymap: Keymap,
    /// Styles and glyphs.
    pub theme: Theme,
    /// Pane arrangement.
    pub layout: LayoutPref,
    /// Show the hint bar.
    pub hints: bool,
    /// Accessible mode: the terminal cursor follows the focused row, for
    /// screen readers.
    pub accessible: bool,
    /// Time zone for dates.
    pub time_zone: TimeZone,
    /// Two clicks closer than this are a double click.
    pub double_click: Duration,
    /// Rows or lines per mouse wheel notch.
    pub wheel_lines: usize,
    /// Bodies to prefetch ahead of the cursor.
    pub prefetch: usize,
    /// How long toasts stay.
    pub toast_time: Duration,
    /// List row density.
    pub density: Density,
    /// Show the HTML part of messages that have both.
    pub prefer_html: bool,
    /// How long a sent message is held so it can be cancelled.
    pub undo_send: Duration,
    /// The user allows remote images in the browser view.
    pub remote_images: bool,
    /// How long a conversation is open before it is marked read. Zero
    /// marks it read on open.
    pub mark_read_delay: Duration,
    /// Archiving also marks the mail read. The shell applies it when it
    /// plans the archive; the UI only shows the value.
    pub mark_read_on_archive: bool,
}

/// How much room each list row takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// One line per row and no date headings.
    Compact,
    /// One line per row under date headings. The default.
    #[default]
    Comfortable,
    /// Two lines per row: sender and subject, then the preview.
    Relaxed,
}

impl Density {
    /// Every density, in settings order.
    pub const ALL: [Density; 3] = [Density::Compact, Density::Comfortable, Density::Relaxed];

    /// Config name.
    pub fn name(self) -> &'static str {
        match self {
            Density::Compact => "compact",
            Density::Comfortable => "comfortable",
            Density::Relaxed => "relaxed",
        }
    }

    /// Looks a density up by config name.
    pub fn from_name(name: &str) -> Option<Density> {
        Density::ALL.into_iter().find(|d| d.name() == name)
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            keymap: Keymap::default(),
            theme: Theme::default(),
            layout: LayoutPref::Auto,
            hints: true,
            accessible: false,
            time_zone: TimeZone::UTC,
            double_click: Duration::from_millis(400),
            wheel_lines: 3,
            prefetch: 3,
            toast_time: Duration::from_secs(5),
            density: Density::Comfortable,
            prefer_html: false,
            undo_send: Duration::from_secs(10),
            remote_images: false,
            mark_read_delay: Duration::ZERO,
            mark_read_on_archive: false,
        }
    }
}

/// The top-level screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Screen {
    /// Mailboxes, list and reader. Search results opened as a list also
    /// show here.
    #[default]
    Mail,
    /// Writing a message.
    Compose,
    /// The search bar with live local results.
    Search,
    /// The Masked Email manager.
    Masked,
    /// The rules (Sieve) manager.
    Rules,
    /// Settings.
    Settings,
}

/// Which pane has focus on the mail screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    /// The mailbox list.
    Sidebar,
    /// The conversation list.
    List,
    /// The reader.
    Reader,
}

/// A modal layer above the screen. The top of the stack gets the keys.
#[derive(Clone, Debug, PartialEq)]
pub enum Overlay {
    /// Bindings for the context help was opened from.
    Help(HelpState),
    /// Frame timings.
    Perf,
    /// A yes or no question in the status line.
    Confirm(Confirm),
    /// A fuzzy list: the command palette, the move and label pickers, the
    /// identity picker, links, the copy and actions menus.
    Picker(Box<Picker>),
    /// A one-line text question in the status line.
    Prompt(Prompt),
}

/// A pending text question.
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    /// The question, shown before the input.
    pub label: String,
    /// The answer so far.
    pub input: LineInput,
    /// What Enter does with the answer.
    pub on_enter: PromptAction,
}

/// What a prompt's answer is for.
#[derive(Clone, Debug, PartialEq)]
pub enum PromptAction {
    /// Attach the file at this path to the open draft.
    AttachPath,
    /// Save the current search under this name.
    SaveSearchName {
        /// The query to save.
        query: String,
    },
    /// The site for a new Masked Email address; asks for the description
    /// next.
    MaskedDomain,
    /// The description for a new Masked Email address.
    MaskedDescription {
        /// The site.
        for_domain: String,
        /// Compose asked for it.
        draft_id: Option<DraftId>,
    },
    /// A new description for a Masked Email address.
    MaskedRename {
        /// Masked Email id.
        id: String,
    },
    /// A rule field being edited.
    RuleField(crate::rules::FieldEdit),
    /// The user's pasted Sieve script, for the paste check.
    RulePaste,
}

/// State of the help overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpState {
    /// The context whose bindings are listed (plus global ones).
    pub context: Context,
    /// Typed filter text.
    pub filter: String,
    /// First visible row.
    pub scroll: usize,
}

/// A pending confirmation.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    /// The question.
    pub prompt: String,
    /// What `y` does.
    pub on_yes: ConfirmAction,
}

/// Actions that need a confirm because they cannot be undone, or because
/// something looks wrong.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmAction {
    /// Destroy messages.
    DeletePermanently(Target),
    /// Throw away the open draft.
    DiscardDraft,
    /// Send although a check failed (no subject, attachment mentioned but
    /// missing).
    SendAnyway,
    /// Disable a Masked Email address (mail to it then goes to Trash).
    DisableMasked {
        /// Masked Email id.
        id: String,
    },
    /// Delete a Masked Email address (mail to it then bounces).
    DeleteMasked {
        /// Masked Email id.
        id: String,
    },
    /// Delete a rule.
    DeleteRule {
        /// Rule id.
        id: String,
    },
}

/// A short notice in the status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// The text.
    pub text: String,
    /// How it looks.
    pub level: ToastLevel,
    /// Show "z undo" after the text.
    pub undo: bool,
    /// When it disappears.
    pub expires: Timestamp,
    /// While set, the toast is an undo send countdown for this draft:
    /// the text says how many seconds are left and the undo key cancels
    /// the send.
    pub sending: Option<DraftId>,
}

/// A message held in the undo send window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingSend {
    pub(crate) message: OutgoingMessage,
    pub(crate) deadline: Timestamp,
}

/// Selected conversations.
///
/// After `* a` the selection is "everything in the mailbox except these",
/// so selecting a 100k mailbox stores nothing per row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    all: bool,
    ids: BTreeSet<ThreadId>,
}

impl Selection {
    /// Is the conversation selected?
    pub fn contains(&self, thread: &ThreadId) -> bool {
        self.all != self.ids.contains(thread)
    }

    /// Selects or deselects one conversation.
    pub fn toggle(&mut self, thread: &ThreadId) {
        if !self.ids.remove(thread) {
            self.ids.insert(thread.clone());
        }
    }

    /// Selects one conversation.
    pub fn insert(&mut self, thread: &ThreadId) {
        if self.all {
            self.ids.remove(thread);
        } else {
            self.ids.insert(thread.clone());
        }
    }

    /// Selects everything.
    pub fn select_all(&mut self) {
        self.all = true;
        self.ids.clear();
    }

    /// Clears the selection.
    pub fn clear(&mut self) {
        self.all = false;
        self.ids.clear();
    }

    /// Number of selected conversations out of `total`.
    pub fn count(&self, total: usize) -> usize {
        if self.all {
            total.saturating_sub(self.ids.len())
        } else {
            self.ids.len()
        }
    }

    /// True when "everything except" mode is on.
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// The explicit ids: selected ones, or the exceptions in "all" mode.
    pub fn ids(&self) -> impl Iterator<Item = &ThreadId> {
        self.ids.iter()
    }

    /// True when nothing is selected, given the list length.
    pub fn is_empty(&self, total: usize) -> bool {
        self.count(total) == 0
    }

    pub(crate) fn retain(&mut self, keep: impl Fn(&ThreadId) -> bool) {
        self.ids.retain(|t| keep(t));
    }
}

/// Rows per cached list chunk.
pub(crate) const CHUNK: usize = 64;
/// Chunks kept.
const MAX_CHUNKS: usize = 4;

/// A few aligned chunks of the list around the viewport.
#[derive(Clone, Debug, Default)]
pub(crate) struct PageCache {
    mailbox: Option<MailboxId>,
    chunks: Vec<Page<ConversationRow>>,
    total: Option<usize>,
    /// Rows that do not come from a mailbox, such as search results.
    fixed: Option<Arc<Vec<ConversationRow>>>,
}

impl PageCache {
    pub(crate) fn invalidate(&mut self) {
        self.chunks.clear();
        self.total = None;
    }

    /// Serves these rows instead of a mailbox's pages.
    pub(crate) fn set_fixed(&mut self, rows: Arc<Vec<ConversationRow>>) {
        self.chunks.clear();
        self.total = Some(rows.len());
        self.fixed = Some(rows);
    }

    /// True when the rows are fixed (search results).
    pub(crate) fn is_fixed(&self) -> bool {
        self.fixed.is_some()
    }

    /// Position of a conversation among fixed rows.
    pub(crate) fn fixed_index(&self, thread: &ThreadId) -> Option<usize> {
        self.fixed
            .as_ref()?
            .iter()
            .position(|r| &r.thread_id == thread)
    }

    /// Loads the chunks covering `start..end` of `mailbox`.
    pub(crate) fn ensure(
        &mut self,
        data: &dyn MailData,
        mailbox: &MailboxId,
        start: usize,
        end: usize,
    ) {
        if let Some(rows) = &self.fixed {
            self.total = Some(rows.len());
            return;
        }
        if self.mailbox.as_ref() != Some(mailbox) {
            self.mailbox = Some(mailbox.clone());
            self.invalidate();
        }
        let end = end.max(start + 1);
        let mut chunk = start / CHUNK * CHUNK;
        while chunk < end {
            if let Some(total) = self.total
                && chunk >= total
                && chunk > 0
            {
                break;
            }
            if let Some(pos) = self.chunks.iter().position(|p| p.offset == chunk) {
                let page = self.chunks.remove(pos);
                self.chunks.push(page);
            } else {
                let page = data.conversation_page(mailbox, chunk, CHUNK);
                self.total = Some(page.total);
                self.chunks.push(page);
                if self.chunks.len() > MAX_CHUNKS {
                    self.chunks.remove(0);
                }
            }
            chunk += CHUNK;
        }
    }

    pub(crate) fn row(&self, index: usize) -> Option<&ConversationRow> {
        if let Some(rows) = &self.fixed {
            return rows.get(index);
        }
        let chunk = index / CHUNK * CHUNK;
        let page = self.chunks.iter().find(|p| p.offset == chunk)?;
        page.items.get(index - chunk)
    }

    pub(crate) fn total(&self) -> Option<usize> {
        self.total
    }
}

/// How the list keeps its viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Follow {
    /// Scroll so the cursor is visible.
    #[default]
    Cursor,
    /// Keep the scroll offset (after wheel scrolling) and move the cursor
    /// into view instead.
    Offset,
}

/// The conversation list.
#[derive(Clone, Debug, Default)]
pub struct ListState {
    pub(crate) cursor: usize,
    pub(crate) offset: usize,
    pub(crate) total: usize,
    pub(crate) cursor_thread: Option<ThreadId>,
    pub(crate) selection: Selection,
    pub(crate) anchor: Option<usize>,
    pub(crate) pages: PageCache,
    pub(crate) follow: Follow,
    pub(crate) relocate: bool,
    pub(crate) last_prefetch: Option<usize>,
    pub(crate) direction: i8,
}

impl ListState {
    /// Index of the cursor row.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Index of the first visible row.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Conversations in the mailbox.
    pub fn total(&self) -> usize {
        self.total
    }

    /// The selection.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The conversation under the cursor, if loaded.
    pub fn cursor_thread(&self) -> Option<&ThreadId> {
        self.cursor_thread.as_ref()
    }
}

/// The reader.
#[derive(Clone, Debug, Default)]
pub struct ReaderState {
    pub(crate) thread: Option<ThreadId>,
    pub(crate) conversation: Option<ConversationView>,
    pub(crate) expanded: HashSet<EmailId>,
    pub(crate) focused: usize,
    pub(crate) scroll: usize,
    pub(crate) quotes_shown: bool,
    pub(crate) fold_overrides: HashSet<(EmailId, usize)>,
    pub(crate) headers_full: bool,
    pub(crate) html_flip: HashSet<EmailId>,
    pub(crate) tree: bool,
    pub(crate) doc: Arc<ReaderDoc>,
    pub(crate) dirty: bool,
    pub(crate) scroll_to: Option<EmailId>,
}

impl ReaderState {
    /// The conversation shown.
    pub fn conversation(&self) -> Option<&ConversationView> {
        self.conversation.as_ref()
    }

    /// Is a message expanded?
    pub fn is_expanded(&self, email: &EmailId) -> bool {
        self.expanded.contains(email)
    }

    /// Index of the focused message.
    pub fn focused(&self) -> usize {
        self.focused
    }

    /// First visible line.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Are quoted blocks shown?
    pub fn quotes_shown(&self) -> bool {
        self.quotes_shown
    }

    /// Are full headers shown?
    pub fn headers_full(&self) -> bool {
        self.headers_full
    }

    /// Is the conversation shown as a thread tree?
    pub fn tree(&self) -> bool {
        self.tree
    }

    /// Plain text of every line of the current reader document, for tests
    /// and screen reader output.
    pub fn text_lines(&self) -> Vec<String> {
        self.doc.lines.iter().map(|l| l.text()).collect()
    }
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarItem {
    /// A mailbox.
    Mailbox {
        /// Its id.
        id: MailboxId,
        /// Display name.
        name: String,
        /// Tree depth.
        depth: u8,
        /// Count to show, if any.
        count: Option<u32>,
    },
    /// A group heading.
    Heading(&'static str),
    /// A spacer.
    Blank,
    /// A saved search.
    SavedSearch {
        /// Display name.
        name: String,
        /// The query it runs.
        query: String,
        /// Unread matches.
        unread: Option<u32>,
    },
    /// A tool entry with its key.
    Tool {
        /// Display name.
        name: &'static str,
        /// What it opens.
        action: Action,
    },
}

impl SidebarItem {
    /// Can the cursor rest on this row?
    pub fn selectable(&self) -> bool {
        !matches!(self, SidebarItem::Heading(_) | SidebarItem::Blank)
    }
}

/// Builds the sidebar rows from mailboxes and saved searches.
pub(crate) fn sidebar_items(boxes: &[MailboxView], saved: &[SavedSearchView]) -> Vec<SidebarItem> {
    let mut out = Vec::new();
    let count_for = |m: &MailboxView| -> Option<u32> {
        let n = if m.role == Some(MailboxRole::Drafts) {
            m.total_emails
        } else {
            m.unread_threads
        };
        (n > 0).then_some(n)
    };
    for role in MailboxRole::ORDER {
        if let Some(m) = boxes.iter().find(|m| m.role == Some(role)) {
            let name = if m.name.trim().is_empty() {
                role.default_name().to_owned()
            } else {
                m.name.clone()
            };
            out.push(SidebarItem::Mailbox {
                id: m.id.clone(),
                name,
                depth: 0,
                count: count_for(m),
            });
        }
    }
    let folders: Vec<&MailboxView> = boxes.iter().filter(|m| m.role.is_none()).collect();
    if !folders.is_empty() {
        out.push(SidebarItem::Blank);
        out.push(SidebarItem::Heading("Folders"));
        let mut tree = Vec::new();
        push_tree(&folders, None, 0, &mut tree);
        for m in tree {
            out.push(SidebarItem::Mailbox {
                id: m.0.id.clone(),
                name: m.0.name.clone(),
                depth: m.1,
                count: count_for(m.0),
            });
        }
    }
    out.push(SidebarItem::Blank);
    out.push(SidebarItem::Heading("Saved searches"));
    for s in saved {
        out.push(SidebarItem::SavedSearch {
            name: s.name.clone(),
            query: s.query.clone(),
            unread: s.unread.filter(|n| *n > 0),
        });
    }
    out.push(SidebarItem::Blank);
    out.push(SidebarItem::Heading("Tools"));
    out.push(SidebarItem::Tool {
        name: "Rules",
        action: Action::GoRules,
    });
    out.push(SidebarItem::Tool {
        name: "Masked Email",
        action: Action::GoMasked,
    });
    out.push(SidebarItem::Tool {
        name: "Settings",
        action: Action::GoSettings,
    });
    out
}

fn push_tree<'a>(
    all: &[&'a MailboxView],
    parent: Option<&MailboxId>,
    depth: u8,
    out: &mut Vec<(&'a MailboxView, u8)>,
) {
    if depth > 16 {
        return;
    }
    let mut kids: Vec<&'a MailboxView> = all
        .iter()
        .copied()
        .filter(|m| {
            m.parent.as_ref() == parent
                // A folder whose parent is a role mailbox or unknown shows at
                // the top level rather than disappearing.
                || (parent.is_none()
                    && m.parent
                        .as_ref()
                        .is_some_and(|p| !all.iter().any(|o| &o.id == p)))
        })
        .collect();
    kids.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    for k in kids {
        out.push((k, depth));
        push_tree(all, Some(&k.id), depth + 1, out);
    }
}

/// Where drafts are kept: on the server's Drafts mailbox, or on this device
/// for now. The compose status line says which.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftSync {
    /// Drafts whose newest version is not on the server yet.
    pub pending: usize,
    /// The account has no Drafts mailbox, so drafts stay on this device.
    pub local_only: bool,
    /// Whether the server answers, so pending drafts are on their way.
    pub online: bool,
    /// Why the server refused a draft, while one is pending.
    pub error: Option<String>,
}

/// Everything the UI remembers.
#[derive(Debug)]
pub struct AppState {
    pub(crate) config: UiConfig,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) size: (u16, u16),
    pub(crate) screen: Screen,
    pub(crate) focus: Focus,
    pub(crate) reader_full: bool,
    pub(crate) sidebar_overlay: bool,
    pub(crate) account: Option<AccountView>,
    pub(crate) mailbox: Option<MailboxId>,
    pub(crate) mailboxes: Vec<MailboxView>,
    pub(crate) mailboxes_dirty: bool,
    pub(crate) sidebar: Vec<SidebarItem>,
    pub(crate) sidebar_cursor: usize,
    pub(crate) identities: Vec<String>,
    pub(crate) list: ListState,
    pub(crate) reader: ReaderState,
    pub(crate) bodies: BodyCache,
    pub(crate) overlays: Vec<Overlay>,
    pub(crate) pending: Pending,
    pub(crate) toast: Option<Toast>,
    pub(crate) sync: SyncStatus,
    pub(crate) draft_sync: DraftSync,
    pub(crate) push: PushState,
    pub(crate) perf: PerfStats,
    pub(crate) panes: PaneSizes,
    pub(crate) drag: Option<Splitter>,
    pub(crate) last_click: Option<(Timestamp, u16, u16)>,
    pub(crate) hits: HitMap,
    pub(crate) last_action: Option<(Action, Option<u32>)>,
    pub(crate) terminal_focused: bool,
    pub(crate) compose: Option<Box<ComposeState>>,
    pub(crate) search: Option<Box<SearchState>>,
    pub(crate) results: Option<Box<ResultsList>>,
    pub(crate) masked: Option<Box<MaskedScreen>>,
    pub(crate) rules: Option<Box<RulesScreen>>,
    pub(crate) settings: Option<SettingsScreen>,
    pub(crate) sends: Vec<PendingSend>,
    pub(crate) recent: Vec<Action>,
    pub(crate) draft_counter: u64,
    pub(crate) mark_read_at: Option<(ThreadId, Timestamp)>,
    pub(crate) announce: bool,
}

impl AppState {
    /// A fresh state for a terminal of `size` (columns, rows). Call
    /// [`update`](crate::update) with any event, or [`AppState::start`],
    /// before the first draw so the first page is loaded.
    pub fn new(config: UiConfig, clock: Arc<dyn Clock>, size: (u16, u16)) -> AppState {
        AppState {
            config,
            clock,
            size,
            screen: Screen::Mail,
            focus: Focus::List,
            reader_full: false,
            sidebar_overlay: false,
            account: None,
            mailbox: None,
            mailboxes: Vec::new(),
            mailboxes_dirty: true,
            sidebar: Vec::new(),
            sidebar_cursor: 0,
            identities: Vec::new(),
            list: ListState::default(),
            reader: ReaderState {
                dirty: true,
                ..ReaderState::default()
            },
            bodies: BodyCache::default(),
            overlays: Vec::new(),
            pending: Pending::default(),
            toast: None,
            sync: SyncStatus::default(),
            draft_sync: DraftSync::default(),
            push: PushState::default(),
            perf: PerfStats::default(),
            panes: PaneSizes::default(),
            drag: None,
            last_click: None,
            hits: HitMap::default(),
            last_action: None,
            terminal_focused: true,
            compose: None,
            search: None,
            results: None,
            masked: None,
            rules: None,
            settings: None,
            sends: Vec::new(),
            recent: Vec::new(),
            draft_counter: 0,
            mark_read_at: None,
            announce: false,
        }
    }

    /// Loads the first page and returns the first commands (body fetches
    /// and prefetches). Same as an update with no event.
    pub fn start(&mut self, data: &dyn MailData) -> Vec<crate::Cmd> {
        let mut cmds = Vec::new();
        self.refresh(data, &mut cmds);
        cmds
    }

    /// Restores pane sizes saved by an earlier session.
    pub fn set_pane_sizes(&mut self, panes: PaneSizes) {
        // The sizes come from a state file, so take nothing on trust.
        self.panes = PaneSizes {
            sidebar: panes
                .sidebar
                .clamp(crate::layout::SIDEBAR_MIN, crate::layout::SIDEBAR_MAX),
            list_ratio: panes
                .list_ratio
                .filter(|r| r.is_finite())
                .map(|r| r.clamp(0.0, 1.0)),
        };
        self.reader.dirty = true;
    }

    /// The current screen.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The focused pane.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// True when the reader fills the screen.
    pub fn reader_full(&self) -> bool {
        self.reader_full
    }

    /// True when the sidebar overlay is open.
    pub fn sidebar_overlay(&self) -> bool {
        self.sidebar_overlay
    }

    /// The mailbox being viewed.
    pub fn mailbox(&self) -> Option<&MailboxId> {
        self.mailbox.as_ref()
    }

    /// The list.
    pub fn list(&self) -> &ListState {
        &self.list
    }

    /// The reader.
    pub fn reader(&self) -> &ReaderState {
        &self.reader
    }

    /// The overlay stack, bottom first.
    pub fn overlays(&self) -> &[Overlay] {
        &self.overlays
    }

    /// The current toast, if it has not expired.
    pub fn toast(&self) -> Option<&Toast> {
        let now = self.clock.now();
        self.toast.as_ref().filter(|t| t.expires > now)
    }

    /// Keys of a pending prefix.
    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    /// Frame timing statistics.
    pub fn perf(&self) -> &PerfStats {
        &self.perf
    }

    /// Current pane sizes.
    pub fn pane_sizes(&self) -> PaneSizes {
        self.panes
    }

    /// The sidebar rows.
    pub fn sidebar_items(&self) -> &[SidebarItem] {
        &self.sidebar
    }

    /// Index of the sidebar cursor.
    pub fn sidebar_cursor(&self) -> usize {
        self.sidebar_cursor
    }

    /// The UI settings.
    pub fn config(&self) -> &UiConfig {
        &self.config
    }

    /// The hit map of the last drawn frame.
    pub fn hit_map(&self) -> &HitMap {
        &self.hits
    }

    /// Terminal size.
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Sync state.
    pub fn sync_status(&self) -> &SyncStatus {
        &self.sync
    }

    /// Where drafts are kept, for the compose status line.
    pub fn draft_sync(&self) -> &DraftSync {
        &self.draft_sync
    }

    /// Sets where drafts are kept. The shell calls this when the engine's
    /// status or its own pending server copies change.
    pub fn set_draft_sync(&mut self, drafts: DraftSync) {
        self.draft_sync = drafts;
    }

    /// When the shell should next wake the UI with `Msg::Tick`, if ever:
    /// the earliest of toast expiry, the next second of an undo send
    /// countdown, a draft autosave and a delayed mark-read. The status
    /// clock needs its own tick each second while
    /// [`AppState::shows_clock`] is true.
    pub fn next_wakeup(&self) -> Option<Timestamp> {
        let now = self.clock.now();
        let mut times: Vec<Timestamp> = Vec::new();
        if let Some(t) = self.toast() {
            times.push(t.expires);
            if t.sending.is_some() {
                let next = now
                    .checked_add(jiff::SignedDuration::from_secs(1))
                    .unwrap_or(t.expires);
                times.push(next.min(t.expires));
            }
        }
        if let Some(c) = &self.compose
            && let Some(at) = c.autosave_at()
        {
            times.push(at);
        }
        if let Some((_, at)) = &self.mark_read_at {
            times.push(*at);
        }
        if let Some((_, at)) = self.countdown() {
            let next = now
                .checked_add(jiff::SignedDuration::from_secs(1))
                .unwrap_or(at);
            times.push(next.min(at));
        }
        times.into_iter().min()
    }

    /// The undo send countdown the status line shows, if any: the draft
    /// and when it goes out. It shows while its own toast is up, or while
    /// no other notice is, so a toast about something else never leaves
    /// the undo key cancelling a send the user cannot see.
    pub(crate) fn countdown(&self) -> Option<(DraftId, Timestamp)> {
        let now = self.clock.now();
        if let Some(t) = self.toast() {
            return t.sending.clone().map(|id| (id, t.expires));
        }
        if matches!(
            self.overlays.last(),
            Some(Overlay::Confirm(_) | Overlay::Prompt(_))
        ) {
            return None;
        }
        self.sends
            .iter()
            .filter(|s| s.deadline > now)
            .max_by_key(|s| s.deadline)
            .map(|s| (s.message.draft_id.clone(), s.deadline))
    }

    /// The open compose screen's draft id, if compose is open.
    pub fn draft_id(&self) -> Option<&DraftId> {
        self.compose.as_ref().map(|c| &c.draft_id)
    }

    /// The focused compose field, if compose is open.
    pub fn compose_field(&self) -> Option<crate::compose::Field> {
        self.compose.as_ref().map(|c| c.field)
    }

    /// Plain text of the compose body, if compose is open.
    pub fn compose_body(&self) -> Option<String> {
        self.compose.as_ref().map(|c| c.body_text())
    }

    /// The current search query, if the search screen or a results list
    /// is open.
    pub fn search_query(&self) -> Option<&str> {
        if let Some(s) = &self.search {
            return Some(s.input.text());
        }
        self.results.as_ref().map(|r| r.query.as_str())
    }

    /// True when the screen changes every second: the status clock (mail
    /// screen, two and three pane widths, no pending prefix) or a draft's
    /// "saved N s ago". The shell then sends `Msg::Tick` once a second;
    /// otherwise an idle UI needs no wake-ups at all.
    pub fn shows_clock(&self) -> bool {
        self.clock_visible()
            || self
                .compose
                .as_ref()
                .is_some_and(|c| self.screen == Screen::Compose && c.saved_at.is_some())
    }

    /// True when the status line draws the clock: the mail screen at two
    /// pane widths, with no prefix pending and no picker open.
    pub(crate) fn clock_visible(&self) -> bool {
        self.size.0 >= crate::layout::TWO_PANE_MIN
            && self.pending.is_empty()
            && self.screen == Screen::Mail
            && !matches!(self.overlays.last(), Some(Overlay::Picker(_)))
    }

    /// The mail screen layout for the current size and state.
    pub fn layout(&self) -> MailLayout {
        compute(&LayoutInput {
            area: ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1),
            pref: self.config.layout,
            hints: self.config.hints,
            reader_full: self.reader_full,
            sidebar_overlay: self.sidebar_overlay,
            panes: self.panes,
        })
    }

    /// The keymap contexts that receive keys now, most specific first.
    pub fn contexts(&self) -> Vec<Context> {
        if let Some(Overlay::Help(_)) = self.overlays.last() {
            return vec![Context::Help];
        }
        match self.screen {
            Screen::Mail => {}
            Screen::Compose => {
                return if self.compose.as_ref().is_some_and(|c| c.editing) {
                    vec![Context::Compose]
                } else {
                    vec![Context::Compose, Context::Global]
                };
            }
            Screen::Search => return vec![Context::Search],
            Screen::Masked => return vec![Context::Masked, Context::Global],
            Screen::Rules => return vec![Context::Rules, Context::Global],
            Screen::Settings => return vec![Context::Settings, Context::Global],
        }
        let pane = match self.focus {
            Focus::Sidebar => Context::Sidebar,
            Focus::List => Context::List,
            Focus::Reader => Context::Reader,
        };
        vec![pane, Context::Global]
    }

    pub(crate) fn mailbox_view(&self) -> Option<&MailboxView> {
        let id = self.mailbox.as_ref()?;
        self.mailboxes.iter().find(|m| &m.id == id)
    }

    pub(crate) fn mailbox_role(&self) -> Option<MailboxRole> {
        self.mailbox_view().and_then(|m| m.role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_all_except() {
        let mut s = Selection::default();
        let a = ThreadId::from("a");
        let b = ThreadId::from("b");
        assert!(s.is_empty(10));
        s.toggle(&a);
        assert!(s.contains(&a) && !s.contains(&b));
        assert_eq!(s.count(10), 1);
        s.select_all();
        assert!(s.contains(&a) && s.contains(&b));
        s.toggle(&b);
        assert!(!s.contains(&b));
        assert_eq!(s.count(10), 9);
        s.insert(&b);
        assert_eq!(s.count(10), 10);
        s.clear();
        assert!(s.is_empty(10));
    }

    #[test]
    fn sidebar_tree_and_roles() {
        let mb = |id: &str, parent: Option<&str>, name: &str, role, order| MailboxView {
            id: id.into(),
            parent: parent.map(MailboxId::from),
            name: name.into(),
            role,
            depth: 0,
            sort_order: order,
            total_threads: 0,
            unread_threads: if id == "c" { 2 } else { 0 },
            total_emails: 1,
            unread_emails: 0,
        };
        let boxes = vec![
            mb("c", Some("p"), "Child", None, 1),
            mb("t", None, "Trash", Some(MailboxRole::Trash), 0),
            mb("p", None, "Parent", None, 2),
            mb("i", None, "", Some(MailboxRole::Inbox), 0),
            mb("d", None, "Drafts", Some(MailboxRole::Drafts), 0),
            mb("o", Some("missing"), "Orphan", None, 0),
        ];
        let items = sidebar_items(&boxes, &[]);
        let names: Vec<String> = items
            .iter()
            .map(|i| match i {
                SidebarItem::Mailbox {
                    name, depth, count, ..
                } => {
                    format!(
                        "{}{name}{}",
                        "  ".repeat(*depth as usize),
                        count.map(|c| format!(" {c}")).unwrap_or_default()
                    )
                }
                SidebarItem::Heading(h) => format!("#{h}"),
                SidebarItem::Blank => String::new(),
                SidebarItem::SavedSearch { name, .. } => format!("?{name}"),
                SidebarItem::Tool { name, .. } => format!("!{name}"),
            })
            .collect();
        assert_eq!(
            names,
            [
                "Inbox",
                "Drafts 1",
                "Trash",
                "",
                "#Folders",
                "Orphan",
                "Parent",
                "  Child 2",
                "",
                "#Saved searches",
                "",
                "#Tools",
                "!Rules",
                "!Masked Email",
                "!Settings"
            ]
        );
    }
}
