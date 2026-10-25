//! Test harness: an engine against mejl-fake, an event recorder, and
//! transports that fail on purpose. Nothing here touches the network
//! beyond 127.0.0.1.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use mejl_fake::FakeServer;
use mejl_jmap::transport::{
    BoxFuture, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
};
use mejl_jmap::{Client, Id, Secret};
use mejl_store::{Reader, Store, StoreOptions};
use mejl_sync::{Engine, EngineConfig, SyncEvent, SyncHandle, SyncStatus};
use serde_json::{Value, json};

pub const WAIT: Duration = Duration::from_secs(15);

pub fn now() -> Timestamp {
    "2026-10-23T14:27:00Z".parse().unwrap()
}

/// Timings in milliseconds, so the suite stays fast.
pub fn fast_config() -> EngineConfig {
    EngineConfig {
        window: 200,
        load_more: 50,
        push_ping_seconds: 1,
        push_idle_margin: Duration::from_millis(500),
        push_debounce: Duration::from_millis(10),
        push_failures_before_poll: 3,
        poll_interval: Duration::from_millis(300),
        backoff_min: Duration::from_millis(20),
        backoff_max: Duration::from_millis(300),
        masked_refresh: Duration::from_secs(600),
        data_coalesce: Duration::from_millis(5),
        shutdown_timeout: Duration::from_secs(3),
        time_zone: TimeZone::UTC,
        ..EngineConfig::default()
    }
}

/// Collects events and lets tests wait for one.
#[derive(Clone, Default)]
pub struct Recorder {
    inner: Arc<(Mutex<Vec<SyncEvent>>, Condvar)>,
}

impl Recorder {
    pub fn push(&self, e: SyncEvent) {
        let (lock, cv) = &*self.inner;
        lock.lock().unwrap().push(e);
        cv.notify_all();
    }

    /// The number of events so far, to wait for later ones only.
    pub fn mark(&self) -> usize {
        self.inner.0.lock().unwrap().len()
    }

    pub fn all(&self) -> Vec<SyncEvent> {
        self.inner.0.lock().unwrap().clone()
    }

    /// The first event at or after `from` that matches.
    pub fn wait_after(
        &self,
        from: usize,
        what: &str,
        pred: impl Fn(&SyncEvent) -> bool,
    ) -> SyncEvent {
        self.wait_index(from, what, pred).1
    }

    /// Like [`Recorder::wait_after`], with the event's position.
    pub fn wait_index(
        &self,
        from: usize,
        what: &str,
        pred: impl Fn(&SyncEvent) -> bool,
    ) -> (usize, SyncEvent) {
        let (lock, cv) = &*self.inner;
        let deadline = Instant::now() + WAIT;
        let mut events = lock.lock().unwrap();
        loop {
            if let Some((i, e)) = events.iter().enumerate().skip(from).find(|(_, e)| pred(e)) {
                return (i, e.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let tail: Vec<String> = events
                    .iter()
                    .rev()
                    .take(15)
                    .map(|e| format!("{e:?}"))
                    .collect();
                panic!("timed out waiting for {what}; last events: {tail:#?}");
            }
            events = cv.wait_timeout(events, left).unwrap().0;
        }
    }

    pub fn wait(&self, what: &str, pred: impl Fn(&SyncEvent) -> bool) -> SyncEvent {
        self.wait_after(0, what, pred)
    }

    pub fn any(&self, pred: impl Fn(&SyncEvent) -> bool) -> bool {
        self.inner.0.lock().unwrap().iter().any(pred)
    }

    pub fn last_status(&self) -> Option<SyncStatus> {
        self.inner
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                SyncEvent::Status(s) => Some(s.clone()),
                _ => None,
            })
    }
}

/// A transport that can be switched off, counts requests and records
/// request bodies.
pub struct TestTransport {
    pub inner: Arc<dyn Transport>,
    pub down: AtomicBool,
    pub push_down: AtomicBool,
    /// Push connects but never sends a byte.
    pub stall_push: AtomicBool,
    pub sends: AtomicUsize,
    pub streams: AtomicUsize,
    pub bodies: Mutex<Vec<Value>>,
    /// The next request with a call of this name is answered by the server
    /// at once, but its response is held until [`TestTransport::release`].
    pub hold: Mutex<Option<String>>,
    pub holding: Arc<AtomicBool>,
    /// The next request whose body contains this text is refused with
    /// this HTTP status, without reaching the server.
    pub refuse: Mutex<Option<(String, u16)>>,
    gate: Arc<tokio::sync::Notify>,
}

impl TestTransport {
    pub fn new(inner: Arc<dyn Transport>) -> Arc<Self> {
        Arc::new(TestTransport {
            inner,
            down: AtomicBool::new(false),
            push_down: AtomicBool::new(false),
            stall_push: AtomicBool::new(false),
            sends: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
            hold: Mutex::new(None),
            holding: Arc::new(AtomicBool::new(false)),
            refuse: Mutex::new(None),
            gate: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Lets a held response through.
    pub fn release(&self) {
        self.gate.notify_one();
    }

    /// How many method calls with this name were sent.
    pub fn calls_named(&self, name: &str) -> usize {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .flat_map(|b| b["methodCalls"].as_array().cloned().unwrap_or_default())
            .filter(|c| c[0] == name)
            .count()
    }
}

impl TestTransport {
    /// How many requests held at least one call with this name.
    pub fn requests_with(&self, name: &str) -> usize {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|b| {
                b["methodCalls"]
                    .as_array()
                    .is_some_and(|calls| calls.iter().any(|c| c[0] == name))
            })
            .count()
    }
}

impl Transport for TestTransport {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let mut held = false;
        let mut refused = None;
        if let Some(body) = &req.body
            && let Ok(v) = serde_json::from_slice::<Value>(body)
        {
            let mut refuse = self.refuse.lock().unwrap();
            if let Some((text, status)) = refuse.as_ref()
                && v.to_string().contains(text.as_str())
            {
                refused = Some(*status);
                *refuse = None;
            }
            let mut hold = self.hold.lock().unwrap();
            if let Some(name) = hold.as_deref()
                && v["methodCalls"]
                    .as_array()
                    .is_some_and(|calls| calls.iter().any(|c| c[0] == name))
            {
                held = true;
                *hold = None;
            }
            self.bodies.lock().unwrap().push(v);
        }
        if self.down.load(Ordering::SeqCst) {
            return Box::pin(async { Err(TransportError::Connect("test: network down".into())) });
        }
        if let Some(status) = refused {
            return Box::pin(async move {
                Ok(HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: Default::default(),
                })
            });
        }
        let reply = self.inner.send(req);
        if !held {
            return reply;
        }
        let gate = self.gate.clone();
        let holding = self.holding.clone();
        Box::pin(async move {
            // The server handles the request now; only the answer waits.
            let response = reply.await;
            holding.store(true, Ordering::SeqCst);
            gate.notified().await;
            response
        })
    }

    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
        self.streams.fetch_add(1, Ordering::SeqCst);
        if self.down.load(Ordering::SeqCst) {
            return Box::pin(async { Err(TransportError::Connect("test: network down".into())) });
        }
        if self.push_down.load(Ordering::SeqCst) {
            // The API still answers; only the event endpoint refuses.
            return Box::pin(async {
                Ok(StreamingResponse {
                    status: 503,
                    headers: Vec::new(),
                    body: Box::pin(futures::stream::empty()),
                })
            });
        }
        if self.stall_push.load(Ordering::SeqCst) {
            return Box::pin(async {
                Ok(StreamingResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "text/event-stream".into())],
                    body: Box::pin(futures::stream::pending()),
                })
            });
        }
        self.inner.stream(req)
    }
}

pub struct Harness {
    pub server: FakeServer,
    pub handle: SyncHandle,
    pub events: Recorder,
    pub transport: Arc<TestTransport>,
    pub dir: tempfile::TempDir,
    reader: Mutex<Option<Reader>>,
}

/// Prepares the store before the engine takes it.
pub type BeforeStart = Box<dyn FnOnce(&mut Store)>;

pub struct Setup {
    pub config: EngineConfig,
    pub token: Option<String>,
    pub dir: Option<tempfile::TempDir>,
    pub before_start: Option<BeforeStart>,
    pub transport: Option<(Arc<dyn Transport>, String)>,
    /// Push connections fail from the start.
    pub push_down: bool,
    /// Push connects but stays silent from the start.
    pub stall_push: bool,
    /// Set [`TestTransport::refuse`] before the engine starts.
    pub refuse: Option<(String, u16)>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            config: fast_config(),
            token: None,
            dir: None,
            before_start: None,
            transport: None,
            push_down: false,
            stall_push: false,
            refuse: None,
        }
    }
}

impl Harness {
    pub fn demo() -> Self {
        Self::start(FakeServer::demo(now()), Setup::default())
    }

    pub fn start(server: FakeServer, setup: Setup) -> Self {
        let dir = setup
            .dir
            .unwrap_or_else(|| tempfile::tempdir().expect("temp dir"));
        let (inner, session_url): (Arc<dyn Transport>, String) = match setup.transport {
            Some(t) => t,
            None => (
                Arc::new(server.transport()) as Arc<dyn Transport>,
                server.session_url(),
            ),
        };
        let transport = TestTransport::new(inner);
        transport.push_down.store(setup.push_down, Ordering::SeqCst);
        transport
            .stall_push
            .store(setup.stall_push, Ordering::SeqCst);
        *transport.refuse.lock().unwrap() = setup.refuse;
        let token = setup.token.unwrap_or_else(|| server.token().to_owned());
        let client = Client::new(transport.clone(), session_url, Secret::new(token));
        let mut store = Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        if let Some(f) = setup.before_start {
            f(&mut store);
        }
        let events = Recorder::default();
        let rec = events.clone();
        let handle = Engine::start(setup.config, client, store, move |e| rec.push(e)).unwrap();
        Harness {
            server,
            handle,
            events,
            transport,
            dir,
            reader: Mutex::new(None),
        }
    }

    /// Runs `f` with a reader.
    pub fn read<R>(&self, f: impl FnOnce(&Reader) -> R) -> R {
        let mut guard = self.reader.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.handle.reader_factory().open().unwrap());
        }
        f(guard.as_ref().unwrap())
    }

    /// Polls the store until `pred` holds.
    pub fn wait_store(&self, what: &str, pred: impl Fn(&Reader) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.read(&pred) {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for the store: {what}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Polls the fake server until `pred` holds.
    pub fn wait_server(&self, what: &str, pred: impl Fn(&FakeServer) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if pred(&self.server) {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for the server: {what}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the first full sync.
    pub fn synced(&self) {
        self.events.wait(
            "first sync",
            |e| matches!(e, SyncEvent::Status(s) if s.last_sync.is_some()),
        );
    }

    /// Waits for a sync that finished after `mark`.
    pub fn synced_after(&self, mark: usize) {
        let before = self.events.last_status().and_then(|s| s.last_sync);
        self.events.wait_after(
            mark,
            "a later sync",
            |e| matches!(e, SyncEvent::Status(s) if s.last_sync.is_some() && s.last_sync != before),
        );
    }

    pub fn mailbox(&self, role_or_name: &str) -> Id {
        Id::from(self.server.mailbox_id(role_or_name).expect("mailbox"))
    }

    /// The server's view of one email.
    pub fn server_email(&self, id: &str) -> Value {
        let r = self
            .server
            .call(
                "Email/get",
                json!({"ids": [id], "properties": ["mailboxIds", "keywords", "subject"]}),
            )
            .unwrap();
        r["list"][0].clone()
    }

    pub fn shutdown(self) {
        assert!(self.handle.shutdown(), "the engine stops in time");
    }

    /// Stops the engine and keeps the account directory for a restart.
    pub fn stop(self) -> (FakeServer, tempfile::TempDir) {
        assert!(self.handle.shutdown(), "the engine stops in time");
        (self.server, self.dir)
    }
}

/// The ids of every email the server has in a mailbox.
pub fn server_ids_in(server: &FakeServer, mailbox: &str) -> Vec<String> {
    let r = server
        .call(
            "Email/query",
            json!({"filter": {"inMailbox": mailbox}, "limit": 1000}),
        )
        .unwrap();
    r["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

/// Thread ids and email ids in a mailbox, from the server.
pub fn server_threads_in(server: &FakeServer, mailbox: &str) -> Vec<(String, String)> {
    let ids = server_ids_in(server, mailbox);
    let r = server
        .call("Email/get", json!({"ids": ids, "properties": ["threadId"]}))
        .unwrap();
    r["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["threadId"].as_str().unwrap().to_owned(),
                e["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
