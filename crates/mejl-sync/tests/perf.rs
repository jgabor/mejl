//! Timings against a large synthetic account. Ignored by default; run with
//! `cargo test -p mejl-sync --release --test perf -- --ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use mejl_fake::{ExternalChange, FakeServer};
use mejl_sync::{BulkAction, Command, SyncEvent};

#[test]
#[ignore]
fn large_account_timings() {
    let server = FakeServer::synthetic(20_000, 7, now());
    let mut setup = Setup::default();
    setup.config.push = false;
    setup.config.poll_interval = Duration::from_secs(600);
    setup.config.window = 200;
    let started = Instant::now();
    let h = Harness::start(server, setup);
    h.synced();
    let cold = started.elapsed();
    let inbox = h.mailbox("inbox");
    let cached = h.read(|r| r.conversations(inbox.as_str(), 0, 0).unwrap().total);

    // 500 flag changes made elsewhere, then one warm pass.
    let ids = server_ids_in(&h.server, inbox.as_str());
    for id in ids.iter().take(500) {
        h.server
            .mutate_from_elsewhere(ExternalChange::SetKeyword {
                email_id: id.clone(),
                keyword: "$flagged".into(),
                value: true,
            })
            .unwrap();
    }
    let mark = h.events.mark();
    let started = Instant::now();
    h.handle.send(Command::RefreshNow);
    h.synced_after(mark);
    let warm = started.elapsed();

    // Every cached Inbox conversation archived as one action.
    let started = Instant::now();
    h.handle.send(Command::Bulk {
        token: 1,
        mailbox: inbox.clone(),
        except_threads: Vec::new(),
        action: BulkAction::Archive,
        label: None,
    });
    h.events.wait("bulk applied", |e| {
        matches!(e, SyncEvent::Applied { token: 1, .. })
    });
    let bulk = started.elapsed();
    let moved = h.read(|r| {
        r.messages(h.mailbox("archive").as_str(), 0, 0)
            .unwrap()
            .total
    });
    println!(
        "cold sync {cold:?} ({cached} Inbox conversations cached); \
         warm sync of 500 changes {warm:?}; bulk archive applied locally in {bulk:?} \
         ({moved} messages now in Archive)"
    );
    h.shutdown();
}
