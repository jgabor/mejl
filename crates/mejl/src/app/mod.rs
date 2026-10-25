//! The interactive app: the UI thread's loop and everything it wires
//! together.
//!
//! # The loop
//!
//! The UI thread owns an [`App`] and blocks on one channel
//! ([`crate::events`]) that carries every input:
//!
//! - terminal events from the input thread, stamped with the time they
//!   were read;
//! - [`SyncEvent`]s from the engine's sink, wrapped in [`AppEvent::Sync`];
//! - finished body renders from the render workers;
//! - clock ticks and wake-ups from the timer, and signals.
//!
//! [`App::handle_batch`] is one turn: it takes everything that was waiting,
//! folds runs of mouse motion and resizes, feeds each item to
//! [`mejl_ui::update`], runs the [`Cmd`]s that come back, then draws once
//! with [`mejl_ui::draw`], inside synchronized output when the terminal
//! supports it. The time from the oldest input of the batch to the end of
//! the draw goes to the UI's perf overlay at the start of the next turn.
//!
//! The UI thread never awaits and never waits on the network. Store writes
//! go to the engine's store thread and network work to its runtime, both
//! through [`SyncHandle::send`], which never blocks. Attachment downloads
//! run on a small runtime of the app's own. The only blocking calls are
//! the external editor and a Ctrl+Z suspend, which own the terminal while
//! they last, and the engine shutdown when quitting.
//!
//! # Same-frame feedback
//!
//! See [`overlay`]: mutations are planned against the Reader on this
//! thread, shown at once through a short-lived overlay keyed by the
//! command's token, and the overlay is dropped when the engine answers
//! with `Applied` or `Rejected` for that token. Undo and redo work the
//! same way, from a journal of this run's undoable actions, and their
//! entry goes with `Undone` or `Redone`.
//!
//! # Drafts
//!
//! Every draft save goes to the store first, then a copy goes to the
//! engine for the server's Drafts mailbox. A save the user asked for (a
//! key or a click), and leaving compose, copy at once. Autosaves while
//! compose stays open copy at most once every ten seconds: the newest one
//! waits, and goes up when that time is over, when the terminal loses
//! focus or when mejl quits. The compose status line counts waiting copies
//! as not on the server yet.
//!
//! # Quitting and suspending
//!
//! `q` while a sent message is in its undo send window asks first (the UI
//! shows the question). "Send now" ends the hold and keeps the loop
//! running until the engine reports the send, for at most a few seconds,
//! then quits; a failed send cancels the quit. Ctrl+Z hands the terminal
//! back to the shell through [`Host::suspend`], then clears and redraws
//! everything at the size the terminal has after `fg`.

pub mod convert;
pub mod data;
pub mod host;
pub mod mutate;
pub mod overlay;
pub mod settings;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use mejl_jmap::{Client, Id, Patch, capability};
use mejl_store::{EmailPatchTarget, Mutation, RuleRecord, Seq, UndoOutcome};
use mejl_sync::{Command, LocalOutcome, LocalWrite, SearchRequest, SyncEvent, SyncHandle};
use mejl_ui::{
    self as ui, AppState, Cmd, DataChange, DraftId, Event, FrameSample, MailData, Msg,
    OutboxOutcome, OutboxReport, SendOutcome, SendReport, ToastLevel, UiConfig,
};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::events::{LoopEvent, LoopReceiver, LoopSender, TimedEvent};
use crate::render_pool::{self, RenderDone, RenderPool, RenderRequest};
use crate::timer::Timer;
use data::{PANE_SIZES_KEY, SAVED_SEARCHES_KEY, StoreData};
use host::Host;
use mutate::Plan;
use overlay::{Journal, MaskedOverlay, ThreadSnapshot};
use settings::Settings;

/// What the app's channel carries besides terminal input.
#[derive(Debug)]
pub enum AppEvent {
    /// An event from the sync engine.
    Sync(SyncEvent),
    /// An attachment finished downloading, or failed.
    Downloaded {
        /// The file name shown to the user.
        name: String,
        /// The saved file, or what went wrong.
        result: Result<PathBuf, String>,
    },
}

/// The sending side of the app's channel.
pub type AppSender = LoopSender<AppEvent>;

/// What the app needs to start.
pub struct AppParts<B: Backend> {
    /// The terminal to draw on.
    pub terminal: Terminal<B>,
    /// Side effects outside the UI.
    pub host: Box<dyn Host>,
    /// The running engine.
    pub handle: SyncHandle,
    /// The JMAP client the engine uses, for attachment downloads.
    pub client: Client,
    /// The channel the engine, the timer and the workers send to.
    pub tx: AppSender,
    /// Config and where setting changes are saved.
    pub settings: Settings,
    /// The UI's settings, built from `settings`.
    pub ui: UiConfig,
    /// The account name, shown until the session is cached.
    pub account: String,
    /// The UI's clock.
    pub clock: Arc<dyn ui::Clock>,
    /// Render worker threads.
    pub render_threads: usize,
}

/// Why the app is waiting on a token.
#[derive(Debug)]
enum Waiting {
    Mutate {
        label: String,
    },
    CreateMailbox {
        name: String,
        then_add: Option<ui::Target>,
        then_move: bool,
    },
    Undo,
    Redo,
    Send {
        message: Box<ui::OutgoingMessage>,
    },
    CancelSend {
        draft_id: DraftId,
    },
    Upload {
        draft_id: DraftId,
        path: String,
    },
    Search {
        query: String,
    },
    MaskedCreate {
        draft_id: Option<DraftId>,
    },
    MaskedChange {
        label: String,
    },
    SaveDraft {
        draft_id: DraftId,
    },
    Local {
        changes: DataChange,
        what: &'static str,
    },
}

/// How long "send now, then quit" waits for the sends before quitting
/// anyway. What is still queued then goes out on the next start.
const SEND_NOW_WAIT: Duration = Duration::from_secs(5);

/// A send the engine is holding.
#[derive(Debug)]
struct HeldSend {
    message: Box<ui::OutgoingMessage>,
}

/// While compose stays open, autosaves copy a draft to the server's Drafts
/// at most this often. A save the user asks for, leaving compose, the
/// terminal losing focus and quitting copy at once.
const DRAFT_COPY_EVERY: Duration = Duration::from_secs(10);

/// The newest local save of a draft whose server copy waits for
/// [`DRAFT_COPY_EVERY`].
#[derive(Debug)]
struct HeldCopy {
    message: Box<ui::OutgoingMessage>,
    due: Instant,
}

/// Whether an event is the user asking for something: a key, a paste or a
/// click. A draft save from one copies to the server at once. Mouse motion
/// and scrolling do not count.
fn asked_by_user(ev: &Event) -> bool {
    use crossterm::event::MouseEventKind;
    match ev {
        Event::Key(_) | Event::Paste(_) => true,
        Event::Mouse(m) => matches!(m.kind, MouseEventKind::Down(_) | MouseEventKind::Up(_)),
        _ => false,
    }
}

/// The interactive app.
pub struct App<B: Backend> {
    state: AppState,
    data: StoreData,
    terminal: Terminal<B>,
    host: Box<dyn Host>,
    handle: SyncHandle,
    client: Client,
    tx: AppSender,
    timer: Timer,
    pool: RenderPool,
    io: Option<tokio::runtime::Runtime>,
    settings: Settings,
    files: Option<tempfile::TempDir>,
    queue: VecDeque<Event>,
    next_token: u64,
    tokens: HashMap<u64, Waiting>,
    sends: HashMap<Seq, HeldSend>,
    send_seq: HashMap<DraftId, Seq>,
    cancel_wanted: HashSet<DraftId>,
    send_now_wanted: HashSet<DraftId>,
    quit_after_sends: Option<Instant>,
    masked_seqs: HashMap<Seq, Option<DraftId>>,
    draft_copied: HashMap<DraftId, Instant>,
    draft_held: HashMap<DraftId, HeldCopy>,
    input_turn: bool,
    journal: Journal,
    loading_more: HashSet<String>,
    prefetched: HashSet<String>,
    render_width: Option<usize>,
    engine_status: mejl_sync::SyncStatus,
    auth_error: Option<String>,
    shown_sync: Option<ui::SyncStatus>,
    shown_push: Option<ui::PushState>,
    perf: Option<FrameSample>,
    frames: u64,
    quit: bool,
    exit_note: Option<String>,
    read_error: Option<String>,
}

impl<B: Backend> std::fmt::Debug for App<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("frames", &self.frames)
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

/// The message for an access token the server refused.
pub fn auth_message(error: &str) -> String {
    format!(
        "Fastmail did not accept the API token ({error}). Quit mejl and run `mejl login` to paste a new one."
    )
}

/// Only these schemes are opened from mail. Others (`file:`, `javascript:`,
/// custom app schemes) could run things on the user's machine.
fn openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

/// A file name from mail that is safe to put in a directory: no path
/// parts, no control characters, no leading dots, and not too long.
pub fn safe_file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim().trim_start_matches(['.', ' ']);
    let short: String = trimmed.chars().take(120).collect();
    if short.is_empty() {
        "attachment".to_owned()
    } else {
        short
    }
}

/// Text from mail made safe for the clipboard: no control characters but
/// newlines and tabs, and no bidi controls or invisible characters. A
/// paste would otherwise carry escape sequences into another terminal, or
/// show an address or link in an order other than the one it has.
pub fn clipboard_text(text: &str) -> String {
    text.chars()
        .filter(|c| !(c.is_control() && !matches!(c, '\n' | '\t')) && !ui::text::is_dropped(*c))
        .collect()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A copy of a message for the browser. A Content-Security-Policy as the
/// first thing in the head blocks scripts, forms, frames and, unless the
/// user allows remote images, every remote load. Policies only add up, so
/// nothing in the mail can loosen it.
pub fn browser_page(subject: &str, body: &ui::BodyView, remote_images: bool) -> String {
    let images = if remote_images {
        "img-src * data: cid:"
    } else {
        "img-src data: cid:"
    };
    let csp = format!(
        "default-src 'none'; {images}; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'"
    );
    let content = match (&body.html, &body.text) {
        (Some(html), _) => mejl_render::sanitize_for_browser(html, remote_images),
        (None, Some(text)) => format!(
            "<pre style=\"white-space: pre-wrap\">{}</pre>",
            html_escape(text)
        ),
        (None, None) => String::new(),
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\"><meta name=\"referrer\" content=\"no-referrer\"><title>{}</title></head><body>{content}</body></html>",
        html_escape(subject)
    )
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn backend_error(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Splits a batch into runs of input, coalesced, and everything else, in
/// order.
fn coalesce_batch(batch: Vec<LoopEvent<AppEvent>>) -> Vec<LoopEvent<AppEvent>> {
    let mut out = Vec::with_capacity(batch.len());
    let mut run: Vec<TimedEvent> = Vec::new();
    let flush = |run: &mut Vec<TimedEvent>, out: &mut Vec<LoopEvent<AppEvent>>| {
        if !run.is_empty() {
            out.extend(
                crate::input::coalesce(std::mem::take(run))
                    .into_iter()
                    .map(LoopEvent::Input),
            );
        }
    };
    for e in batch {
        match e {
            LoopEvent::Input(t) => run.push(t),
            other => {
                flush(&mut run, &mut out);
                out.push(other);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

impl<B: Backend> App<B> {
    /// Builds the app: opens the UI thread's Reader, starts the timer, the
    /// render workers and the download runtime. Nothing is drawn yet; call
    /// [`App::start`].
    pub fn new(parts: AppParts<B>) -> std::io::Result<App<B>> {
        let reader = parts
            .handle
            .reader_factory()
            .open()
            .map_err(std::io::Error::other)?;
        let data = StoreData::new(reader, &parts.account, parts.ui.time_zone.clone());
        let size = parts
            .terminal
            .size()
            .map(|s| (s.width, s.height))
            .unwrap_or((80, 24));
        let state = AppState::new(parts.ui, parts.clock, size);
        let timer = Timer::spawn(parts.tx.clone())?;
        let pool = RenderPool::spawn(
            parts.render_threads.max(1),
            render_pool::DEFAULT_QUEUE,
            mejl_render::RenderOptions::default(),
            parts.tx.clone(),
        )?;
        let io = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("mejl-io")
            .on_thread_start(crate::terminal::mark_thread_catches_panics)
            .enable_all()
            .build()?;
        Ok(App {
            state,
            data,
            terminal: parts.terminal,
            host: parts.host,
            handle: parts.handle,
            client: parts.client,
            tx: parts.tx,
            timer,
            pool,
            io: Some(io),
            settings: parts.settings,
            files: None,
            queue: VecDeque::new(),
            next_token: 1,
            tokens: HashMap::new(),
            sends: HashMap::new(),
            send_seq: HashMap::new(),
            cancel_wanted: HashSet::new(),
            send_now_wanted: HashSet::new(),
            quit_after_sends: None,
            masked_seqs: HashMap::new(),
            draft_copied: HashMap::new(),
            draft_held: HashMap::new(),
            input_turn: false,
            journal: Journal::default(),
            loading_more: HashSet::new(),
            prefetched: HashSet::new(),
            render_width: None,
            engine_status: mejl_sync::SyncStatus::default(),
            auth_error: None,
            shown_sync: None,
            shown_push: None,
            perf: None,
            frames: 0,
            quit: false,
            exit_note: None,
            read_error: None,
        })
    }

    /// Restores pane sizes, loads the first page and draws the first
    /// frame. `warnings` from the config are shown as a toast.
    pub fn start(&mut self, warnings: &[String]) -> std::io::Result<()> {
        if let Some(json) = self
            .data
            .ok("pane sizes", self.data.reader().ui_value(PANE_SIZES_KEY))
            && let Ok(p) = serde_json::from_str::<StoredPanes>(&json)
        {
            self.state.set_pane_sizes(ui::PaneSizes {
                sidebar: p.sidebar,
                list_ratio: p.list_ratio,
            });
        }
        let cmds = self.state.start(&self.data);
        for c in cmds {
            self.exec(c);
        }
        if let Some(first) = warnings.first() {
            let more = if warnings.len() > 1 {
                format!(
                    " (and {} more; run `mejl keys --check`)",
                    warnings.len() - 1
                )
            } else {
                String::new()
            };
            self.feed(Event::Msg(Msg::Toast {
                text: format!("{first}{more}"),
                level: ToastLevel::Warning,
            }));
        }
        self.drain();
        self.after_turn();
        self.draw(None)
    }

    /// Runs turns until the user quits or every sender is gone.
    pub fn run(&mut self, rx: &LoopReceiver<AppEvent>) -> std::io::Result<()> {
        while !self.quit {
            let batch = rx.recv_batch();
            if batch.is_empty() {
                break;
            }
            self.handle_batch(batch)?;
        }
        Ok(())
    }

    /// One turn of the loop: applies everything in `batch`, runs the
    /// commands, then draws once.
    pub fn handle_batch(&mut self, batch: Vec<LoopEvent<AppEvent>>) -> std::io::Result<()> {
        if let Some(sample) = self.perf.take() {
            self.feed(Event::Msg(Msg::Perf(sample)));
        }
        let mut first_input: Option<Instant> = None;
        for item in coalesce_batch(batch) {
            match item {
                LoopEvent::Input(t) => {
                    first_input = Some(first_input.map_or(t.at, |f| f.min(t.at)));
                    if let Some(ev) = Event::from_crossterm(t.event) {
                        self.feed(ev);
                    }
                }
                LoopEvent::InputClosed(reason) => {
                    self.exit_note = Some(format!("the terminal stopped sending input: {reason}"));
                    self.quit = true;
                }
                LoopEvent::Rendered(done) => self.rendered(done),
                LoopEvent::Tick | LoopEvent::Wake(_) => self.feed(Event::Msg(Msg::Tick)),
                LoopEvent::Signal(kind) => {
                    if kind.is_stop() {
                        self.quit = true;
                    } else {
                        self.repaint_all();
                    }
                }
                LoopEvent::Msg(AppEvent::Sync(e)) => self.sync_event(e),
                LoopEvent::Msg(AppEvent::Downloaded { name, result }) => match result {
                    Ok(path) => {
                        if let Err(e) = self.host.open_path(&path) {
                            self.toast(e, ToastLevel::Error);
                        }
                    }
                    Err(e) => self.toast(format!("Cannot download {name}: {e}"), ToastLevel::Error),
                },
            }
            self.drain();
            if self.quit {
                return Ok(());
            }
        }
        self.after_turn();
        self.draw(first_input)
    }

    /// Whether the user asked to quit, or the terminal went away.
    pub fn quitting(&self) -> bool {
        self.quit
    }

    /// Why the app stopped on its own, for a line after the terminal is
    /// restored.
    pub fn exit_note(&self) -> Option<&str> {
        self.exit_note.as_deref()
    }

    /// Sends the engine is still holding, for the note on quit.
    pub fn held_sends(&self) -> usize {
        self.engine_status.held_sends
    }

    /// Drafts whose newest version is not on the server yet: those the
    /// engine is uploading and those held back by the autosave debounce.
    pub fn drafts_pending(&self) -> usize {
        self.engine_status.drafts_pending + self.draft_held.len()
    }

    /// The UI state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The read model.
    pub fn data(&self) -> &StoreData {
        &self.data
    }

    /// The terminal.
    pub fn terminal(&self) -> &Terminal<B> {
        &self.terminal
    }

    /// The terminal, to resize a test backend.
    pub fn terminal_mut(&mut self) -> &mut Terminal<B> {
        &mut self.terminal
    }

    /// The engine's handle.
    pub fn handle(&self) -> &SyncHandle {
        &self.handle
    }

    /// Frames drawn so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Stops the timer, the workers and the download runtime. The engine
    /// is left to the caller, which shuts it down after the terminal is
    /// restored.
    pub fn close(mut self) -> SyncHandle {
        // The engine keeps queued copies across a restart; held ones only
        // live here.
        self.flush_draft_copies(true);
        self.timer.set_ticking(false);
        self.timer.cancel_wakes();
        if let Some(io) = self.io.take() {
            io.shutdown_background();
        }
        self.handle.clone()
    }

    fn feed(&mut self, ev: Event) {
        self.queue.push_back(ev);
        self.drain();
    }

    fn drain(&mut self) {
        while let Some(ev) = self.queue.pop_front() {
            let blur = matches!(ev, Event::Focus(false));
            let outer = std::mem::replace(&mut self.input_turn, asked_by_user(&ev));
            let cmds = ui::update(&mut self.state, &self.data, ev);
            for c in cmds {
                self.exec(c);
            }
            self.input_turn = outer;
            if blur {
                self.flush_draft_copies(true);
            }
        }
    }

    fn toast(&mut self, text: impl Into<String>, level: ToastLevel) {
        self.queue.push_back(Event::Msg(Msg::Toast {
            text: text.into(),
            level,
        }));
    }

    fn changed(&mut self, change: DataChange) {
        self.queue.push_back(Event::Msg(Msg::DataChanged(change)));
    }

    fn token(&mut self) -> u64 {
        let t = self.next_token;
        self.next_token += 1;
        t
    }

    fn send(&mut self, command: Command) -> bool {
        let ok = self.handle.send(command);
        if !ok {
            self.toast(
                "The sync engine has stopped; changes cannot be saved",
                ToastLevel::Error,
            );
        }
        ok
    }

    /// Housekeeping after a turn: window growth, read errors, the clock,
    /// wake-ups, and a quit that waits for sends.
    fn after_turn(&mut self) {
        if let Some(deadline) = self.quit_after_sends
            && (!self.sending() || Instant::now() >= deadline)
        {
            self.quit = true;
        }
        if let Some(mailbox) = self.data.take_load_more()
            && self.loading_more.insert(mailbox.clone())
        {
            self.handle.send(Command::LoadMore {
                mailbox: Id::from(mailbox),
            });
        }
        // A broken cache fails every read; say so once, not every frame.
        if let Some(e) = self.data.take_error()
            && self.read_error.as_ref() != Some(&e)
        {
            self.read_error = Some(e.clone());
            self.feed(Event::Msg(Msg::Error(e)));
        }
        self.flush_draft_copies(false);
        let drafts = convert::draft_sync(&self.engine_status, self.draft_held.keys());
        if self.state.draft_sync() != &drafts {
            self.state.set_draft_sync(drafts);
        }
        self.timer.set_ticking(self.state.shows_clock());
        if let Some(at) = self.state.next_wakeup() {
            self.timer.wake_at_timestamp(at);
        }
    }

    fn draw(&mut self, first_input: Option<Instant>) -> std::io::Result<()> {
        self.host.frame_begin();
        let state = &mut self.state;
        let data = &self.data;
        let result = self.terminal.draw(|f| ui::draw(state, data, f));
        self.host.frame_end();
        result.map_err(backend_error)?;
        self.frames += 1;
        if let Some(at) = first_input {
            self.perf = Some(FrameSample {
                input_to_flush: at.elapsed(),
            });
        }
        Ok(())
    }

    fn rendered(&mut self, done: RenderDone) {
        let email_id = ui::EmailId::new(done.email_id);
        let msg = match done.result {
            Ok(body) => Msg::BodyRendered {
                email_id,
                prefer_html: done.prefer_html,
                body: Box::new(body),
            },
            Err(error) => Msg::BodyFailed { email_id, error },
        };
        self.feed(Event::Msg(msg));
    }

    fn submit_render(&mut self, email: &str, width: usize, prefer_html: bool) -> bool {
        let Some(body) = self.data.body_text(email) else {
            return false;
        };
        self.pool.submit(RenderRequest {
            email_id: email.to_owned(),
            width,
            prefer_html,
            body,
        });
        true
    }

    // ----- commands -----

    fn exec(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::FetchBody(id) => {
                let email_id = Id::from(id.as_str());
                self.handle.send(Command::FetchBody {
                    email_id: email_id.clone(),
                });
                self.handle.send(Command::TouchBody { email_id });
            }
            Cmd::Prefetch(ids) => {
                self.prefetched = ids.iter().map(|i| i.as_str().to_owned()).collect();
                self.handle.send(Command::Prefetch {
                    email_ids: ids.iter().map(|i| Id::from(i.as_str())).collect(),
                });
            }
            Cmd::RenderBody(req) => {
                self.render_width = Some(req.width);
                if !self.submit_render(req.email_id.as_str(), req.width, req.prefer_html) {
                    self.handle.send(Command::FetchBody {
                        email_id: Id::from(req.email_id.as_str()),
                    });
                }
            }
            Cmd::Mutate(m) => self.mutate(&m),
            Cmd::OpenUrl(url) => {
                if openable(&url) {
                    if let Err(e) = self.host.open_url(url.trim()) {
                        self.toast(e, ToastLevel::Error);
                    }
                } else {
                    self.toast(
                        "mejl only opens web and mail links from messages",
                        ToastLevel::Warning,
                    );
                }
            }
            Cmd::CopyToClipboard(text) => {
                if let Err(e) = self.host.copy(&clipboard_text(&text)) {
                    self.toast(format!("Cannot copy: {e}"), ToastLevel::Error);
                }
            }
            Cmd::OpenInBrowser(id) => self.open_in_browser(&id),
            Cmd::OpenAttachment {
                email_id,
                blob_id,
                name,
            } => self.open_attachment(&email_id, &blob_id, &name),
            Cmd::RefreshNow => {
                self.handle.send(Command::RefreshNow);
            }
            Cmd::SaveLayout(p) => {
                let json = serde_json::to_string(&StoredPanes {
                    sidebar: p.sidebar,
                    list_ratio: p.list_ratio,
                })
                .unwrap_or_default();
                self.local(
                    LocalWrite::SetUiValue {
                        key: PANE_SIZES_KEY.to_owned(),
                        value: json,
                    },
                    DataChange::default(),
                    "the pane sizes",
                );
            }
            Cmd::WakeAt(at) => self.timer.wake_at_timestamp(at),
            Cmd::Quit => self.quit = true,
            Cmd::Suspend => {
                if let Err(e) = self.host.suspend() {
                    self.toast(e, ToastLevel::Error);
                }
                self.repaint_all();
            }
            Cmd::SendNowAndQuit(ids) => self.send_now_and_quit(ids),
            Cmd::SaveDraft(m) => self.save_draft(&m),
            Cmd::DeleteDraft(id) => self.delete_draft(&id),
            Cmd::Send(m) => self.send_message(m),
            Cmd::CancelSend(id) => self.cancel_send(id),
            Cmd::OpenEditor { draft_id, text } => {
                let result = self.host.edit(&text);
                // The editor drew over the screen; paint everything again.
                if let Err(e) = self.terminal.clear() {
                    tracing::warn!(error = %e, "cannot clear the terminal after the editor");
                }
                self.queue
                    .push_back(Event::Msg(Msg::EditorDone { draft_id, result }));
            }
            Cmd::AttachFile { draft_id, path } => {
                let token = self.token();
                self.tokens.insert(
                    token,
                    Waiting::Upload {
                        draft_id,
                        path: path.clone(),
                    },
                );
                self.send(Command::UploadAttachment {
                    token,
                    path: expand_home(path.trim()),
                    content_type: None,
                });
            }
            Cmd::ServerSearch { query, limit } => {
                let token = self.token();
                self.tokens.insert(
                    token,
                    Waiting::Search {
                        query: query.clone(),
                    },
                );
                self.send(Command::Search {
                    token,
                    request: SearchRequest {
                        query,
                        mailbox: None,
                        position: 0,
                        limit: u64::try_from(limit).unwrap_or(50),
                        collapse_threads: true,
                    },
                });
            }
            Cmd::SaveSearch { name, query } => {
                let json = self.data.add_saved(&name, &query);
                self.local(
                    LocalWrite::SetUiValue {
                        key: SAVED_SEARCHES_KEY.to_owned(),
                        value: json,
                    },
                    DataChange::default(),
                    "the saved search",
                );
                self.changed(DataChange {
                    mailboxes: true,
                    ..DataChange::default()
                });
            }
            Cmd::CreateMailbox {
                name,
                parent,
                then_add,
                then_move,
            } => {
                let token = self.token();
                let mailbox = mejl_jmap::types::Mailbox {
                    name: Some(name.clone()),
                    parent_id: parent.map(|p| Id::from(p.as_str())),
                    is_subscribed: Some(true),
                    ..Default::default()
                };
                self.tokens.insert(
                    token,
                    Waiting::CreateMailbox {
                        name: name.clone(),
                        then_add,
                        then_move,
                    },
                );
                self.journal.record(token, String::new(), Vec::new());
                self.send(Command::Mutate {
                    token,
                    mutation: Mutation::MailboxCreate(mailbox),
                    label: Some(format!("Create {name}")),
                    undoable: true,
                });
            }
            Cmd::MaskedCreate {
                for_domain,
                description,
                draft_id,
            } => {
                let token = self.token();
                self.tokens
                    .insert(token, Waiting::MaskedCreate { draft_id });
                self.journal.clear_redo();
                self.send(Command::MaskedCreate {
                    token,
                    for_domain: convert::site_origin(&for_domain),
                    description,
                    email_prefix: None,
                });
            }
            Cmd::MaskedSetState { id, state } => {
                let Some(wire) = convert::masked_state_for_sync(state) else {
                    self.toast("Fastmail sets that state itself", ToastLevel::Warning);
                    return;
                };
                let token = self.token();
                self.data.pending_mut().add_masked(
                    token,
                    MaskedOverlay {
                        id: id.clone(),
                        state: Some(state),
                        description: None,
                    },
                );
                self.tokens.insert(
                    token,
                    Waiting::MaskedChange {
                        label: format!("Set {}", wire.as_str()),
                    },
                );
                self.journal.record(token, String::new(), Vec::new());
                self.send(Command::MaskedSetState {
                    token,
                    id: Id::from(id),
                    state: wire,
                });
                self.changed(DataChange {
                    masked: true,
                    ..DataChange::default()
                });
            }
            Cmd::MaskedSetDescription { id, description } => {
                let token = self.token();
                self.data.pending_mut().add_masked(
                    token,
                    MaskedOverlay {
                        id: id.clone(),
                        state: None,
                        description: Some(description.clone()),
                    },
                );
                self.tokens.insert(
                    token,
                    Waiting::MaskedChange {
                        label: "Change the description".into(),
                    },
                );
                self.journal.record(token, String::new(), Vec::new());
                self.send(Command::MaskedSetDescription {
                    token,
                    id: Id::from(id),
                    description,
                });
                self.changed(DataChange {
                    masked: true,
                    ..DataChange::default()
                });
            }
            Cmd::SaveRule(rule) => self.save_rule(&rule),
            Cmd::DeleteRule(id) => self.rules_write(LocalWrite::DeleteRule(id)),
            Cmd::ReorderRules(ids) => self.rules_write(LocalWrite::ReorderRules(ids)),
            Cmd::RecordRuleRender { sieve, body_sha256 } => {
                self.rules_write(LocalWrite::RecordRuleRender { sieve, body_sha256 });
            }
            Cmd::ConfirmRulePaste { render_id } => {
                self.rules_write(LocalWrite::ConfirmRulePaste { render_id });
            }
            Cmd::SaveSetting(setting) => match self.settings.apply(&setting) {
                (Some(c), error) => {
                    self.queue.push_back(Event::Msg(Msg::Config(Box::new(c))));
                    if let Some(e) = error {
                        self.toast(
                            format!("Not saved, applies until you quit: {e}"),
                            ToastLevel::Warning,
                        );
                    }
                }
                (None, error) => {
                    let error = error.unwrap_or_else(|| "the change was refused".into());
                    self.queue
                        .push_back(Event::Msg(Msg::SettingRefused { error }));
                }
            },
            Cmd::EditKeys(edit) => match self.settings.edit_keys(&edit) {
                Ok(c) => self.queue.push_back(Event::Msg(Msg::Config(Box::new(c)))),
                Err(error) => self
                    .queue
                    .push_back(Event::Msg(Msg::SettingRefused { error })),
            },
        }
    }

    fn local(&mut self, write: LocalWrite, changes: DataChange, what: &'static str) {
        let token = self.token();
        self.tokens.insert(token, Waiting::Local { changes, what });
        self.send(Command::Local { token, write });
    }

    fn rules_write(&mut self, write: LocalWrite) {
        self.local(
            write,
            DataChange {
                rules: true,
                ..DataChange::default()
            },
            "the rules",
        );
    }

    fn save_rule(&mut self, rule: &ui::RuleJson) {
        let parsed: mejl_sieve::Rule = match serde_json::from_str(&rule.json) {
            Ok(r) => r,
            Err(e) => {
                self.toast(format!("Cannot save the rule: {e}"), ToastLevel::Error);
                return;
            }
        };
        let existing = self.data.ok("rules", self.data.reader().rules());
        let sort_order = existing
            .iter()
            .find(|r| r.id == rule.id)
            .map(|r| r.sort_order)
            .unwrap_or_else(|| existing.iter().map(|r| r.sort_order + 1).max().unwrap_or(0));
        let record = RuleRecord {
            id: rule.id.clone(),
            name: parsed.name.clone(),
            enabled: parsed.enabled,
            sort_order,
            rule_json: rule.json.clone(),
            updated_at: Timestamp::now(),
        };
        self.rules_write(LocalWrite::SaveRule(record));
    }

    fn save_draft(&mut self, m: &ui::OutgoingMessage) {
        let now = Timestamp::now();
        let created = self
            .data
            .ok("a draft", self.data.reader().draft(m.draft_id.as_str()))
            .map_or(now, |d| d.created_at);
        // The store keeps the server copy's id the engine recorded.
        let record = convert::draft_record(m, created, now);
        let token = self.token();
        self.tokens.insert(
            token,
            Waiting::SaveDraft {
                draft_id: m.draft_id.clone(),
            },
        );
        self.send(Command::Local {
            token,
            write: LocalWrite::SaveDraft(Box::new(record)),
        });
        self.copy_draft(m);
    }

    /// Asks for a server copy of a draft just saved locally, after the
    /// local save so the engine finds the row. A save the user asked for,
    /// or one after compose closed or moved to another message, copies at
    /// once. An autosave while compose stays open copies at most once per
    /// [`DRAFT_COPY_EVERY`]; the newest one is held until then, or until
    /// the terminal loses focus, compose closes or mejl quits.
    fn copy_draft(&mut self, m: &ui::OutgoingMessage) {
        let now = Instant::now();
        let open = self.state.draft_id() == Some(&m.draft_id);
        let due = match self.draft_copied.get(&m.draft_id) {
            Some(last) if open && !self.input_turn => *last + DRAFT_COPY_EVERY,
            _ => now,
        };
        self.draft_held.insert(
            m.draft_id.clone(),
            HeldCopy {
                message: Box::new(m.clone()),
                due,
            },
        );
        if due > now {
            self.timer.wake_at(due);
        }
        self.flush_draft_copies(false);
    }

    /// Sends held draft copies to the engine: those that are due, those
    /// whose compose is no longer open, or all of them. A copy waits while
    /// the account has no identity yet (before the first sync).
    fn flush_draft_copies(&mut self, all: bool) {
        if self.draft_held.is_empty() {
            return;
        }
        let now = Instant::now();
        let open = self.state.draft_id().cloned();
        let ready: Vec<DraftId> = self
            .draft_held
            .iter()
            .filter(|(id, held)| all || held.due <= now || open.as_ref() != Some(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            let Some(identity) = self
                .draft_held
                .get(&id)
                .and_then(|held| self.identity_for(&held.message))
            else {
                continue;
            };
            let Some(held) = self.draft_held.remove(&id) else {
                continue;
            };
            let message = convert::outgoing(&held.message, Id::from(identity));
            self.draft_copied.insert(id.clone(), now);
            self.send(Command::SaveDraftToServer {
                local_id: id.as_str().to_owned(),
                message: Box::new(message),
            });
        }
    }

    /// The identity a message goes out as: the one compose picked, else
    /// the one with the From address, else the first.
    fn identity_for(&self, m: &ui::OutgoingMessage) -> Option<String> {
        m.identity_id.clone().or_else(|| {
            let ids = self.data.identities();
            ids.iter()
                .find(|i| i.email.eq_ignore_ascii_case(&m.from.email))
                .or(ids.first())
                .map(|i| i.id.clone())
        })
    }

    /// Deletes a draft. The engine destroys its server copies too, durably,
    /// from the same local write.
    fn delete_draft(&mut self, id: &DraftId) {
        self.draft_held.remove(id);
        self.draft_copied.remove(id);
        self.local(
            LocalWrite::DeleteDraft(id.as_str().to_owned()),
            DataChange {
                drafts: true,
                ..DataChange::default()
            },
            "the draft",
        );
    }

    fn send_message(&mut self, m: Box<ui::OutgoingMessage>) {
        // The send makes its own copy and, once sent, the engine destroys
        // the draft's copies; a held autosave would only add one more.
        self.draft_held.remove(&m.draft_id);
        let Some(identity) = self.identity_for(&m) else {
            self.queue.push_back(Event::Msg(Msg::Send(SendReport {
                draft_id: m.draft_id.clone(),
                outcome: SendOutcome::Failed {
                    error: "the account has no sending identity yet; wait for the sync".into(),
                    message: m,
                },
            })));
            return;
        };
        let token = self.token();
        let outgoing = convert::outgoing(&m, Id::from(identity));
        let window = self.state.config().undo_send;
        self.tokens.insert(token, Waiting::Send { message: m });
        self.journal.record(token, String::new(), Vec::new());
        self.send(Command::Send {
            token,
            message: Box::new(outgoing),
            undo_window: window,
        });
    }

    /// Whether a send is queued or held and not yet sent or failed.
    fn sending(&self) -> bool {
        !self.sends.is_empty()
            || self
                .tokens
                .values()
                .any(|w| matches!(w, Waiting::Send { .. }))
    }

    /// Ends the hold on these sends and quits once nothing is being sent,
    /// or after [`SEND_NOW_WAIT`]. The loop keeps running meanwhile, so the
    /// screen stays live and the engine's answers arrive.
    fn send_now_and_quit(&mut self, ids: Vec<DraftId>) {
        for id in ids {
            match self.send_seq.get(&id).copied() {
                Some(seq) => {
                    self.send(Command::SendNow { seq });
                }
                // Not queued yet; end the hold as soon as it is.
                None => {
                    self.send_now_wanted.insert(id);
                }
            }
        }
        if self.sending() {
            let deadline = Instant::now() + SEND_NOW_WAIT;
            self.quit_after_sends = Some(deadline);
            self.timer.wake_at(deadline);
        } else {
            self.quit = true;
        }
    }

    /// A send failed while quitting waited for it: stay, so the user sees
    /// why and the message is back in compose.
    fn stop_quit_after_sends(&mut self) {
        if self.quit_after_sends.take().is_some() {
            self.send_now_wanted.clear();
            self.toast("Not quitting: a message was not sent", ToastLevel::Warning);
        }
    }

    /// Paints the whole screen again at the terminal's size now, after
    /// something else drew over it or it was resized while mejl was not
    /// looking (a suspend, or SIGCONT).
    fn repaint_all(&mut self) {
        if let Err(e) = self.terminal.clear() {
            tracing::warn!(error = %e, "cannot clear the terminal");
        }
        match self.terminal.size() {
            Ok(size) => self.queue.push_back(Event::Resize(size.width, size.height)),
            Err(e) => tracing::warn!(error = %e, "cannot read the terminal size"),
        }
    }

    fn cancel_send(&mut self, draft_id: DraftId) {
        match self.send_seq.get(&draft_id).copied() {
            Some(seq) => {
                let token = self.token();
                self.tokens.insert(token, Waiting::CancelSend { draft_id });
                self.send(Command::CancelSend { token, seq });
            }
            // The engine has not said which entry holds it yet; cancel as
            // soon as it does.
            None => {
                self.cancel_wanted.insert(draft_id);
            }
        }
    }

    fn mutate(&mut self, m: &ui::Mutation) {
        let plan = match mutate::plan(
            &self.data,
            m,
            self.settings.config().ui.mark_read_on_archive,
        ) {
            Ok(p) => p,
            Err(text) => {
                self.toast(text, ToastLevel::Warning);
                return;
            }
        };
        let token = self.token();
        match plan {
            Plan::Undo | Plan::Redo => {
                let redo = plan == Plan::Redo;
                let snapshots = self.undo_preview(redo);
                if redo {
                    self.tokens.insert(token, Waiting::Redo);
                    self.send(Command::Redo { token });
                } else {
                    self.tokens.insert(token, Waiting::Undo);
                    self.send(Command::Undo { token });
                }
                if !snapshots.is_empty() {
                    let threads: Vec<ui::ThreadId> = snapshots
                        .iter()
                        .map(|s| ui::ThreadId::new(s.thread.as_str()))
                        .collect();
                    self.data.pending_mut().add_threads(token, snapshots);
                    self.changed(DataChange {
                        mailboxes: true,
                        threads,
                        ..DataChange::default()
                    });
                }
            }
            Plan::Queue {
                mutation,
                label,
                undoable,
                snapshots,
            } => {
                let threads: Vec<ui::ThreadId> = snapshots
                    .iter()
                    .map(|s| ui::ThreadId::new(s.thread.as_str()))
                    .collect();
                if undoable {
                    self.journal.record(token, label.clone(), snapshots.clone());
                } else {
                    self.journal.clear_redo();
                }
                self.data.pending_mut().add_threads(token, snapshots);
                self.tokens.insert(
                    token,
                    Waiting::Mutate {
                        label: label.clone(),
                    },
                );
                if self.send(Command::Mutate {
                    token,
                    mutation,
                    label: Some(label),
                    undoable,
                }) {
                    self.changed(DataChange {
                        mailboxes: true,
                        threads,
                        ..DataChange::default()
                    });
                } else {
                    self.journal.rejected(token);
                    self.data.pending_mut().remove(token);
                }
            }
            Plan::Bulk {
                mailbox,
                except,
                action,
                label,
                overlay,
            } => {
                let shown = label
                    .clone()
                    .unwrap_or_else(|| action.label(0).trim_end_matches(" 0").to_owned());
                self.journal.record(token, String::new(), Vec::new());
                self.data.pending_mut().add_bulk(token, overlay);
                self.tokens.insert(token, Waiting::Mutate { label: shown });
                if self.send(Command::Bulk {
                    token,
                    mailbox,
                    except_threads: except,
                    action,
                    label,
                }) {
                    self.changed(DataChange {
                        mailboxes: true,
                        ..DataChange::default()
                    });
                } else {
                    self.journal.rejected(token);
                    self.data.pending_mut().remove(token);
                }
            }
        }
    }

    /// What an undo (or redo) changes, to show in the frame of its key. See
    /// [`overlay`]: nothing while another undo or redo is on its way, as
    /// the store's stack is then moving.
    fn undo_preview(&self, redo: bool) -> Vec<ThreadSnapshot> {
        if self
            .tokens
            .values()
            .any(|w| matches!(w, Waiting::Undo | Waiting::Redo))
        {
            return Vec::new();
        }
        let reader = self.data.reader();
        let now = |thread: &str| self.data.thread_flags(thread);
        if redo {
            let top = self.data.ok("the redo stack", reader.redo_peek());
            self.journal.redo_preview(top.as_deref(), now)
        } else {
            let top = self.data.ok("the undo stack", reader.undo_peek());
            self.journal.undo_preview(top.as_deref(), now)
        }
    }

    fn files_dir(&mut self) -> Result<PathBuf, String> {
        if self.files.is_none() {
            let dir = tempfile::Builder::new()
                .prefix("mejl-open-")
                .tempdir()
                .map_err(|e| format!("cannot make a folder for the file: {e}"))?;
            self.files = Some(dir);
        }
        self.files
            .as_ref()
            .map(|d| d.path().to_owned())
            .ok_or_else(|| "no folder for files".to_owned())
    }

    fn open_in_browser(&mut self, id: &ui::EmailId) {
        let Some(body) = self.data.message_body(id) else {
            self.toast("The message is not downloaded yet", ToastLevel::Warning);
            return;
        };
        let subject = self
            .data
            .ok("a message", self.data.reader().email(id.as_str()))
            .map(|d| d.headers.row.subject)
            .unwrap_or_default();
        let page = browser_page(&subject, &body, self.state.config().remote_images);
        let result = self.files_dir().and_then(|dir| {
            let path = dir.join(format!("message-{}.html", safe_file_name(id.as_str())));
            std::fs::write(&path, page)
                .map(|()| path)
                .map_err(|e| format!("cannot write the page: {e}"))
        });
        match result.and_then(|p| self.host.open_path(&p)) {
            Ok(()) => self.toast("Opened in the browser", ToastLevel::Info),
            Err(e) => self.toast(e, ToastLevel::Error),
        }
    }

    fn open_attachment(&mut self, email: &ui::EmailId, blob: &ui::BlobId, name: &str) {
        let Some(session) = self.client.session() else {
            self.toast("Not connected to Fastmail yet", ToastLevel::Warning);
            return;
        };
        let Some(account) = session.primary_account(capability::MAIL).cloned() else {
            self.toast("The session has no mail account", ToastLevel::Error);
            return;
        };
        let dir = match self.files_dir() {
            Ok(d) => d,
            Err(e) => {
                self.toast(e, ToastLevel::Error);
                return;
            }
        };
        let mime = self
            .data
            .message_body(email)
            .and_then(|b| {
                b.attachments
                    .into_iter()
                    .find(|a| &a.blob_id == blob)
                    .map(|a| a.mime_type)
            })
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let safe = safe_file_name(name);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let blob = Id::from(blob.as_str());
        let Some(io) = &self.io else {
            return;
        };
        let shown = safe.clone();
        io.spawn(async move {
            let result = match client.download(&account, &blob, &shown, &mime).await {
                Ok(bytes) => save_unique(&dir, &shown, &bytes),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.msg(AppEvent::Downloaded {
                name: shown,
                result,
            });
        });
        self.toast(format!("Downloading {safe}"), ToastLevel::Info);
    }

    // ----- engine events -----

    fn report_send(&mut self, draft_id: DraftId, outcome: SendOutcome) {
        self.queue
            .push_back(Event::Msg(Msg::Send(SendReport { draft_id, outcome })));
    }

    fn show_status(&mut self) {
        let sync = match &self.auth_error {
            Some(e) => ui::SyncStatus::Failed { error: e.clone() },
            None => convert::sync_status(&self.engine_status),
        };
        if self.shown_sync.as_ref() != Some(&sync) {
            self.shown_sync = Some(sync.clone());
            self.queue.push_back(Event::Msg(Msg::Sync(sync)));
        }
        let push = convert::push_state(self.engine_status.push);
        if self.shown_push != Some(push) {
            self.shown_push = Some(push);
            self.queue.push_back(Event::Msg(Msg::Push(push)));
        }
    }

    fn sync_event(&mut self, e: SyncEvent) {
        match e {
            SyncEvent::Status(s) => {
                self.engine_status = s;
                self.show_status();
            }
            SyncEvent::DataChanged(c) => self.changed(convert::data_change(&c)),
            SyncEvent::AuthFailed { error } => {
                let text = auth_message(&error);
                self.auth_error = Some(text.clone());
                self.show_status();
                self.queue.push_back(Event::Msg(Msg::Error(text)));
            }
            SyncEvent::Applied {
                token,
                seq,
                temp_id,
                changed,
            } => self.applied(token, seq, temp_id, &changed),
            SyncEvent::Rejected { token, error } => self.rejected(token, error),
            SyncEvent::Undone {
                token,
                outcome,
                changed,
            } => {
                self.tokens.remove(&token);
                self.undo_answered(token, &changed);
                match outcome {
                    None => {
                        self.journal.nothing_to_undo();
                        self.toast("Nothing to undo", ToastLevel::Info);
                    }
                    Some(UndoOutcome::Reverted {
                        label,
                        original,
                        seq,
                    }) => {
                        self.journal.undone(original, Some(seq));
                        self.toast(format!("Undid: {label}"), ToastLevel::Info);
                    }
                    Some(UndoOutcome::Cancelled {
                        label,
                        seq,
                        payload,
                    }) => {
                        self.journal.undone(seq, None);
                        if let Some(held) = self.sends.remove(&seq) {
                            self.send_seq.remove(&held.message.draft_id);
                            self.stop_quit_after_sends();
                            let draft_id = held.message.draft_id.clone();
                            // The text may exist nowhere else: a later close
                            // of compose would see it as already saved.
                            self.save_draft(&held.message);
                            self.report_send(
                                draft_id,
                                SendOutcome::Cancelled {
                                    message: held.message,
                                },
                            );
                        } else if let Some(m) =
                            payload.as_ref().and_then(mejl_sync::decode_send_payload)
                        {
                            let message = convert::outgoing_back(&m);
                            self.save_draft(&message);
                            self.report_send(
                                message.draft_id.clone(),
                                SendOutcome::Cancelled {
                                    message: Box::new(message),
                                },
                            );
                        } else {
                            self.toast(format!("Undid: {label}"), ToastLevel::Info);
                        }
                    }
                    Some(UndoOutcome::TooLate { label, seq }) => {
                        self.journal.undone(seq, None);
                        self.toast(format!("Too late to undo {label}"), ToastLevel::Warning);
                    }
                }
            }
            SyncEvent::Redone {
                token,
                outcome,
                changed,
            } => {
                self.tokens.remove(&token);
                self.undo_answered(token, &changed);
                match outcome {
                    None => {
                        self.journal.nothing_to_redo();
                        self.toast("Nothing to redo", ToastLevel::Info);
                    }
                    Some(mejl_store::RedoOutcome::Reapplied { label, undone, seq }) => {
                        self.journal.redone(undone, Some(seq));
                        self.toast(format!("Redid: {label}"), ToastLevel::Info);
                    }
                    Some(mejl_store::RedoOutcome::TooLate { label, seq }) => {
                        self.journal.redone(seq, None);
                        self.toast(format!("Too late to redo {label}"), ToastLevel::Warning);
                    }
                }
            }
            SyncEvent::OutboxDone { .. } => {}
            SyncEvent::OutboxFailed {
                seq, label, error, ..
            } => {
                if let Some(held) = self.sends.remove(&seq) {
                    self.send_seq.remove(&held.message.draft_id);
                    self.stop_quit_after_sends();
                    let draft_id = held.message.draft_id.clone();
                    self.report_send(
                        draft_id,
                        SendOutcome::Failed {
                            error,
                            message: held.message,
                        },
                    );
                } else if let Some(draft_id) = self.masked_seqs.remove(&seq) {
                    self.queue.push_back(Event::Msg(Msg::MaskedCreated {
                        draft_id,
                        result: Err(error),
                    }));
                } else {
                    self.queue.push_back(Event::Msg(Msg::Outbox(OutboxReport {
                        label: label.unwrap_or_else(|| "A change".to_owned()),
                        outcome: OutboxOutcome::Failed { error },
                    })));
                }
                self.changed(DataChange {
                    mailboxes: true,
                    masked: true,
                    ..DataChange::default()
                });
            }
            SyncEvent::SendQueued {
                token,
                seq,
                hold_until,
                ..
            } => {
                let Some(Waiting::Send { message }) = self.tokens.remove(&token) else {
                    return;
                };
                self.journal.applied(token, seq);
                let draft_id = message.draft_id.clone();
                self.sends.insert(seq, HeldSend { message });
                self.send_seq.insert(draft_id.clone(), seq);
                self.report_send(draft_id.clone(), SendOutcome::Scheduled { at: hold_until });
                if self.cancel_wanted.remove(&draft_id) {
                    self.cancel_send(draft_id);
                } else if self.send_now_wanted.remove(&draft_id) {
                    self.send(Command::SendNow { seq });
                }
            }
            SyncEvent::SendCancelled {
                token,
                seq,
                message,
            } => {
                let asked = match self.tokens.remove(&token) {
                    Some(Waiting::CancelSend { draft_id }) => Some(draft_id),
                    _ => None,
                };
                let held = self.sends.remove(&seq);
                let draft_id = held
                    .as_ref()
                    .map(|h| h.message.draft_id.clone())
                    .or(asked)
                    .or_else(|| message.as_ref().map(|m| convert::outgoing_back(m).draft_id));
                let Some(draft_id) = draft_id else {
                    return;
                };
                self.send_seq.remove(&draft_id);
                if message.is_some() {
                    self.stop_quit_after_sends();
                }
                let outcome = match (message, held) {
                    (Some(_), Some(h)) => {
                        self.save_draft(&h.message);
                        SendOutcome::Cancelled { message: h.message }
                    }
                    (Some(m), None) => {
                        let message = convert::outgoing_back(&m);
                        self.save_draft(&message);
                        SendOutcome::Cancelled {
                            message: Box::new(message),
                        }
                    }
                    (None, held) => {
                        // Too late: it is going out; keep tracking it.
                        if let Some(h) = held {
                            self.send_seq.insert(draft_id.clone(), seq);
                            self.sends.insert(seq, h);
                        }
                        SendOutcome::TooLate
                    }
                };
                self.report_send(draft_id, outcome);
            }
            SyncEvent::Sent { seq, .. } => {
                if let Some(held) = self.sends.remove(&seq) {
                    self.send_seq.remove(&held.message.draft_id);
                    self.mark_original(&held.message);
                    self.report_send(held.message.draft_id.clone(), SendOutcome::Sent);
                } else {
                    self.toast("Sent", ToastLevel::Success);
                }
                self.changed(DataChange {
                    mailboxes: true,
                    drafts: true,
                    ..DataChange::default()
                });
            }
            SyncEvent::BodyReady { email_id } => {
                let id = email_id.as_str().to_owned();
                self.queue
                    .push_back(Event::Msg(Msg::BodyFetched(ui::EmailId::new(id.as_str()))));
                // Prefetched bodies are laid out ahead, at the reader's
                // width, so opening them is instant.
                if self.prefetched.remove(&id)
                    && let Some(width) = self.render_width
                {
                    let prefer = self.state.config().prefer_html;
                    self.submit_render(&id, width, prefer);
                }
            }
            SyncEvent::BodyFailed { email_id, error } => {
                self.queue.push_back(Event::Msg(Msg::BodyFailed {
                    email_id: ui::EmailId::new(email_id.as_str()),
                    error,
                }));
            }
            SyncEvent::WindowLoaded { mailbox, .. } => {
                self.loading_more.remove(mailbox.as_str());
                self.changed(DataChange {
                    mailboxes: true,
                    ..DataChange::default()
                });
            }
            SyncEvent::Uploaded {
                token,
                blob_id,
                name,
                content_type,
                size,
            } => {
                if let Some(Waiting::Upload { draft_id, path }) = self.tokens.remove(&token) {
                    self.queue.push_back(Event::Msg(Msg::Attached {
                        draft_id,
                        path,
                        result: Ok(ui::OutgoingAttachment {
                            blob_id: ui::BlobId::new(blob_id.as_str()),
                            name,
                            mime_type: content_type,
                            size,
                        }),
                    }));
                }
            }
            SyncEvent::UploadFailed { token, error } => {
                if let Some(Waiting::Upload { draft_id, path }) = self.tokens.remove(&token) {
                    self.queue.push_back(Event::Msg(Msg::Attached {
                        draft_id,
                        path,
                        result: Err(error),
                    }));
                }
            }
            SyncEvent::SearchResults { token, results } => {
                if let Some(Waiting::Search { query }) = self.tokens.remove(&token) {
                    let rows = self.data.rows_for_emails(&results.ids);
                    self.queue.push_back(Event::Msg(Msg::ServerSearch {
                        query,
                        result: Ok(rows),
                    }));
                    if !results.unsupported.is_empty() {
                        self.toast(
                            format!(
                                "Fastmail could not search for: {}",
                                results.unsupported.join(", ")
                            ),
                            ToastLevel::Info,
                        );
                    }
                }
            }
            SyncEvent::SearchFailed { token, error } => {
                if let Some(Waiting::Search { query }) = self.tokens.remove(&token) {
                    self.queue.push_back(Event::Msg(Msg::ServerSearch {
                        query,
                        result: Err(error),
                    }));
                }
            }
            SyncEvent::MaskedCreated { seq, id, email } => {
                let draft_id = self.masked_seqs.remove(&seq).flatten();
                let view = self
                    .data
                    .masked_emails()
                    .into_iter()
                    .find(|m| m.id == id.as_str())
                    .unwrap_or_else(|| ui::MaskedEmailView {
                        id: id.as_str().to_owned(),
                        email: email.clone().unwrap_or_default(),
                        state: ui::MaskedState::Enabled,
                        for_domain: String::new(),
                        description: String::new(),
                        created_at: Some(Timestamp::now()),
                        last_message_at: None,
                        created_by: String::new(),
                        local_messages: 0,
                        local_in_spam: 0,
                    });
                let view = ui::MaskedEmailView {
                    email: email.unwrap_or(view.email.clone()),
                    ..view
                };
                self.queue.push_back(Event::Msg(Msg::MaskedCreated {
                    draft_id,
                    result: Ok(view),
                }));
                self.changed(DataChange {
                    masked: true,
                    ..DataChange::default()
                });
            }
            SyncEvent::LocalDone { token, result } => self.local_done(token, result),
        }
    }

    /// The store answered an undo or redo: its own overlay now holds the
    /// change, so the early entry goes, and what moved is read again.
    fn undo_answered(&mut self, token: u64, changed: &mejl_sync::DataChanges) {
        let threads = self.data.pending_mut().remove(token).unwrap_or_default();
        let mut change = convert::data_change(changed);
        change
            .threads
            .extend(threads.iter().map(|t| ui::ThreadId::new(t.as_str())));
        self.changed(change);
    }

    fn applied(
        &mut self,
        token: u64,
        seq: Seq,
        temp_id: Option<String>,
        changed: &mejl_sync::DataChanges,
    ) {
        self.journal.applied(token, seq);
        let threads = self.data.pending_mut().remove(token).unwrap_or_default();
        let mut change = convert::data_change(changed);
        change.mailboxes = true;
        change
            .threads
            .extend(threads.iter().map(|t| ui::ThreadId::new(t.as_str())));
        match self.tokens.remove(&token) {
            Some(Waiting::CreateMailbox {
                then_add: Some(target),
                then_move,
                ..
            }) => {
                if let Some(temp) = temp_id {
                    let mailbox = ui::MailboxId::new(temp);
                    let m = if then_move {
                        ui::Mutation::MoveTo { target, mailbox }
                    } else {
                        ui::Mutation::SetMailbox {
                            target,
                            mailbox,
                            member: true,
                        }
                    };
                    self.mutate(&m);
                }
            }
            Some(Waiting::MaskedCreate { draft_id }) => {
                self.masked_seqs.insert(seq, draft_id);
            }
            Some(Waiting::MaskedChange { .. }) => change.masked = true,
            _ => {}
        }
        self.changed(change);
    }

    fn rejected(&mut self, token: u64, error: String) {
        self.journal.rejected(token);
        let threads = self.data.pending_mut().remove(token).unwrap_or_default();
        self.changed(DataChange {
            mailboxes: true,
            masked: true,
            threads: threads
                .iter()
                .map(|t| ui::ThreadId::new(t.as_str()))
                .collect(),
            ..DataChange::default()
        });
        match self.tokens.remove(&token) {
            Some(Waiting::Send { message }) => {
                self.stop_quit_after_sends();
                self.report_send(
                    message.draft_id.clone(),
                    SendOutcome::Failed { error, message },
                );
            }
            Some(Waiting::MaskedCreate { draft_id }) => {
                self.queue.push_back(Event::Msg(Msg::MaskedCreated {
                    draft_id,
                    result: Err(error),
                }));
            }
            Some(Waiting::CancelSend { .. }) => {
                self.toast(
                    format!("Cannot cancel the send: {error}"),
                    ToastLevel::Error,
                );
            }
            Some(Waiting::Mutate { label } | Waiting::MaskedChange { label }) => {
                self.queue
                    .push_back(Event::Msg(Msg::Error(format!("{label} failed: {error}"))));
            }
            Some(Waiting::CreateMailbox { name, .. }) => {
                self.queue.push_back(Event::Msg(Msg::Error(format!(
                    "Cannot create {name}: {error}"
                ))));
            }
            Some(Waiting::Undo) => self.toast(format!("Cannot undo: {error}"), ToastLevel::Error),
            Some(Waiting::Redo) => self.toast(format!("Cannot redo: {error}"), ToastLevel::Error),
            _ => self.queue.push_back(Event::Msg(Msg::Error(error))),
        }
    }

    fn local_done(&mut self, token: u64, result: Result<LocalOutcome, String>) {
        match self.tokens.remove(&token) {
            Some(Waiting::SaveDraft { draft_id }) => {
                self.queue.push_back(Event::Msg(Msg::DraftSaved {
                    draft_id,
                    error: result.err(),
                }));
            }
            Some(Waiting::Local { changes, what }) => match result {
                Ok(_) => {
                    if changes != DataChange::default() {
                        self.changed(changes);
                    }
                }
                Err(e) => {
                    self.changed(changes);
                    self.queue
                        .push_back(Event::Msg(Msg::Error(format!("Cannot save {what}: {e}"))));
                }
            },
            _ => {}
        }
    }

    /// Marks the message a reply or forward answered, once it is sent.
    /// Any new outbox entry clears the redo stack, so the mark is skipped
    /// while there is something to redo: the user's redo matters more than
    /// the answered flag.
    fn mark_original(&mut self, m: &ui::OutgoingMessage) {
        let Some(original) = &m.original else {
            return;
        };
        if self
            .data
            .ok("the redo stack", self.data.reader().redo_peek())
            .is_some()
        {
            return;
        }
        let keyword = match m.kind {
            ui::ComposeKind::Reply | ui::ComposeKind::ReplyAll => "$answered",
            ui::ComposeKind::Forward => "$forwarded",
            ui::ComposeKind::New => return,
        };
        let token = self.token();
        self.tokens.insert(
            token,
            Waiting::Mutate {
                label: "Mark as answered".into(),
            },
        );
        self.handle.send(Command::Mutate {
            token,
            mutation: Mutation::EmailPatch(vec![EmailPatchTarget::new(
                original.as_str(),
                Patch::new().keyword(keyword, true),
            )]),
            label: None,
            undoable: false,
        });
    }
}

/// Pane sizes as stored in `state.db`.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct StoredPanes {
    sidebar: u16,
    list_ratio: Option<f32>,
}

/// Saves bytes under a name in `dir`, adding a number when the name is
/// taken, and returns the path.
fn save_unique(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    for n in 0..1000 {
        let candidate = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, bytes)
                    .map_err(|e| format!("cannot save the file: {e}"))?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot save the file: {e}")),
        }
    }
    Err("too many files with that name".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_from_mail_cannot_escape_the_folder() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("   "), "attachment");
        assert_eq!(safe_file_name("a\u{0}b\nc.pdf"), "abc.pdf");
        assert_eq!(safe_file_name("rota.pdf"), "rota.pdf");
        assert_eq!(safe_file_name(&"x".repeat(500)).len(), 120);
    }

    #[test]
    fn copied_text_keeps_lines_but_no_controls_or_bidi() {
        assert_eq!(
            clipboard_text("a\u{1b}]52;c;eA==\u{7}b\u{9b}2J\u{202E}c\u{2066}\r\nd\te"),
            "a]52;c;eA==b2Jc\nd\te"
        );
        assert_eq!(clipboard_text("plain@example.org"), "plain@example.org");
    }

    #[test]
    fn only_web_and_mail_links_open() {
        assert!(openable("https://example.org/x"));
        assert!(openable("  HTTP://example.org"));
        assert!(openable("mailto:juniper@example.net"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("vscode://open"));
    }

    #[test]
    fn browser_pages_block_remote_content_first() {
        let body = ui::BodyView {
            email_id: ui::EmailId::new("E1"),
            text: Some("<b>not html</b>".into()),
            html: None,
            attachments: vec![],
            headers: vec![],
        };
        let page = browser_page("Hi & bye", &body, false);
        let csp = page.find("Content-Security-Policy").unwrap();
        assert!(csp < page.find("<body>").unwrap());
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("img-src data: cid:"));
        assert!(page.contains("&lt;b&gt;not html"));
        assert!(page.contains("<title>Hi &amp; bye</title>"));
        assert!(browser_page("s", &body, true).contains("img-src * data:"));
    }

    #[test]
    fn browser_pages_sanitize_hostile_html() {
        let hostile = ui::BodyView {
            email_id: ui::EmailId::new("E1"),
            text: None,
            html: Some(
                r#"<meta http-equiv="refresh" content="0;url=https://evil.example.com"><p onclick="x()">Hi</p><script>x()</script><img src="https://img.example.com/a.png">"#
                    .into(),
            ),
            attachments: vec![],
            headers: vec![],
        };
        let page = browser_page("s", &hostile, false);
        assert!(page.contains("<body><p>Hi</p><img></body>"), "{page}");
        assert!(!page.contains("evil.example.com"));
        let page = browser_page("s", &hostile, true);
        assert!(
            page.contains(r#"<img src="https://img.example.com/a.png">"#),
            "{page}"
        );
        assert_eq!(page.matches("http-equiv").count(), 1, "only the CSP");
    }

    #[test]
    fn unique_names_do_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let a = save_unique(dir.path(), "rota.pdf", b"1").unwrap();
        let b = save_unique(dir.path(), "rota.pdf", b"2").unwrap();
        assert_ne!(a, b);
        assert!(b.ends_with("rota (1).pdf"));
        assert_eq!(std::fs::read(&a).unwrap(), b"1");
    }

    #[test]
    fn home_is_expanded() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/x.txt"), PathBuf::from(home).join("x.txt"));
        }
        assert_eq!(expand_home("/tmp/x"), PathBuf::from("/tmp/x"));
    }
}
