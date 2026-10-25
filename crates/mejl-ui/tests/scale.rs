//! A 100k conversation mailbox: update and render stay fast and only the
//! visible page is ever read.

mod common;

use std::time::{Duration, Instant};

use common::{Harness, key_event, now};
use mejl_ui::{CHUNK_ROWS, Event, MemoryData, UiConfig};

const ROWS: usize = 100_000;

fn big(w: u16, h: u16) -> Harness {
    Harness::with(
        MemoryData::synthetic(ROWS, now()),
        UiConfig::default(),
        w,
        h,
    )
}

fn p99(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[(v.len() * 99 / 100).min(v.len() - 1)]
}

#[test]
fn startup_reads_one_chunk() {
    let data = MemoryData::synthetic(ROWS, now());
    let hs = Harness::with(data, UiConfig::default(), 160, 40);
    let stats = hs.data.stats();
    assert!(stats.max_limit <= CHUNK_ROWS, "{stats:?}");
    assert!(stats.rows_returned <= 2 * CHUNK_ROWS, "{stats:?}");
    assert!(hs.text().contains("100000 conversations"));
}

#[test]
fn scrolling_100k_rows_is_fast_and_virtual() {
    let mut hs = big(160, 40);
    hs.data.reset_stats();
    let mut frames = Vec::new();
    let started = Instant::now();
    for _ in 0..400 {
        let t = Instant::now();
        hs.event(Event::Key(key_event("j")));
        frames.push(t.elapsed());
    }
    let total = started.elapsed();
    let mut sorted = frames.clone();
    sorted.sort_unstable();
    eprintln!(
        "100k rows, update plus draw per key at 160x40: p50 {:?}, p99 {:?}, max {:?}",
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 99 / 100],
        sorted[sorted.len() - 1]
    );
    assert_eq!(hs.state.list().cursor(), 400);
    let stats = hs.data.stats();
    // 400 rows of travel touch about seven 64-row chunks, never the list.
    assert!(stats.max_limit <= CHUNK_ROWS, "{stats:?}");
    assert!(stats.rows_returned <= 12 * CHUNK_ROWS, "{stats:?}");
    assert!(stats.page_calls <= 12, "{stats:?}");
    // Generous bounds: this is a debug build on a shared machine. The real
    // budget (p99 under 8 ms) is checked in release benchmarks.
    let worst = p99(frames);
    assert!(worst < Duration::from_millis(100), "p99 {worst:?}");
    assert!(total < Duration::from_secs(20), "total {total:?}");
}

#[test]
fn jumping_to_the_end_reads_only_the_last_page() {
    let mut hs = big(160, 40);
    hs.data.reset_stats();
    let t = Instant::now();
    hs.keys("G");
    let elapsed = t.elapsed();
    assert_eq!(hs.state.list().cursor(), ROWS - 1);
    let stats = hs.data.stats();
    assert!(stats.rows_returned <= 3 * CHUNK_ROWS, "{stats:?}");
    assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    assert!(hs.text().contains("99999"), "the last row is drawn");
    hs.data.reset_stats();
    hs.keys("g g");
    assert_eq!(hs.state.list().cursor(), 0);
    assert!(hs.data.stats().rows_returned <= 3 * CHUNK_ROWS);
}

#[test]
fn paging_and_select_all_stay_bounded() {
    let mut hs = big(80, 24);
    hs.data.reset_stats();
    for _ in 0..200 {
        hs.event(Event::Key(key_event("pgdn")));
    }
    assert!(hs.state.list().cursor() > 3_000);
    let stats = hs.data.stats();
    assert!(stats.max_limit <= CHUNK_ROWS, "{stats:?}");
    // Each page of 19 rows needs at most one new chunk.
    assert!(stats.rows_returned <= 80 * CHUNK_ROWS, "{stats:?}");
    // Selecting everything stores nothing per row.
    hs.data.reset_stats();
    hs.keys("* a");
    assert_eq!(hs.state.list().selection().count(ROWS), ROWS);
    assert!(hs.data.stats().rows_returned <= CHUNK_ROWS);
    // An X range beyond the limit asks for * a instead of reading 50k rows.
    hs.keys("* n x G X");
    assert!(hs.state.toast().unwrap().text.contains("use * a"));
    assert!(hs.data.stats().max_limit <= CHUNK_ROWS);
}
