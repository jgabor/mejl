//! The whole app, headless: the real loop, the real engine and store, and
//! mejl-fake in process. Keys go in as terminal events; assertions read
//! the drawn screen and the fake server's state. Nothing here touches the
//! network, a keyring or the user's directories.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use mejl::app::host::RecordingHost;
use mejl::app::settings::Settings;
use mejl::app::{App, AppEvent, AppParts};
use mejl::config::Config;
use mejl::events::{self, LoopEvent, LoopReceiver, TimedEvent};
use mejl_fake::{Address, AttachmentSpec, ExternalChange, FakeConfig, FakeServer, IncomingMessage};
use mejl_jmap::transport::{
    BoxFuture, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
};
use mejl_jmap::{Client, Secret};
use mejl_store::{Store, StoreOptions};
use mejl_sync::{Engine, EngineConfig};
use mejl_ui::{MailData, Screen};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
const UNDO_SEND: Duration = Duration::from_millis(1500);

struct Harness {
    app: App<TestBackend>,
    rx: LoopReceiver<AppEvent>,
    server: FakeServer,
    host: RecordingHost,
    requests: Arc<AtomicUsize>,
    delay_ms: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
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

/// Counts every request the engine and the app make, and can hold each
/// one back for a while, like a slow network.
struct Counting {
    inner: Arc<dyn Transport>,
    count: Arc<AtomicUsize>,
    delay_ms: Arc<AtomicU64>,
}

impl Transport for Counting {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
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
        self.count.fetch_add(1, Ordering::SeqCst);
        self.inner.stream(req)
    }
}

/// How a harness differs from the default.
struct Setup {
    server: FakeServer,
    engine: EngineConfig,
    token: Option<String>,
    toast: Option<Duration>,
    config: Config,
    undo_send: Duration,
}

impl Setup {
    fn demo() -> Setup {
        Setup {
            server: FakeServer::demo(Timestamp::now()),
            engine: engine_config(),
            token: None,
            toast: None,
            config: Config::default(),
            undo_send: UNDO_SEND,
        }
    }
}

impl Harness {
    fn with(setup: Setup, w: u16, h: u16) -> Harness {
        let Setup {
            server,
            engine,
            token,
            toast,
            config,
            undo_send,
        } = setup;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let transport: Arc<dyn Transport> = Arc::new(Counting {
            inner: Arc::new(server.transport()),
            count: requests.clone(),
            delay_ms: delay_ms.clone(),
        });
        let token = token.unwrap_or_else(|| server.token().to_owned());
        let client = Client::new(transport, server.session_url(), Secret::new(token));
        let (tx, rx) = events::channel::<AppEvent>();
        let sink = tx.clone();
        let handle = Engine::start(engine, client.clone(), store, move |e| {
            let _ = sink.msg(AppEvent::Sync(e));
        })
        .unwrap();
        let settings = Settings::new(dir.path().join("config.toml"), config, false, TimeZone::UTC);
        let (mut ui, _) = settings.ui_config().unwrap();
        ui.undo_send = undo_send;
        if let Some(t) = toast {
            ui.toast_time = t;
        }
        let host = RecordingHost::default();
        let mut app = App::new(AppParts {
            terminal: Terminal::new(TestBackend::new(w, h)).unwrap(),
            host: Box::new(host.clone()),
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
        Harness {
            app,
            rx,
            server,
            host,
            requests,
            delay_ms,
            _dir: dir,
        }
    }

    /// The app after the first sync, showing the Inbox.
    fn synced(w: u16, h: u16) -> Harness {
        Harness::synced_with(Setup::demo(), w, h)
    }

    /// [`Harness::synced`] with a different setup.
    fn synced_with(setup: Setup, w: u16, h: u16) -> Harness {
        let mut hs = Harness::with(setup, w, h);
        hs.wait_text("the inbox", "Re: Garden club plot rota");
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
            let step = (now + Duration::from_millis(50)).min(deadline);
            let batch = self.rx.recv_batch_until(step);
            if !batch.is_empty() {
                self.app.handle_batch(batch).unwrap();
            }
        }
    }

    fn wait_text(&mut self, what: &str, text: &str) {
        self.wait(what, |hs| hs.screen().contains(text));
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

    /// One key as one turn of the loop, with nothing else in the batch, so
    /// what is drawn after it is the same frame as the key.
    fn key(&mut self, spec: &str) {
        let ev = key_event(spec);
        self.app
            .handle_batch(vec![LoopEvent::Input(TimedEvent {
                event: CEvent::Key(ev),
                at: Instant::now(),
            })])
            .unwrap();
    }

    fn keys(&mut self, specs: &str) {
        for s in specs.split_whitespace() {
            self.key(s);
        }
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let spec = if c == ' ' {
                "space".to_owned()
            } else {
                c.to_string()
            };
            self.key(&spec);
        }
    }

    fn call(&self, method: &str, args: Value) -> Value {
        self.server.call(method, args).unwrap()
    }

    fn mailbox(&self, role: &str) -> String {
        self.server.mailbox_id(role).unwrap()
    }

    fn emails_in(&self, role: &str, subject: &str) -> Vec<Value> {
        let found = self.call(
            "Email/query",
            json!({"filter": {"inMailbox": self.mailbox(role), "subject": subject}}),
        );
        let ids = found["ids"].clone();
        self.call(
            "Email/get",
            json!({"ids": ids, "properties": ["subject", "mailboxIds", "keywords"]}),
        )["list"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The emails of the conversation under the cursor.
    fn cursor_emails(&self) -> (String, Vec<String>) {
        let thread = self
            .app
            .state()
            .list()
            .cursor_thread()
            .expect("a cursor row")
            .clone();
        let conv = self.app.data().conversation(&thread).expect("cached");
        (
            conv.subject.clone(),
            conv.messages
                .iter()
                .map(|m| m.email_id.as_str().to_owned())
                .collect(),
        )
    }

    fn in_mailbox(&self, email: &str, role: &str) -> bool {
        let got = self.call(
            "Email/get",
            json!({"ids": [email], "properties": ["mailboxIds"]}),
        );
        got["list"][0]["mailboxIds"]
            .get(self.mailbox(role))
            .is_some_and(|v| v == &json!(true))
    }

    fn keywords(&self, email: &str) -> Value {
        let got = self.call(
            "Email/get",
            json!({"ids": [email], "properties": ["keywords"]}),
        );
        got["list"][0]["keywords"].clone()
    }

    fn seen_on_server(&self, email: &str) -> bool {
        self.keywords(email).get("$seen") == Some(&json!(true))
    }

    /// The mailbox with a role, as the UI reads it now, pending changes
    /// included.
    fn mailbox_view(&self, role: mejl_ui::MailboxRole) -> mejl_ui::MailboxView {
        self.app
            .data()
            .mailboxes()
            .into_iter()
            .find(|m| m.role == Some(role))
            .expect("a mailbox with the role")
    }

    fn inbox_id(&self) -> mejl_ui::MailboxId {
        self.mailbox_view(mejl_ui::MailboxRole::Inbox).id
    }

    /// Where a conversation sits in the Inbox list, as the UI reads it now.
    fn inbox_index(&self, thread: &mejl_ui::ThreadId) -> Option<usize> {
        self.app.data().conversation_index(&self.inbox_id(), thread)
    }

    /// Moves the cursor to the first unread conversation of the Inbox and
    /// returns it with its unread messages there.
    fn go_to_first_unread(&mut self) -> (mejl_ui::ThreadId, Vec<String>) {
        let inbox = self.inbox_id();
        let page = self.app.data().conversation_page(&inbox, 0, 200);
        let (index, row) = page
            .items
            .iter()
            .enumerate()
            .find(|(_, r)| r.unread)
            .map(|(i, r)| (i, r.clone()))
            .expect("an unread conversation in the demo Inbox");
        self.keys("g g");
        for _ in 0..index {
            self.key("j");
        }
        assert_eq!(
            self.app.state().list().cursor_thread(),
            Some(&row.thread_id)
        );
        let unread = self.unread_in_inbox(&row.thread_id);
        assert!(!unread.is_empty());
        (row.thread_id, unread)
    }

    /// The conversation's unread messages in the Inbox, as the UI reads
    /// them now.
    fn unread_in_inbox(&self, thread: &mejl_ui::ThreadId) -> Vec<String> {
        let inbox = self.inbox_id();
        self.app
            .data()
            .conversation(thread)
            .expect("cached")
            .messages
            .iter()
            .filter(|m| m.unread && m.mailboxes.contains(&inbox))
            .map(|m| m.email_id.as_str().to_owned())
            .collect()
    }

    /// The messages of a conversation that are read, as the UI reads them
    /// now.
    fn read_now(&self, thread: &mejl_ui::ThreadId, emails: &[String]) -> Vec<bool> {
        let conv = self.app.data().conversation(thread).expect("cached");
        emails
            .iter()
            .map(|e| {
                conv.messages
                    .iter()
                    .find(|m| m.email_id.as_str() == e)
                    .is_some_and(|m| !m.unread)
            })
            .collect()
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

fn key_event(spec: &str) -> KeyEvent {
    let (mods, name) = match spec.strip_prefix("ctrl-") {
        Some(rest) => (KeyModifiers::CONTROL, rest),
        None => (KeyModifiers::NONE, spec),
    };
    let code = match name {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "down" => KeyCode::Down,
        "up" => KeyCode::Up,
        "backspace" => KeyCode::Backspace,
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap_or(' ');
            if c.is_ascii_uppercase() {
                return KeyEvent::new(KeyCode::Char(c), mods | KeyModifiers::SHIFT);
            }
            KeyCode::Char(c)
        }
        other => panic!("unknown key {other}"),
    };
    KeyEvent::new(code, mods)
}

#[test]
fn start_shows_the_inbox_and_the_account() {
    let mut hs = Harness::synced(160, 40);
    let screen = hs.screen();
    assert!(screen.contains("Inbox"), "{screen}");
    assert!(screen.contains("mira@example.org"), "{screen}");
    assert!(
        screen.contains("Draft budget for the hall repairs"),
        "{screen}"
    );
    hs.wait_text("push to connect", "synced via JMAP push");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.quit();
}

#[test]
fn moving_and_opening_renders_the_body() {
    let mut hs = Harness::synced(160, 40);
    let top = hs.cursor_emails().0;
    hs.key("j");
    let second = hs.cursor_emails().0;
    assert_ne!(top, second);
    hs.key("k");
    assert_eq!(hs.cursor_emails().0, top);
    // The top conversation is Juniper's rota reply; open it.
    hs.key("enter");
    hs.wait_text("the body", "Swapping Saturday works for me");
    assert!(hs.app.state().reader_full() || hs.screen().contains("Re: Garden club"));
    // An HTML body goes through the render workers.
    hs.key("u");
    hs.keys("j j");
    let (subject, _) = hs.cursor_emails();
    assert_eq!(subject, "Re: Your order 48213 has shipped");
    hs.key("enter");
    hs.wait_text("the rendered HTML body", "Track your parcel");
    hs.quit();
}

#[test]
fn archive_shows_in_the_same_frame_and_reaches_the_server_and_undo_restores_it() {
    let mut hs = Harness::synced(160, 40);
    hs.key("j");
    let (subject, emails) = hs.cursor_emails();
    assert_eq!(subject, "Draft budget for the hall repairs");
    assert!(hs.screen().contains(&subject));
    let frames = hs.app.frames();
    hs.key("y");
    // One turn, one frame: the row is gone from that frame.
    assert_eq!(hs.app.frames(), frames + 1);
    let screen = hs.screen();
    assert!(
        !screen.contains("Draft budget for the hall repairs"),
        "the archived row is still drawn:\n{screen}"
    );
    hs.wait("the archive on the server", |hs| {
        hs.in_mailbox(&emails[0], "archive") && !hs.in_mailbox(&emails[0], "inbox")
    });
    // Undo brings it back, locally and on the server.
    hs.key("z");
    hs.wait_text("the row to come back", "Draft budget for the hall repairs");
    hs.wait("the undo on the server", |hs| {
        hs.in_mailbox(&emails[0], "inbox") && !hs.in_mailbox(&emails[0], "archive")
    });
    hs.quit();
}

fn mark_read_setup(on: bool) -> Setup {
    let mut config = Config::default();
    config.ui.mark_read_on_archive = on;
    Setup {
        config,
        ..Setup::demo()
    }
}

/// Archives the first unread Inbox conversation with `key` and undoes it.
/// With `mark_read` the archive also marks it read, in the frame drawn for
/// the key and on the server; either way undo brings it back unread.
fn archive_and_undo(hs: &mut Harness, key: &str, mark_read: bool) {
    use mejl_ui::MailboxRole;
    let (thread, unread) = hs.go_to_first_unread();
    let n = unread.len() as u32;
    let inbox = hs.mailbox_view(MailboxRole::Inbox);
    let archive = hs.mailbox_view(MailboxRole::Archive);
    let frames = hs.app.frames();
    hs.key(key);
    // All in the frame drawn for the key.
    assert_eq!(hs.app.frames(), frames + 1);
    assert_eq!(hs.inbox_index(&thread), None, "the row left the Inbox");
    let inbox_now = hs.mailbox_view(MailboxRole::Inbox);
    let archive_now = hs.mailbox_view(MailboxRole::Archive);
    assert_eq!(inbox_now.unread_emails, inbox.unread_emails - n);
    let added = if mark_read { 0 } else { n };
    assert_eq!(
        archive_now.unread_emails,
        archive.unread_emails + added,
        "Archive's unread count"
    );
    assert_eq!(hs.read_now(&thread, &unread), vec![mark_read; unread.len()]);
    hs.wait("the archive on the server", |hs| {
        unread
            .iter()
            .all(|e| hs.in_mailbox(e, "archive") && !hs.in_mailbox(e, "inbox"))
    });
    for e in &unread {
        assert_eq!(hs.seen_on_server(e), mark_read, "{e}");
    }
    hs.key("z");
    hs.wait("the row back in the Inbox", |hs| {
        hs.inbox_index(&thread).is_some()
    });
    hs.wait("the undo on the server", |hs| {
        unread.iter().all(|e| hs.in_mailbox(e, "inbox"))
    });
    for e in &unread {
        assert!(!hs.seen_on_server(e), "{e} is unread again");
    }
    hs.wait("the row unread again", |hs| {
        hs.read_now(&thread, &unread).iter().all(|r| !r)
    });
}

#[test]
fn archive_marks_read_only_with_the_setting_and_undo_brings_it_back_unread() {
    for on in [false, true] {
        let mut hs = Harness::synced_with(mark_read_setup(on), 160, 40);
        archive_and_undo(&mut hs, "y", on);
        hs.quit();
    }
}

#[test]
fn archive_next_follows_the_mark_read_setting() {
    for on in [false, true] {
        let mut hs = Harness::synced_with(mark_read_setup(on), 160, 40);
        archive_and_undo(&mut hs, "]", on);
        hs.quit();
    }
}

#[test]
fn archiving_the_whole_mailbox_marks_it_read_and_undo_restores_unread() {
    let mut hs = Harness::synced_with(mark_read_setup(true), 160, 40);
    let inbox = hs.inbox_id();
    let unread: Vec<String> = hs.call(
        "Email/query",
        json!({"filter": {"inMailbox": hs.mailbox("inbox"), "notKeyword": "$seen"}}),
    )["ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(!unread.is_empty());
    let archive = hs.mailbox_view(mejl_ui::MailboxRole::Archive);
    hs.keys("* a y");
    // The whole list is gone in the frame drawn for the key, and the
    // counts moved with it.
    assert_eq!(hs.app.data().conversation_page(&inbox, 0, 10).total, 0);
    let inbox_now = hs.mailbox_view(mejl_ui::MailboxRole::Inbox);
    assert_eq!((inbox_now.total_threads, inbox_now.unread_emails), (0, 0));
    let archive_now = hs.mailbox_view(mejl_ui::MailboxRole::Archive);
    assert_eq!(archive_now.unread_emails, archive.unread_emails);
    hs.wait("everything archived and read on the server", |hs| {
        unread
            .iter()
            .all(|e| hs.in_mailbox(e, "archive") && hs.seen_on_server(e))
    });
    hs.key("z");
    hs.wait("the undo on the server", |hs| {
        unread
            .iter()
            .all(|e| hs.in_mailbox(e, "inbox") && !hs.seen_on_server(e))
    });
    hs.wait("the rows back, unread", |hs| {
        hs.mailbox_view(mejl_ui::MailboxRole::Inbox).unread_emails as usize == unread.len()
    });
    hs.quit();
}

#[test]
fn toggling_the_setting_applies_to_the_next_archive_without_a_restart() {
    let mut hs = Harness::synced(160, 40);
    hs.keys("g ,");
    assert_eq!(*hs.app.state().screen(), Screen::Settings);
    hs.wait_text("the settings row", "Archive reads");
    // The last row; one step turns it on.
    hs.keys("G enter");
    assert!(hs.app.state().config().mark_read_on_archive);
    hs.key("esc");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    archive_and_undo(&mut hs, "y", true);
    hs.quit();
}

fn compose(hs: &mut Harness, to: &str, subject: &str, body: &str) {
    hs.key("c");
    assert_eq!(*hs.app.state().screen(), Screen::Compose);
    hs.type_text(to);
    hs.keys("tab tab");
    hs.type_text(subject);
    hs.key("tab");
    hs.type_text(body);
}

#[test]
fn compose_and_send_lands_in_sent_after_the_undo_window() {
    let mut hs = Harness::synced(120, 36);
    compose(
        &mut hs,
        "ada@example.com",
        "Shed key handover",
        "I will leave it with Noor.",
    );
    hs.keys("ctrl-o s");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.wait_text("the countdown", "Sending in");
    // Held: nothing on the server yet.
    assert!(hs.emails_in("sent", "Shed key handover").is_empty());
    hs.wait("the message in Sent", |hs| {
        !hs.emails_in("sent", "Shed key handover").is_empty()
    });
    let sent = hs.emails_in("sent", "Shed key handover");
    assert!(sent[0]["keywords"].get("$draft").is_none(), "{sent:?}");
    hs.quit();
}

/// The ids of the server's copies of a draft, by subject.
fn draft_copies(hs: &Harness, subject: &str) -> Vec<String> {
    hs.emails_in("drafts", subject)
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .collect()
}

/// An email's preview on the server.
fn preview(hs: &Harness, id: &str) -> String {
    hs.call("Email/get", json!({"ids": [id], "properties": ["preview"]}))["list"][0]["preview"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Every email submission on the server.
fn submissions(hs: &Harness) -> usize {
    hs.call("EmailSubmission/get", json!({"ids": null}))["list"]
        .as_array()
        .map_or(0, Vec::len)
}

/// Waits until the app and the engine have no draft copy left to upload.
fn wait_drafts_settled(hs: &mut Harness) {
    hs.wait("the draft copies to settle", |hs| {
        hs.app.drafts_pending() == 0
    });
}

#[test]
fn cancel_send_inside_the_window_never_reaches_the_server() {
    let mut hs = Harness::synced(120, 36);
    let submitted = submissions(&hs);
    compose(
        &mut hs,
        "ada@example.com",
        "Never mind the rota",
        "Ignore my last note.",
    );
    // The autosave puts the draft's own copy in Drafts first.
    hs.wait("the draft's copy", |hs| {
        draft_copies(hs, "Never mind the rota").len() == 1
    });
    hs.keys("ctrl-o s");
    assert_eq!(
        *hs.app.state().screen(),
        Screen::Mail,
        "the send was accepted"
    );
    // Undo at once, before the engine has even queued it.
    hs.key("z");
    hs.wait("compose to come back", |hs| {
        *hs.app.state().screen() == Screen::Compose
    });
    assert!(
        hs.screen().contains("Never mind the rota"),
        "{}",
        hs.screen()
    );
    // Well past the window: never submitted, nothing in Sent, and Drafts
    // holds only the draft's own copy, no extra one from the send.
    hs.idle(UNDO_SEND + Duration::from_secs(2));
    wait_drafts_settled(&mut hs);
    assert!(hs.emails_in("sent", "Never mind the rota").is_empty());
    assert_eq!(submissions(&hs), submitted, "nothing was submitted");
    let drafts = hs.emails_in("drafts", "Never mind the rota");
    // Cancelling saves the draft again, so the copy may be a newer one, but
    // never a second one.
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0]["keywords"]["$draft"], json!(true));
    hs.quit();
}

#[test]
fn closing_compose_after_a_cancelled_send_keeps_the_text() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Hall rota", "Swap Saturday?");
    // Send and undo before any autosave could have run.
    hs.keys("ctrl-o s");
    hs.key("z");
    hs.wait("compose to come back", |hs| {
        *hs.app.state().screen() == Screen::Compose
    });
    hs.keys("esc esc");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.wait("the copy in Drafts", |hs| {
        draft_copies(hs, "Hall rota").len() == 1
    });
    wait_drafts_settled(&mut hs);
    let drafts = hs.emails_in("drafts", "Hall rota");
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    let id = drafts[0]["id"].as_str().unwrap_or_default().to_owned();
    assert!(preview(&hs, &id).contains("Swap Saturday?"));
    hs.quit();
}

#[test]
fn closing_compose_leaves_one_copy_in_drafts() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Hall keys", "Back on Friday.");
    hs.keys("esc esc");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.wait("the copy in Drafts", |hs| {
        draft_copies(hs, "Hall keys").len() == 1
    });
    wait_drafts_settled(&mut hs);
    hs.idle(Duration::from_millis(300));
    let drafts = hs.emails_in("drafts", "Hall keys");
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    assert_eq!(drafts[0]["keywords"]["$draft"], json!(true));
    let id = drafts[0]["id"].as_str().unwrap_or_default().to_owned();
    assert!(preview(&hs, &id).contains("Back on Friday."));
    hs.quit();
}

#[test]
fn autosaves_copy_to_the_server_at_most_once_every_ten_seconds() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Plot rota swap", "First draft.");
    hs.wait("the first copy", |hs| {
        draft_copies(hs, "Plot rota swap").len() == 1
    });
    let start = Instant::now();
    let first = draft_copies(&hs, "Plot rota swap");
    // Two more autosaves inside the window go nowhere yet.
    for more in [" Second.", " Third."] {
        hs.type_text(more);
        hs.idle(Duration::from_millis(2600));
        assert!(
            start.elapsed() < Duration::from_secs(9),
            "the test is too slow"
        );
        assert_eq!(draft_copies(&hs, "Plot rota swap"), first);
    }
    let screen = hs.screen();
    assert!(screen.contains("Draft saved"), "{screen}");
    assert!(screen.contains("(saving to Drafts)"), "{screen}");
    // Once the window is over the newest version goes up, compose open.
    hs.wait("the second copy", |hs| {
        let now = draft_copies(hs, "Plot rota swap");
        now.len() == 1 && now != first
    });
    assert!(
        start.elapsed() >= Duration::from_secs(9),
        "uploaded after {:?}",
        start.elapsed()
    );
    assert_eq!(*hs.app.state().screen(), Screen::Compose);
    let second = draft_copies(&hs, "Plot rota swap");
    assert!(preview(&hs, &second[0]).contains("Third."));
    wait_drafts_settled(&mut hs);
    hs.wait_text("the status", "(Drafts)");
    // Closing right after copies at once, without waiting.
    hs.type_text(" Fourth.");
    hs.keys("esc esc");
    hs.wait("the closing copy", |hs| {
        let now = draft_copies(hs, "Plot rota swap");
        now.len() == 1 && now != second
    });
    assert!(start.elapsed() < Duration::from_secs(19));
    let last = draft_copies(&hs, "Plot rota swap");
    assert!(preview(&hs, &last[0]).contains("Fourth."));
    hs.quit();
}

#[test]
fn the_save_key_and_losing_focus_copy_at_once() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Tool shed lock", "New code.");
    hs.wait("the first copy", |hs| {
        draft_copies(hs, "Tool shed lock").len() == 1
    });
    let start = Instant::now();
    let first = draft_copies(&hs, "Tool shed lock");
    hs.type_text(" Saved.");
    hs.key("ctrl-s");
    hs.wait("the saved copy", |hs| {
        let now = draft_copies(hs, "Tool shed lock");
        now.len() == 1 && now != first
    });
    assert!(start.elapsed() < Duration::from_secs(5), "not held back");
    let saved = draft_copies(&hs, "Tool shed lock");
    assert!(preview(&hs, &saved[0]).contains("Saved."));
    // An autosave right after waits for the window...
    hs.type_text(" Away.");
    hs.idle(Duration::from_millis(2600));
    assert_eq!(draft_copies(&hs, "Tool shed lock"), saved);
    assert!(
        hs.screen().contains("(saving to Drafts)"),
        "{}",
        hs.screen()
    );
    // ...unless the terminal loses focus.
    hs.app
        .handle_batch(vec![LoopEvent::Input(TimedEvent {
            event: CEvent::FocusLost,
            at: Instant::now(),
        })])
        .unwrap();
    hs.wait("the copy on blur", |hs| {
        let now = draft_copies(hs, "Tool shed lock");
        now.len() == 1 && now != saved
    });
    assert!(start.elapsed() < Duration::from_secs(9), "not held back");
    let blurred = draft_copies(&hs, "Tool shed lock");
    assert!(preview(&hs, &blurred[0]).contains("Away."));
    hs.quit();
}

#[test]
fn discarding_a_draft_removes_its_server_copy() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Compost order", "Two bags.");
    hs.wait("the copy in Drafts", |hs| {
        draft_copies(hs, "Compost order").len() == 1
    });
    hs.keys("ctrl-o d y");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.wait("the copy to go", |hs| {
        draft_copies(hs, "Compost order").is_empty()
    });
    wait_drafts_settled(&mut hs);
    assert!(hs.app.data().drafts().is_empty());
    hs.quit();
}

#[test]
fn without_a_drafts_folder_compose_says_the_draft_stays_on_this_device() {
    let server = FakeServer::demo(Timestamp::now());
    let drafts = server.mailbox_id("drafts").expect("a Drafts mailbox");
    server
        .mutate_from_elsewhere(ExternalChange::ClearMailboxRole {
            mailbox_id: drafts.clone(),
        })
        .unwrap();
    let mut hs = Harness::synced_with(
        Setup {
            server,
            ..Setup::demo()
        },
        120,
        36,
    );
    compose(&mut hs, "ada@example.com", "Seed swap", "Bring beans.");
    hs.wait_text(
        "the on-device status",
        "(on this device: the account has no Drafts folder)",
    );
    assert!(
        hs.screen().contains("draft on this device"),
        "{}",
        hs.screen()
    );
    hs.keys("esc esc");
    hs.idle(Duration::from_millis(500));
    let found = hs.call(
        "Email/query",
        json!({"filter": {"inMailbox": drafts, "subject": "Seed swap"}}),
    );
    assert_eq!(found["ids"], json!([]), "nothing uploaded");
    assert_eq!(hs.app.data().drafts().len(), 1, "kept on this device");
    hs.quit();
}

#[test]
fn local_search_finds_a_demo_message() {
    let mut hs = Harness::synced(120, 36);
    hs.key("/");
    assert_eq!(*hs.app.state().screen(), Screen::Search);
    hs.type_text("lichens");
    hs.wait_text("a local hit", "Your reservation is ready to collect");
    hs.quit();
}

#[test]
fn new_mail_arrives_through_push() {
    let mut hs = Harness::synced(160, 40);
    hs.wait_text("push to connect", "synced via JMAP push");
    hs.server
        .deliver_incoming(
            IncomingMessage::new(
                Address::new("Harbour Library", "notices@library.example.org"),
                "Your interlibrary loan has arrived",
            )
            .text("It is at the front desk."),
        )
        .unwrap();
    hs.wait_text("the pushed message", "Your interlibrary loan has arrived");
    hs.quit();
}

fn disabled_on_server(hs: &Harness) -> Vec<String> {
    let all = hs.call("MaskedEmail/get", json!({"ids": null}));
    all["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["state"] == "disabled")
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn masked_email_screen_lists_masks_and_disable_round_trips() {
    let mut hs = Harness::synced(120, 36);
    hs.keys("g m");
    assert_eq!(*hs.app.state().screen(), Screen::Masked);
    hs.wait("masks cached", |hs| {
        hs.app.data().masked_emails().len() == 20
    });
    let screen = hs.screen();
    let shown: Vec<String> = hs
        .app
        .data()
        .masked_emails()
        .iter()
        .filter(|m| screen.contains(m.email.as_str()))
        .map(|m| m.email.clone())
        .collect();
    assert!(!shown.is_empty(), "{screen}");
    let before = disabled_on_server(&hs);
    // The cursor starts on an enabled mask; x disables it after a y/N.
    hs.keys("x y");
    hs.wait("a mask disabled on the server", |hs| {
        disabled_on_server(hs).len() == before.len() + 1
    });
    let now_disabled: Vec<String> = disabled_on_server(&hs)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    let id = &now_disabled[0];
    hs.wait("the cache to agree", |hs| {
        hs.app
            .data()
            .masked_emails()
            .iter()
            .any(|m| &m.id == id && m.state == mejl_ui::MaskedState::Disabled)
    });
    hs.quit();
}

#[test]
fn rules_screen_renders_a_sieve_block() {
    use mejl_sieve::{Action, Condition, Rule, Test, TextOp};
    let mut hs = Harness::synced(160, 40);
    let mut rule = Rule::new("r_receipts", "Receipts to folder");
    rule.conditions.push(Condition::new(Test::Subject {
        op: TextOp::Contains,
        value: "receipt".into(),
    }));
    rule.actions.push(Action::MarkRead);
    hs.app.handle().send(mejl_sync::Command::Local {
        token: 9_000,
        write: mejl_sync::LocalWrite::SaveRule(mejl_store::RuleRecord {
            id: "r_receipts".into(),
            name: rule.name.clone(),
            enabled: true,
            sort_order: 0,
            rule_json: serde_json::to_string(&rule).unwrap(),
            updated_at: Timestamp::now(),
        }),
    });
    hs.wait("the rule in the store", |hs| {
        hs.app.data().rules().len() == 1
    });
    hs.keys("g r");
    assert_eq!(*hs.app.state().screen(), Screen::Rules);
    hs.wait_text("the block", "mejl managed block");
    let screen = hs.screen();
    assert!(screen.contains("Receipts to folder"), "{screen}");
    assert!(
        screen.contains("header :contains \"subject\" \"receipt\""),
        "{screen}"
    );
    hs.quit();
}

#[test]
fn narrow_terminal_shows_one_pane() {
    let hs = Harness::synced(80, 24);
    let screen = hs.screen();
    assert!(screen.contains("Inbox"), "{screen}");
    assert!(!screen.contains("Mailboxes"), "{screen}");
    hs.quit();
}

#[test]
fn scrolling_past_the_cached_window_loads_more() {
    let server = FakeServer::synthetic(3000, 7, Timestamp::now());
    let engine = EngineConfig {
        window: 40,
        load_more: 40,
        ..engine_config()
    };
    let mut hs = Harness::with(
        Setup {
            server,
            engine,
            ..Setup::demo()
        },
        120,
        36,
    );
    hs.wait("the first sync", |hs| hs.screen().contains("synced"));
    hs.idle(Duration::from_millis(300));
    let first = hs.app.state().list().total();
    assert!((40..400).contains(&first), "{first}");
    let server_total = hs.server.call("Email/query", json!({"filter": {"inMailbox": hs.mailbox("inbox")}, "collapseThreads": true, "calculateTotal": true})).unwrap_or_default();
    // The server has far more than the first window holds.
    assert!(
        server_total["total"].as_u64().unwrap_or(0) > (first as u64) + 40,
        "{server_total}"
    );
    // Jump to the end of what is cached, again and again: each time the
    // read model sees the end and asks the engine for the next window.
    for _ in 0..3 {
        hs.key("G");
        hs.idle(Duration::from_millis(200));
    }
    // At least most of one more window: thread expansion alone adds a
    // few rows at most.
    hs.wait("a bigger window", |hs| {
        hs.app.state().list().total() >= first + 30
    });
    hs.quit();
}

#[test]
fn key_to_frame_latency_through_the_real_loop() {
    let mut hs = Harness::synced(160, 40);
    for i in 0..200 {
        hs.key(if (i / 10) % 2 == 0 { "j" } else { "k" });
    }
    // The last sample is fed at the start of the next turn.
    hs.idle(Duration::from_millis(50));
    let p = hs.app.state().perf().summary();
    eprintln!(
        "input to frame over {} keys at 160x40 (TestBackend): p50 {:?}, p95 {:?}, p99 {:?}, max {:?}",
        p.count, p.p50, p.p95, p.p99, p.max
    );
    assert!(p.count >= 150, "{p:?}");
    // Generous for debug builds on a busy machine; release numbers are
    // reported, not asserted.
    assert!(p.p99 < Duration::from_millis(250), "{p:?}");
    hs.quit();
}

#[test]
fn a_refused_token_says_to_log_in_and_stops_asking() {
    let mut hs = Harness::with(
        Setup {
            token: Some("fmu1-not-the-demo-token".into()),
            toast: Some(Duration::from_millis(300)),
            ..Setup::demo()
        },
        160,
        40,
    );
    hs.wait_text("the login hint", "mejl login");
    // The toast is gone after its short time; the status line keeps it.
    hs.idle(Duration::from_millis(800));
    assert!(hs.screen().contains("mejl login"), "{}", hs.screen());
    // The engine waits for a new token instead of retrying.
    let before = hs.requests.load(Ordering::SeqCst);
    hs.idle(Duration::from_secs(2));
    assert_eq!(hs.requests.load(Ordering::SeqCst), before, "no retry loop");
    hs.quit();
}

#[test]
fn attachments_download_to_a_private_folder_and_open() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut hs = Harness::synced(120, 36);
    hs.key("j");
    assert_eq!(hs.cursor_emails().0, "Draft budget for the hall repairs");
    hs.key("enter");
    hs.wait_text("the body", "Attached the spreadsheet");
    let rect = hs
        .app
        .state()
        .hit_map()
        .regions()
        .iter()
        .find(|(_, t)| matches!(t, mejl_ui::HitTarget::Attachment { .. }))
        .map(|(r, _)| *r)
        .expect("an attachment on screen");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        hs.app
            .handle_batch(vec![LoopEvent::Input(TimedEvent {
                event: CEvent::Mouse(MouseEvent {
                    kind,
                    column: rect.x + 1,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                at: Instant::now(),
            })])
            .unwrap();
    }
    hs.wait("the download to open", |hs| {
        !hs.host.snapshot().paths.is_empty()
    });
    let path = hs.host.snapshot().paths[0].clone();
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("hall-repairs-budget.xlsx")
    );
    let folder = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(folder.starts_with("mejl-open-"), "{folder}");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 38_912);
    hs.quit();
}

#[test]
fn the_external_editor_replaces_the_body() {
    let mut hs = Harness::synced(120, 36);
    compose(&mut hs, "ada@example.com", "Editor test", "First line.");
    hs.keys("ctrl-o e");
    let log = hs.host.snapshot();
    assert_eq!(log.edited.len(), 1);
    assert!(log.edited[0].contains("First line."), "{:?}", log.edited);
    hs.wait_text("the edited body", "Edited in a test editor.");
    hs.quit();
}

#[test]
fn copying_a_masked_address_uses_the_clipboard() {
    let mut hs = Harness::synced(120, 36);
    hs.keys("g m");
    hs.wait("masks cached", |hs| {
        hs.app.data().masked_emails().len() == 20
    });
    hs.key("y");
    let copied = hs.host.snapshot().copied;
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(
        hs.app
            .data()
            .masked_emails()
            .iter()
            .any(|m| m.email == copied[0]),
        "{copied:?}"
    );
    hs.quit();
}

/// Any C0 or C1 control or DEL in a drawn cell would reach the terminal as a
/// raw byte, so mail could clear the screen, retitle the window or write the
/// clipboard through OSC 52.
fn control_chars(screen: &str) -> Vec<(usize, char)> {
    screen
        .lines()
        .enumerate()
        .flat_map(|(row, line)| {
            line.chars()
                .filter(|c| c.is_control())
                .map(move |c| (row, c))
        })
        .collect()
}

#[test]
fn hostile_mail_cannot_put_control_bytes_on_the_screen() {
    // OSC 52 clipboard write, OSC 0 title, OSC 8 link, CSI clear, a C1 CSI,
    // a C1 OSC, BEL, backspace and a bare CR.
    let evil = "\u{1b}]52;c;cm0gLXJmIH4K\u{7}\u{1b}]0;owned\u{7}\u{1b}]8;;https://x.example\u{1b}\\\
                \u{1b}[2J\u{9b}31m\u{9d}2;t\u{9c}\u{7}\u{8}\r";
    let mut hs = Harness::synced(160, 40);
    hs.wait_text("push to connect", "synced via JMAP push");
    hs.server
        .deliver_incoming(
            IncomingMessage::new(
                Address::new(
                    format!("Mallory {evil}Sender"),
                    "mallory@hostile.example.net",
                ),
                format!("Hostile plain {evil}subject"),
            )
            .text(format!(
                "First line {evil}\nSecond {evil} line\n> quoted {evil}\n"
            )),
        )
        .unwrap();
    hs.server
        .deliver_incoming(
            IncomingMessage::new(
                Address::new("Mallory Html", "mallory@hostile.example.net"),
                format!("Hostile html {evil}subject"),
            )
            .html(format!(
                "<p>Para {evil}</p><a href=\"https://x.example/{evil}\">link {evil}</a>\
                 <table><tr><td>{evil}cell</td></tr></table>"
            ))
            .attachment(AttachmentSpec::new(
                format!("report{evil}.pdf"),
                "application/pdf",
                1200,
            )),
        )
        .unwrap();
    hs.wait_text("both hostile messages", "Hostile plain");
    hs.wait_text("both hostile messages", "Hostile html");
    let list = hs.screen();
    assert!(
        control_chars(&list).is_empty(),
        "{:?}\n{list}",
        control_chars(&list)
    );

    for subject in ["Hostile html", "Hostile plain"] {
        // The newest mail is at the top; walk there and open each one.
        for _ in 0..4 {
            hs.key("k");
        }
        while !hs.cursor_emails().0.starts_with(subject) {
            hs.key("j");
        }
        hs.key("enter");
        hs.wait("the body", |hs| {
            let s = hs.screen();
            s.contains("Para") || s.contains("First line")
        });
        hs.idle(Duration::from_millis(300));
        let screen = hs.screen();
        assert!(
            control_chars(&screen).is_empty(),
            "{subject}: {:?}\n{screen}",
            control_chars(&screen)
        );
        hs.key("u");
    }
    assert!(hs.host.snapshot().copied.is_empty(), "nothing was copied");
    hs.quit();
}

/// Characters that must never reach a cell or the clipboard: C0 and C1
/// controls and DEL, and the bidi controls and invisible characters that
/// reorder or hide text.
fn unsafe_chars(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| {
            (c.is_control() && *c != '\n')
                || matches!(
                    c,
                    '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{061C}'
                        | '\u{200B}'
                        | '\u{2060}'
                        | '\u{FEFF}'
                )
        })
        .collect()
}

fn assert_safe(what: &str, text: &str) {
    let bad = unsafe_chars(text);
    assert!(bad.is_empty(), "{what}: {bad:?}\n{text}");
}

#[test]
fn hostile_mask_descriptions_cannot_reach_the_screen_or_the_clipboard() {
    // OSC 52 clipboard write, OSC 0 title, CSI clear, C1 CSI and OSC, BEL,
    // backspace, CR, a right-to-left override, an isolate and a mark.
    let evil = "\u{1b}]52;c;cm0gLXJmIH4K\u{7}\u{1b}]0;owned\u{7}\u{1b}[2J\u{9b}31m\
                \u{9d}2;t\u{9c}\u{7}\u{8}\r\u{202E}gnp.exe\u{2066}\u{200F}";
    let server = FakeServer::new(FakeConfig {
        masked_email_domain: format!("mask{evil}.example.net"),
        masked_email_created_by: format!("Tool {evil}"),
        now: Some(Timestamp::now()),
        ..FakeConfig::default()
    });
    let created = server
        .call(
            "MaskedEmail/set",
            json!({"create": {"h1": {
                "state": "enabled",
                "forDomain": format!("https://shop{evil}.example.net"),
                "description": format!("Hostile {evil}shop"),
                "url": format!("https://shop.example.net/{evil}"),
            }}}),
        )
        .unwrap();
    let mask_email = created["created"]["h1"]["email"]
        .as_str()
        .expect("the fake made an address")
        .to_owned();
    assert!(
        !unsafe_chars(&mask_email).is_empty(),
        "the address is hostile"
    );
    server
        .deliver_incoming(
            IncomingMessage::new(
                Address::new("Shop", "orders@shop.example.net"),
                "Your hostile order",
            )
            .to(Address::new(format!("Me {evil}"), mask_email.as_str()))
            .text("Thanks for the order."),
        )
        .unwrap();
    let mut hs = Harness::with(
        Setup {
            server,
            ..Setup::demo()
        },
        160,
        40,
    );
    hs.wait_text("the delivered message", "Your hostile order");
    assert_safe("the list", &hs.screen());

    // The Masked Email screen, its filter and the rename prompt.
    hs.keys("g m");
    hs.wait("the mask cached", |hs| {
        hs.app.data().masked_emails().len() == 1
    });
    hs.wait_text("the mask row", "Hostile");
    assert_safe("the masked screen", &hs.screen());
    hs.key("/");
    hs.type_text("hostile");
    assert_safe("the filtered list", &hs.screen());
    hs.keys("esc");
    hs.key("r");
    assert!(
        hs.app
            .state()
            .overlays()
            .iter()
            .any(|o| matches!(o, mejl_ui::Overlay::Prompt(_))),
        "the rename prompt is open"
    );
    assert_safe("the rename prompt", &hs.screen());
    hs.key("esc");
    hs.key("y");
    let copied = hs.host.snapshot().copied;
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert_safe("the copied address", &copied[0]);

    // The mask menu on the message delivered to it.
    hs.keys("esc esc");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
    hs.key("enter");
    hs.wait_text("the body", "Thanks for the order");
    assert_safe("the reader", &hs.screen());
    hs.keys("@ @");
    hs.wait_text("the mask menu", "Delivered to");
    assert_safe("the mask menu", &hs.screen());
    hs.type_text("copy the masked");
    hs.key("enter");
    let copied = hs.host.snapshot().copied;
    assert_eq!(copied.len(), 2, "{copied:?}");
    assert_safe("the address copied from the menu", &copied[1]);
    hs.quit();
}

impl Harness {
    /// Closes an app that is already quitting and stops the engine.
    fn close(self) -> tempfile::TempDir {
        assert!(self.app.quitting());
        let handle = self.app.close();
        assert!(handle.shutdown(), "the engine stops in time");
        self._dir
    }

    fn suspends(&self) -> usize {
        self.host.snapshot().suspends
    }

    fn picker_title(&self) -> Option<String> {
        self.app.state().overlays().last().and_then(|o| match o {
            mejl_ui::Overlay::Picker(p) => Some(p.title.clone()),
            _ => None,
        })
    }
}

#[test]
fn ctrl_z_suspends_from_every_context_and_redraws_at_the_new_size() {
    let mut hs = Harness::synced(160, 40);
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 1, "from the mail list");
    // The window got smaller while mejl was stopped: the frame drawn
    // after the resume is the whole screen at the new size.
    hs.app.terminal_mut().backend_mut().resize(120, 36);
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 2);
    assert_eq!(hs.app.state().size(), (120, 36));
    let screen = hs.screen();
    assert_eq!(screen.lines().count(), 36);
    assert!(screen.contains("Inbox"), "{screen}");

    // A picker stays open, and a pending key sequence is dropped.
    hs.key(":");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 3, "with the palette open");
    assert_eq!(hs.picker_title().as_deref(), Some("Commands"));
    hs.key("esc");
    hs.key("g");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 4, "after a prefix key");

    // Typing in the search bar.
    hs.key("/");
    hs.type_text("rota");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 5, "from the search bar");
    assert_eq!(*hs.app.state().screen(), Screen::Search);
    hs.key("esc");

    // Typing in the compose body: suspend wins over the editor's undo.
    compose(&mut hs, "ada@example.com", "Suspend test", "Body text");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 6, "from the compose body");
    assert_eq!(*hs.app.state().screen(), Screen::Compose);
    assert!(hs.screen().contains("Body text"), "{}", hs.screen());

    // A confirm stays open.
    hs.keys("ctrl-o d");
    let confirm = |hs: &Harness| {
        matches!(
            hs.app.state().overlays().last(),
            Some(mejl_ui::Overlay::Confirm(_))
        )
    };
    assert!(confirm(&hs));
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 7, "with a confirm open");
    assert!(confirm(&hs));
    hs.key("n");

    // The settings screen, in a fresh app.
    hs.keys("esc Q");
    let _ = hs.close();
    let mut hs = Harness::synced(120, 36);
    hs.keys("g ,");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 1, "from settings");
    assert_eq!(*hs.app.state().screen(), Screen::Settings);
    hs.quit();
}

#[test]
fn ctrl_z_suspends_with_the_vim_preset() {
    let mut config = Config::default();
    config.ui.preset = mejl::config::Preset::Vim;
    let mut hs = Harness::synced_with(
        Setup {
            config,
            ..Setup::demo()
        },
        120,
        36,
    );
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 1);
    // The reader's z prefix is a different key.
    hs.keys("enter z");
    hs.key("ctrl-z");
    assert_eq!(hs.suspends(), 2);
    hs.quit();
}

/// A harness whose sends wait long enough that only the quit prompt can
/// make them go out during a test.
fn long_undo_send() -> Harness {
    Harness::synced_with(
        Setup {
            undo_send: Duration::from_secs(60),
            ..Setup::demo()
        },
        120,
        36,
    )
}

fn send_one(hs: &mut Harness, subject: &str) {
    compose(hs, "ada@example.com", subject, "See you at the hall.");
    hs.keys("ctrl-o s");
    assert_eq!(*hs.app.state().screen(), Screen::Mail);
}

#[test]
fn quit_in_the_undo_window_asks_and_enter_sends_now() {
    let mut hs = long_undo_send();
    send_one(&mut hs, "Hall keys tonight");
    // At once, before the engine has said where it holds the send.
    hs.key("q");
    assert!(!hs.app.quitting());
    assert_eq!(hs.picker_title().as_deref(), Some("Quit mejl?"));
    assert!(
        hs.screen().contains("Send it now, then quit"),
        "{}",
        hs.screen()
    );
    hs.key("enter");
    hs.wait("the quit after the send", |hs| hs.app.quitting());
    assert!(
        !hs.emails_in("sent", "Hall keys tonight").is_empty(),
        "sent well inside its 60 second window"
    );
    let _ = hs.close();
}

#[test]
fn keep_queued_quits_and_leaves_the_send_held() {
    let mut hs = long_undo_send();
    send_one(&mut hs, "Rota for December");
    hs.wait_text("the countdown", "Sending in");
    hs.key("q");
    hs.type_text("keep");
    hs.key("enter");
    assert!(hs.app.quitting());
    assert!(hs.emails_in("sent", "Rota for December").is_empty());
    let dir = hs.close();
    let reader = mejl_store::ReaderFactory::new(dir.path().join("acct"), Duration::from_secs(1))
        .open()
        .unwrap();
    let queued = reader.queued_sends(Timestamp::now()).unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
}

#[test]
fn cancel_send_from_the_quit_prompt_goes_back_to_compose() {
    let mut hs = long_undo_send();
    send_one(&mut hs, "Wrong list");
    hs.wait_text("the countdown", "Sending in");
    hs.key("q");
    hs.type_text("cancel");
    hs.key("enter");
    assert!(!hs.app.quitting());
    hs.wait("compose to come back", |hs| {
        *hs.app.state().screen() == Screen::Compose
    });
    assert!(hs.screen().contains("Wrong list"), "{}", hs.screen());
    hs.idle(Duration::from_millis(500));
    assert!(hs.emails_in("sent", "Wrong list").is_empty());
    hs.quit();
}

#[test]
fn esc_or_stay_keeps_mejl_running_and_quit_now_never_asks() {
    let mut hs = long_undo_send();
    send_one(&mut hs, "Shed paint");
    hs.key("q");
    hs.key("esc");
    assert!(!hs.app.quitting());
    assert!(hs.app.state().overlays().is_empty());
    hs.key("q");
    hs.type_text("stay");
    hs.key("enter");
    assert!(!hs.app.quitting());
    assert!(hs.app.state().overlays().is_empty());
    // Quit now does not ask, and the send stays queued.
    hs.key("Q");
    assert!(hs.app.quitting());
    assert!(hs.emails_in("sent", "Shed paint").is_empty());
    let _ = hs.close();
}

#[test]
fn quit_asks_nothing_once_the_send_is_out() {
    let mut hs = Harness::synced(120, 36);
    send_one(&mut hs, "Out already");
    hs.wait("the message in Sent", |hs| {
        !hs.emails_in("sent", "Out already").is_empty()
    });
    hs.key("q");
    assert!(hs.app.quitting(), "no prompt: {:?}", hs.picker_title());
    let _ = hs.close();
}

#[test]
fn undo_shows_the_restored_row_in_the_same_frame_as_the_key() {
    use mejl_ui::MailboxRole;
    let mut hs = Harness::synced_with(mark_read_setup(true), 160, 40);
    let (thread, unread) = hs.go_to_first_unread();
    let subject = hs.app.data().conversation(&thread).expect("cached").subject;
    let inbox = hs.mailbox_view(MailboxRole::Inbox);
    hs.key("y");
    hs.wait("the archive on the server", |hs| {
        unread.iter().all(|e| hs.in_mailbox(e, "archive"))
    });
    // Let the engine's answers in, so the store holds the archive and
    // nothing of it is pending on this thread any more.
    hs.idle(Duration::from_millis(300));
    assert!(hs.app.data().pending().is_empty());
    assert!(!hs.screen().contains(&subject));
    let frames = hs.app.frames();
    hs.key("z");
    assert_eq!(hs.app.frames(), frames + 1);
    let screen = hs.screen();
    assert!(
        screen.contains(&subject),
        "not back in that frame:\n{screen}"
    );
    assert_eq!(hs.inbox_index(&thread), Some(0), "{screen}");
    assert_eq!(hs.read_now(&thread, &unread), vec![false; unread.len()]);
    let now = hs.mailbox_view(MailboxRole::Inbox);
    assert_eq!(now.total_threads, inbox.total_threads);
    assert_eq!(now.unread_emails, inbox.unread_emails);
    hs.wait("the undo on the server", |hs| {
        unread
            .iter()
            .all(|e| hs.in_mailbox(e, "inbox") && !hs.seen_on_server(e))
    });
    hs.idle(Duration::from_millis(300));
    assert_eq!(hs.inbox_index(&thread), Some(0));
    hs.quit();
}

#[test]
fn undo_right_after_the_action_shows_in_the_same_frame() {
    let mut hs = Harness::synced(160, 40);
    hs.key("j");
    let (subject, emails) = hs.cursor_emails();
    let thread = hs
        .app
        .state()
        .list()
        .cursor_thread()
        .cloned()
        .expect("a row");
    hs.key("y");
    // No turn in between: the archive may not even be in the store yet.
    hs.key("z");
    assert!(hs.screen().contains(&subject), "{}", hs.screen());
    assert_eq!(hs.inbox_index(&thread), Some(1));
    hs.wait("the undo on the server", |hs| {
        hs.in_mailbox(&emails[0], "inbox") && !hs.in_mailbox(&emails[0], "archive")
    });
    hs.quit();
}

#[test]
fn redo_shows_in_the_same_frame_as_the_key() {
    let mut hs = Harness::synced(160, 40);
    hs.key("j");
    let (subject, emails) = hs.cursor_emails();
    let thread = hs
        .app
        .state()
        .list()
        .cursor_thread()
        .cloned()
        .expect("a row");
    hs.key("y");
    hs.wait("the archive on the server", |hs| {
        hs.in_mailbox(&emails[0], "archive")
    });
    hs.key("z");
    hs.wait("the undo on the server", |hs| {
        hs.in_mailbox(&emails[0], "inbox")
    });
    hs.idle(Duration::from_millis(300));
    assert!(hs.app.data().pending().is_empty());
    assert_eq!(hs.inbox_index(&thread), Some(1));
    hs.key("ctrl-y");
    assert_eq!(
        hs.inbox_index(&thread),
        None,
        "gone in the frame of the key"
    );
    assert!(!hs.screen().contains(&subject), "{}", hs.screen());
    hs.wait("the redo on the server", |hs| {
        hs.in_mailbox(&emails[0], "archive") && !hs.in_mailbox(&emails[0], "inbox")
    });
    hs.quit();
}

#[test]
fn send_now_quits_after_a_few_seconds_when_the_network_is_slow() {
    let mut hs = long_undo_send();
    send_one(&mut hs, "Slow network");
    hs.wait_text("the countdown", "Sending in");
    // Every request from now on takes far longer than the quit waits.
    hs.delay_ms.store(60_000, Ordering::SeqCst);
    hs.key("q");
    let asked = Instant::now();
    hs.key("enter");
    assert!(!hs.app.quitting(), "it waits for the send first");
    hs.wait("the quit", |hs| hs.app.quitting());
    let waited = asked.elapsed();
    assert!(
        (Duration::from_secs(4)..Duration::from_secs(10)).contains(&waited),
        "{waited:?}"
    );
    assert!(hs.emails_in("sent", "Slow network").is_empty());
    // The engine may not stop cleanly with a request hanging; that is fine
    // here, the send stays in the outbox for the next start.
    let _ = hs.app.close().shutdown();
}
