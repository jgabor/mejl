//! Macros through the whole app, headless: a key runs every step on the
//! same mail as one outbox entry, one `z` takes it all back, and the fake
//! server ends in the state the steps describe. The harness is a trimmed
//! copy of the one in `app.rs`: the real loop, engine and store, with
//! mejl-fake in process. Nothing here touches the network, a keyring or
//! the user's directories.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use mejl::app::host::RecordingHost;
use mejl::app::settings::Settings;
use mejl::app::{App, AppEvent, AppParts};
use mejl::config::Config;
use mejl::events::{self, LoopEvent, LoopReceiver, TimedEvent};
use mejl_fake::FakeServer;
use mejl_jmap::transport::{
    BoxFuture, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
};
use mejl_jmap::{Client, Secret};
use mejl_store::{Store, StoreOptions};
use mejl_sync::{Engine, EngineConfig};
use mejl_ui::{MailData, Screen, ThreadId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);

/// Holds each request back for a while when asked, like a slow network,
/// so a queued entry can be seen before it is sent.
struct Slow {
    inner: Arc<dyn Transport>,
    delay_ms: Arc<AtomicU64>,
}

impl Transport for Slow {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let delay = Duration::from_millis(self.delay_ms.load(Ordering::SeqCst));
        let sent = self.inner.send(req);
        if delay.is_zero() {
            return sent;
        }
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            sent.await
        })
    }

    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
        self.inner.stream(req)
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        push_ping_seconds: 1,
        push_idle_margin: Duration::from_millis(500),
        push_debounce: Duration::from_millis(10),
        poll_interval: Duration::from_secs(2),
        backoff_min: Duration::from_millis(20),
        backoff_max: Duration::from_millis(300),
        data_coalesce: Duration::from_millis(5),
        shutdown_timeout: Duration::from_secs(3),
        time_zone: TimeZone::UTC,
        ..EngineConfig::default()
    }
}

/// The demo macro, "mark read, unpin, move to a folder", on F2, with the
/// folder and other steps as given.
fn file_receipt(steps: &str) -> toml::Table {
    format!("[file-receipt]\nlabel = \"File receipt\"\nkeys = [\"F2\"]\nsteps = {steps}\n")
        .parse()
        .unwrap()
}

const FILE_RECEIPT: &str = r#"["mark-read", "unpin", { move = "Receipts" }]"#;

/// A config with `macros` and the given preset.
fn config(macros: toml::Table, preset: &str) -> Config {
    let mut config = Config {
        macros,
        ..Config::default()
    };
    config.ui.preset = preset.to_owned();
    config
}

struct Harness {
    app: App<TestBackend>,
    rx: LoopReceiver<AppEvent>,
    server: FakeServer,
    delay_ms: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// The demo account with every Inbox message unread and pinned, so
    /// each step of the macro changes something, synced and showing the
    /// Inbox.
    fn start(config: Config) -> Harness {
        let server = FakeServer::demo(Timestamp::now());
        unread_and_pinned(&server);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        let delay_ms = Arc::new(AtomicU64::new(0));
        let transport: Arc<dyn Transport> = Arc::new(Slow {
            inner: Arc::new(server.transport()),
            delay_ms: delay_ms.clone(),
        });
        let client = Client::new(
            transport,
            server.session_url(),
            Secret::new(server.token().to_owned()),
        );
        let (tx, rx) = events::channel::<AppEvent>();
        let sink = tx.clone();
        let handle = Engine::start(engine_config(), client.clone(), store, move |e| {
            let _ = sink.msg(AppEvent::Sync(e));
        })
        .unwrap();
        let settings = Settings::new(dir.path().join("config.toml"), config, false, TimeZone::UTC);
        let (ui, _) = settings.ui_config().unwrap();
        assert_eq!(ui.macros.len(), 1, "the macro is loaded");
        let mut app = App::new(AppParts {
            terminal: Terminal::new(TestBackend::new(160, 40)).unwrap(),
            host: Box::new(RecordingHost::default()),
            handle,
            client,
            tx,
            settings,
            ui,
            account: "test".into(),
            clock: Arc::new(mejl_ui::SystemClock),
            render_threads: 1,
        })
        .unwrap();
        app.start(&[]).unwrap();
        let mut hs = Harness {
            app,
            rx,
            server,
            delay_ms,
            _dir: dir,
        };
        hs.wait("the inbox", |hs| {
            hs.screen().contains("Re: Garden club plot rota")
        });
        hs.wait("the first sync to finish", |hs| {
            hs.screen().contains("synced")
        });
        hs
    }

    fn screen(&self) -> String {
        let buffer = self.app.terminal().backend().buffer();
        let width = buffer.area.width as usize;
        let mut out = String::new();
        for (i, cell) in buffer.content.iter().enumerate() {
            out.push_str(cell.symbol());
            if (i + 1) % width == 0 {
                out.push('\n');
            }
        }
        out
    }

    /// Runs turns of the loop until `pred` holds.
    fn wait(&mut self, what: &str, pred: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if pred(self) {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {what}:\n{}",
                self.screen()
            );
            let step = (now + Duration::from_millis(20)).min(deadline);
            let batch = self.rx.recv_batch_until(step);
            if !batch.is_empty() {
                self.app.handle_batch(batch).unwrap();
            }
        }
    }

    /// Runs the loop for a while without waiting for anything.
    fn idle(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline {
            let batch = self.rx.recv_batch_until(deadline);
            if !batch.is_empty() {
                self.app.handle_batch(batch).unwrap();
            }
        }
    }

    /// One key as one turn of the loop, so what is drawn after it is the
    /// frame of the key.
    fn key(&mut self, spec: &str) {
        self.app
            .handle_batch(vec![LoopEvent::Input(TimedEvent {
                event: CEvent::Key(key_event(spec)),
                at: Instant::now(),
            })])
            .unwrap();
    }

    fn keys(&mut self, specs: &str) {
        for s in specs.split_whitespace() {
            self.key(s);
        }
    }

    fn call(&self, method: &str, args: Value) -> Value {
        self.server.call(method, args).unwrap()
    }

    fn mailbox(&self, role_or_path: &str) -> String {
        self.server.mailbox_id(role_or_path).unwrap()
    }

    fn email(&self, id: &str) -> Value {
        self.call(
            "Email/get",
            json!({"ids": [id], "properties": ["mailboxIds", "keywords"]}),
        )["list"][0]
            .clone()
    }

    fn in_mailbox(&self, id: &str, role_or_path: &str) -> bool {
        self.email(id)["mailboxIds"]
            .get(self.mailbox(role_or_path))
            .is_some_and(|v| v == &json!(true))
    }

    fn keyword(&self, id: &str, keyword: &str) -> bool {
        self.email(id)["keywords"].get(keyword) == Some(&json!(true))
    }

    /// The server state the tests care about: Inbox, Receipts, read, pinned.
    fn state(&self, id: &str) -> [bool; 4] {
        [
            self.in_mailbox(id, "inbox"),
            self.in_mailbox(id, "Receipts"),
            self.keyword(id, "$seen"),
            self.keyword(id, "$flagged"),
        ]
    }

    fn states(&self, ids: &[String]) -> Vec<[bool; 4]> {
        ids.iter().map(|id| self.state(id)).collect()
    }

    /// The emails' server state, after checking each starts unread and
    /// pinned in the Inbox.
    fn start_states(&self, ids: &[String]) -> Vec<[bool; 4]> {
        let states = self.states(ids);
        for (id, state) in ids.iter().zip(&states) {
            assert!(starts_unread_and_pinned(*state), "{id} {}", self.email(id));
        }
        states
    }

    fn cursor_thread(&self) -> ThreadId {
        self.app
            .state()
            .list()
            .cursor_thread()
            .expect("a cursor row")
            .clone()
    }

    /// The conversation's messages that are in the Inbox on the server.
    fn inbox_emails(&self, thread: &ThreadId) -> Vec<String> {
        let conv = self.app.data().conversation(thread).expect("cached");
        let ids: Vec<String> = conv
            .messages
            .iter()
            .map(|m| m.email_id.as_str().to_owned())
            .filter(|e| self.in_mailbox(e, "inbox"))
            .collect();
        assert!(!ids.is_empty(), "the conversation has Inbox mail");
        ids
    }

    fn inbox_index(&self, thread: &ThreadId) -> Option<usize> {
        let inbox = self
            .app
            .data()
            .mailboxes()
            .into_iter()
            .find(|m| m.role == Some(mejl_ui::MailboxRole::Inbox))
            .expect("an Inbox")
            .id;
        self.app.data().conversation_index(&inbox, thread)
    }

    fn undo_label(&self) -> Option<String> {
        self.app.data().reader().undo_peek().unwrap()
    }

    /// Entries waiting in the outbox or being sent.
    fn outbox_pending(&self) -> u64 {
        self.app
            .data()
            .reader()
            .outbox_summary(Timestamp::now())
            .unwrap()
            .pending
    }

    /// Presses `key` with the network slowed down, so the queued entry can
    /// be counted before it is sent, and checks the macro made exactly one
    /// outbox entry and one undo entry labelled `label`.
    fn run_once(&mut self, key: &str, label: &str) {
        assert_eq!(self.outbox_pending(), 0, "nothing queued before");
        self.delay_ms.store(1500, Ordering::SeqCst);
        self.key(key);
        self.wait("the undo entry", |hs| {
            hs.undo_label().as_deref() == Some(label)
        });
        assert_eq!(self.outbox_pending(), 1, "one outbox entry");
        self.delay_ms.store(0, Ordering::SeqCst);
    }

    fn quit(mut self) {
        for _ in 0..6 {
            if *self.app.state().screen() == Screen::Mail && self.app.state().overlays().is_empty()
            {
                break;
            }
            self.key("esc");
        }
        self.key("Q");
        assert!(self.app.quitting());
        let handle = self.app.close();
        assert!(handle.shutdown(), "the engine stops in time");
    }
}

/// Marks every Inbox message unread and pinned on the server.
fn unread_and_pinned(server: &FakeServer) {
    let inbox = server.mailbox_id("inbox").unwrap();
    let ids = server
        .call("Email/query", json!({"filter": {"inMailbox": inbox}}))
        .unwrap()["ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let update: serde_json::Map<String, Value> = ids
        .iter()
        .filter_map(Value::as_str)
        .map(|id| {
            (
                id.to_owned(),
                json!({"keywords/$seen": null, "keywords/$flagged": true}),
            )
        })
        .collect();
    assert!(!update.is_empty());
    server
        .call("Email/set", json!({ "update": update }))
        .unwrap();
}

fn key_event(spec: &str) -> KeyEvent {
    let code = match spec {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap_or(' ');
            if c.is_ascii_uppercase() {
                return KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
            }
            KeyCode::Char(c)
        }
        s => match s.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
            Some(n) => KeyCode::F(n),
            None => panic!("unknown key {s}"),
        },
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Unread and pinned in the Inbox, as [`unread_and_pinned`] left it. Some
/// demo mail is also in Receipts already, so that part is not checked.
fn starts_unread_and_pinned(state: [bool; 4]) -> bool {
    state[0] && !state[2] && state[3]
}

const FILED: [bool; 4] = [false, true, true, false];

#[test]
fn a_macro_on_the_cursor_is_one_change_and_one_undo_takes_it_all_back() {
    let mut hs = Harness::start(config(file_receipt(FILE_RECEIPT), "fastmail"));
    let thread = hs.cursor_thread();
    let emails = hs.inbox_emails(&thread);
    let before = hs.start_states(&emails);
    assert_eq!(hs.undo_label(), None);
    let frames = hs.app.frames();
    hs.run_once("F2", "Run File receipt on 1 conversation");
    // The row left the Inbox in the frame drawn for the key.
    assert!(hs.app.frames() > frames);
    assert_eq!(hs.inbox_index(&thread), None);
    assert!(hs.screen().contains("File receipt: 1 conversation"));
    hs.wait("the macro on the server", |hs| {
        emails.iter().all(|e| hs.state(e) == FILED)
    });
    // One z restores read state, pin and folder together.
    hs.key("z");
    hs.wait("the undo on the server", |hs| hs.states(&emails) == before);
    hs.wait("the row back in the Inbox", |hs| {
        hs.inbox_index(&thread).is_some()
    });
    assert_eq!(hs.undo_label(), None, "the macro was one undo entry");
    hs.quit();
}

#[test]
fn a_macro_on_three_selected_conversations_is_one_change_and_one_undo() {
    let mut hs = Harness::start(config(file_receipt(FILE_RECEIPT), "fastmail"));
    let mut threads = Vec::new();
    for i in 0..3 {
        if i > 0 {
            hs.key("j");
        }
        threads.push(hs.cursor_thread());
        hs.key("x");
    }
    let emails: Vec<String> = threads.iter().flat_map(|t| hs.inbox_emails(t)).collect();
    let before = hs.start_states(&emails);
    hs.run_once("F2", "Run File receipt on 3 conversations");
    assert!(hs.screen().contains("File receipt: 3 conversations"));
    for t in &threads {
        assert_eq!(hs.inbox_index(t), None, "{t:?} left the Inbox");
    }
    assert!(hs.app.state().list().selection().is_empty(0));
    hs.wait("the macro on the server", |hs| {
        emails.iter().all(|e| hs.state(e) == FILED)
    });
    hs.key("z");
    hs.wait("the undo on the server", |hs| hs.states(&emails) == before);
    assert_eq!(hs.undo_label(), None);
    hs.quit();
}

#[test]
fn a_macro_after_select_all_uses_the_bulk_path() {
    let mut hs = Harness::start(config(file_receipt(FILE_RECEIPT), "fastmail"));
    let inbox = hs.mailbox("inbox");
    let emails: Vec<String> =
        hs.call("Email/query", json!({"filter": {"inMailbox": inbox}}))["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
    assert!(emails.len() > 3);
    let before = hs.start_states(&emails);
    hs.keys("* a");
    // Only the bulk path names the whole mailbox.
    hs.run_once("F2", "Run File receipt on the whole mailbox");
    let inbox_view = hs
        .app
        .data()
        .mailboxes()
        .into_iter()
        .find(|m| m.role == Some(mejl_ui::MailboxRole::Inbox))
        .unwrap();
    assert_eq!(
        hs.app.data().conversation_page(&inbox_view.id, 0, 10).total,
        0,
        "the whole list left in the frame of the key"
    );
    hs.wait("everything filed on the server", |hs| {
        emails.iter().all(|e| hs.state(e) == FILED)
    });
    hs.key("z");
    hs.wait("the undo on the server", |hs| hs.states(&emails) == before);
    hs.quit();
}

#[test]
fn a_missing_folder_says_so_and_changes_nothing() {
    let macros = file_receipt(r#"["mark-read", "unpin", { move = "Receipts/2026" }]"#);
    let mut hs = Harness::start(config(macros, "fastmail"));
    let thread = hs.cursor_thread();
    let emails = hs.inbox_emails(&thread);
    let before = hs.start_states(&emails);
    hs.key("F2");
    assert!(
        hs.screen()
            .contains("File receipt: no folder Receipts/2026"),
        "{}",
        hs.screen()
    );
    assert!(hs.inbox_index(&thread).is_some());
    hs.idle(Duration::from_millis(500));
    assert_eq!(hs.undo_label(), None);
    assert_eq!(hs.outbox_pending(), 0);
    assert_eq!(hs.states(&emails), before, "nothing changed");
    hs.quit();
}

#[test]
fn repeat_replays_a_macro_in_the_vim_preset() {
    let mut hs = Harness::start(config(file_receipt(FILE_RECEIPT), "vim"));
    let first = hs.cursor_thread();
    let first_emails = hs.inbox_emails(&first);
    hs.key("F2");
    hs.wait("the first run on the server", |hs| {
        first_emails.iter().all(|e| hs.state(e) == FILED)
    });
    // The filed row left; the cursor is on the next conversation.
    let second = hs.cursor_thread();
    assert_ne!(second, first);
    let second_emails = hs.inbox_emails(&second);
    let second_before = hs.start_states(&second_emails);
    hs.key(".");
    assert!(hs.screen().contains("File receipt: 1 conversation"));
    assert_eq!(hs.inbox_index(&second), None);
    hs.wait("the repeat on the server", |hs| {
        second_emails.iter().all(|e| hs.state(e) == FILED)
    });
    // Each run is its own undo entry; vim undoes with u.
    hs.key("u");
    hs.wait("the repeat undone", |hs| {
        hs.states(&second_emails) == second_before
    });
    assert!(first_emails.iter().all(|e| hs.state(e) == FILED));
    hs.quit();
}

#[test]
fn the_archive_step_of_a_macro_follows_mark_read_on_archive() {
    for on in [false, true] {
        let mut c = config(file_receipt(r#"["unpin", "archive"]"#), "fastmail");
        c.ui.mark_read_on_archive = on;
        let mut hs = Harness::start(c);
        let thread = hs.cursor_thread();
        let emails = hs.inbox_emails(&thread);
        let before = hs.start_states(&emails);
        hs.run_once("F2", "Run File receipt on 1 conversation");
        hs.wait("the archive on the server", |hs| {
            emails
                .iter()
                .all(|e| hs.in_mailbox(e, "archive") && !hs.in_mailbox(e, "inbox"))
        });
        for e in &emails {
            assert_eq!(hs.keyword(e, "$seen"), on, "{e} read with the setting {on}");
            assert!(!hs.keyword(e, "$flagged"), "{e} unpinned");
        }
        hs.key("z");
        hs.wait("the undo on the server", |hs| hs.states(&emails) == before);
        hs.quit();
    }
}
