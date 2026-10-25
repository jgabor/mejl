//! Push over loopback HTTP with the reported Fastmail quirks: the
//! non-standard event shape, a plain eventSourceUrl and dropped
//! connections. Also the idle timeout.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use mejl_fake::{Address, EventSourceUrl, FakeServer, IncomingMessage, PushShape};
use mejl_jmap::{HttpTransport, Id, Transport};
use mejl_sync::{PushState, SyncEvent};

fn mail(subject: &str) -> IncomingMessage {
    IncomingMessage::new(
        Address::new("Owen Castillo", "owen@castillo.example.net"),
        subject,
    )
    .text("hello")
}

fn in_inbox(r: &mejl_store::Reader, inbox: &Id, id: &Id) -> bool {
    r.conversations(inbox.as_str(), 0, 10)
        .unwrap()
        .rows
        .iter()
        .any(|row| row.email_id == *id)
}

#[test]
fn loopback_push_with_fastmail_quirks_reconnects_and_resyncs() {
    // The test owns a runtime for the loopback server; the engine has its
    // own.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let server = FakeServer::demo(now());
    {
        let mut q = server.quirks_mut();
        q.push_shape = PushShape::FastmailConnect;
        q.event_source_url = EventSourceUrl::Plain;
        q.drop_push_after_events = Some(2);
        q.push_chunk_size = Some(7);
    }
    let http = rt
        .block_on(server.serve_http("127.0.0.1:0".parse().unwrap()))
        .unwrap();
    let transport: Arc<dyn Transport> = Arc::new(HttpTransport::new().unwrap());
    let mut setup = Setup {
        transport: Some((transport, http.session_url())),
        ..Setup::default()
    };
    // Pings would count towards the drop, and polling would hide whether
    // the reconnect resyncs; keep both out of the way.
    setup.config.push_ping_seconds = 30;
    setup.config.poll_interval = Duration::from_secs(600);
    setup.config.push_failures_before_poll = 1000;
    let h = Harness::start(server, setup);
    h.synced();
    h.events.wait(
        "connected",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Connected),
    );
    let inbox = h.mailbox("inbox");

    // First event: picked up through push.
    let a = Id::from(h.server.deliver_incoming(mail("One")).unwrap());
    h.wait_store("first mail", |r| in_inbox(r, &inbox, &a));

    // Second event: delivered, then the connection drops, and the event
    // endpoint refuses new connections for a while.
    h.transport.push_down.store(true, Ordering::SeqCst);
    let mark = h.events.mark();
    let b = Id::from(h.server.deliver_incoming(mail("Two")).unwrap());
    h.wait_store("second mail", |r| in_inbox(r, &inbox, &b));
    let (at, _) = h.events.wait_index(
        mark,
        "reconnecting",
        |e| matches!(e, SyncEvent::Status(s) if matches!(s.push, PushState::Reconnecting { .. })),
    );

    // Mail that arrives while disconnected makes no event for mejl.
    let missed = Id::from(h.server.deliver_incoming(mail("While away")).unwrap());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!h.read(|r| in_inbox(r, &inbox, &missed)));

    // Only the sync after the reconnect can bring it in.
    h.transport.push_down.store(false, Ordering::SeqCst);
    h.events.wait_after(
        at,
        "connected again",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Connected),
    );
    h.wait_store("missed mail after reconnect", |r| {
        in_inbox(r, &inbox, &missed)
    });
    assert!(h.transport.streams.load(Ordering::SeqCst) >= 3);

    // And push still works.
    let c = Id::from(h.server.deliver_incoming(mail("Three")).unwrap());
    h.wait_store("third mail", |r| in_inbox(r, &inbox, &c));
    h.shutdown();
    rt.block_on(http.shutdown());
}

#[test]
fn silent_push_is_dropped_after_the_idle_timeout() {
    let mut setup = Setup::default();
    setup.config.push_ping_seconds = 1;
    setup.config.push_idle_margin = Duration::from_millis(200);
    setup.stall_push = true;
    let h = Harness::start(FakeServer::demo(now()), setup);
    h.synced();
    h.events.wait(
        "connected",
        |e| matches!(e, SyncEvent::Status(s) if s.push == PushState::Connected),
    );
    let opened = h.transport.streams.load(Ordering::SeqCst);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(6);
    while h.transport.streams.load(Ordering::SeqCst) == opened {
        assert!(
            Instant::now() < deadline,
            "no reconnect after the idle timeout"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Twice the ping plus the margin, give or take scheduling.
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    h.shutdown();
}
