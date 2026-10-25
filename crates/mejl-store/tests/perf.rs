//! Percentile timings on a 100k-message store. Ignored by default because
//! building the store takes seconds; run it in release:
//!
//! `cargo test -p mejl-store --release --test perf -- --ignored --nocapture`
//!
//! Budgets are only asserted in release builds.

#[path = "../benches/support/synth.rs"]
mod synth;

use std::time::{Duration, Instant};

use mejl_jmap::method::ChangesResponse;
use mejl_jmap::types::Email;
use mejl_store::SearchOptions;

const N: usize = 100_000;

fn percentiles(mut samples: Vec<Duration>) -> (Duration, Duration, Duration, Duration) {
    samples.sort();
    let at = |p: f64| {
        let i = ((samples.len() as f64 - 1.0) * p).round() as usize;
        samples[i]
    };
    (
        at(0.5),
        at(0.95),
        at(0.99),
        *samples.last().unwrap_or(&Duration::ZERO),
    )
}

fn report(name: &str, samples: Vec<Duration>) -> (Duration, Duration, Duration) {
    let (p50, p95, p99, max) = percentiles(samples);
    println!("{name:<40} p50 {p50:>10.3?}  p95 {p95:>10.3?}  p99 {p99:>10.3?}  max {max:>10.3?}");
    (p50, p95, p99)
}

#[test]
#[ignore = "builds a 100k-message store; run in release with --ignored"]
fn budgets_on_100k_messages() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut store = synth::build(dir.path(), N, synth::now());
    println!("built {N} messages in {:?}", started.elapsed());
    let reader = store.reader().unwrap();
    let release = !cfg!(debug_assertions);

    let mut rng = synth::Rng::new(42);
    let mut near = Vec::new();
    let mut deep = Vec::new();
    for _ in 0..2000 {
        let offset = rng.below(2000) as usize;
        let t = Instant::now();
        let page = reader.conversations(synth::INBOX, offset, 60).unwrap();
        near.push(t.elapsed());
        assert_eq!(page.rows.len(), 60);
        let offset = 30_000 + rng.below(20_000) as usize;
        let t = Instant::now();
        reader.conversations(synth::INBOX, offset, 60).unwrap();
        deep.push(t.elapsed());
    }
    let (_, _, p99) = report("conversation page, offset < 2000", near);
    if release {
        assert!(p99 < Duration::from_millis(2), "page p99 {p99:?} over 2 ms");
    }
    report("conversation page, offset 30k to 50k", deep);

    let mut flat = Vec::new();
    for _ in 0..2000 {
        let offset = rng.below(2000) as usize;
        let t = Instant::now();
        reader.messages(synth::INBOX, offset, 60).unwrap();
        flat.push(t.elapsed());
    }
    let (_, _, p99) = report("message page, offset < 2000", flat);
    if release {
        assert!(
            p99 < Duration::from_millis(2),
            "flat page p99 {p99:?} over 2 ms"
        );
    }

    let thread = synth::long_thread(N, synth::now());
    let samples = (0..2000)
        .map(|_| {
            let t = Instant::now();
            reader.conversation(&thread).unwrap();
            t.elapsed()
        })
        .collect();
    report("conversation detail, 6 messages", samples);

    let samples = (0..500)
        .map(|_| {
            let t = Instant::now();
            reader.mailboxes().unwrap();
            t.elapsed()
        })
        .collect();
    report("mailbox list", samples);

    let options = SearchOptions::default();
    let typed = "garden rota";
    let mut keystrokes = Vec::new();
    let mut per_prefix: Vec<Vec<Duration>> = vec![Vec::new(); typed.len()];
    for _ in 0..30 {
        for end in 1..=typed.len() {
            let t = Instant::now();
            reader.search(&typed[..end], &options).unwrap();
            let elapsed = t.elapsed();
            keystrokes.push(elapsed);
            per_prefix[end - 1].push(elapsed);
        }
    }
    for (end, samples) in per_prefix.into_iter().enumerate() {
        report(&format!("  search {:?}", &typed[..=end]), samples);
    }
    let (_, p95, _) = report("search per keystroke, \"garden rota\"", keystrokes);
    if release {
        assert!(
            p95 < Duration::from_millis(16),
            "search p95 {p95:?} over 16 ms"
        );
    }
    let samples = (0..300)
        .map(|_| {
            let t = Instant::now();
            reader.search("from:juniper is:unread", &options).unwrap();
            t.elapsed()
        })
        .collect();
    report("search with operators", samples);

    let mut applies = Vec::new();
    for round in 0..20 {
        let fetched = synth::flag_batch(N, 500, round);
        let mut changes = ChangesResponse::<Email>::new("A1", "s", "s");
        changes.updated = fetched.iter().map(|e| e.id.clone()).collect();
        let t = Instant::now();
        let mut batch = store.batch().unwrap();
        batch.apply_email_changes(&changes, &fetched).unwrap();
        batch.commit().unwrap();
        applies.push(t.elapsed());
    }
    report("apply 500-email changes batch", applies);
}
