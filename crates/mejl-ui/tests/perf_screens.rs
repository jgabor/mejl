//! Update plus draw time of the heaviest new screens: the palette over
//! every action, mailbox, contact and masked address, compose with a long
//! body, search per keystroke, and the rules screen. Run in release to see
//! the real numbers:
//!
//! `cargo test -p mejl-ui --release --test perf_screens -- --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::{Harness, key_event};
use mejl_ui::{Event, Screen};

fn stats(name: &str, mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    let p50 = v[v.len() / 2];
    let p99 = v[(v.len() * 99 / 100).min(v.len() - 1)];
    eprintln!("{name}: p50 {p50:?}, p99 {p99:?}, max {:?}", v[v.len() - 1]);
    p99
}

/// Debug builds on a shared machine are slow; the bound only catches
/// regressions by an order of magnitude. Release numbers go in the report.
const BOUND: Duration = Duration::from_millis(60);

fn time_keys(hs: &mut Harness, keys: &[&str], rounds: usize) -> Vec<Duration> {
    let mut v = Vec::new();
    for _ in 0..rounds {
        for k in keys {
            let t = Instant::now();
            hs.event(Event::Key(key_event(k)));
            v.push(t.elapsed());
        }
    }
    v
}

#[test]
fn palette_with_every_item() {
    let mut hs = Harness::demo(160, 40);
    hs.keys(":");
    // Type and erase a query, the heaviest path: every keystroke rescores
    // every item.
    let v = time_keys(
        &mut hs,
        &[
            "m",
            "a",
            "s",
            "k",
            "backspace",
            "backspace",
            "backspace",
            "backspace",
            "down",
            "up",
        ],
        30,
    );
    let p99 = stats("palette, update plus draw per key at 160x40", v);
    assert!(p99 < BOUND, "{p99:?}");
}

#[test]
fn compose_with_a_long_body() {
    for lines in [300, 1_000, 2_000] {
        compose_body(lines);
    }
}

fn compose_body(lines: usize) {
    let mut hs = Harness::demo(160, 40);
    hs.keys("c tab tab tab");
    let mut body = String::new();
    for i in 0..lines {
        body.push_str(&format!(
            "Line {i}: the quick brown fox jumps over the lazy dog, and then some more words to wrap.\n"
        ));
    }
    hs.event(Event::Paste(body));
    assert_eq!(*hs.state.screen(), Screen::Compose);
    let v = time_keys(
        &mut hs,
        &["x", "backspace", "up", "down", "space", "backspace"],
        40,
    );
    let p99 = stats(
        &format!("compose, {lines} line body, update plus draw per key at 160x40"),
        v,
    );
    assert!(p99 < BOUND, "{p99:?}");
}

#[test]
fn search_per_keystroke() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("/");
    let v = time_keys(
        &mut hs,
        &[
            "g",
            "a",
            "r",
            "d",
            "backspace",
            "backspace",
            "backspace",
            "backspace",
        ],
        30,
    );
    let p99 = stats("search, update plus draw per key at 160x40", v);
    assert!(p99 < BOUND, "{p99:?}");
}

#[test]
fn rules_and_masked_screens() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("g r");
    let v = time_keys(&mut hs, &["j", "k"], 50);
    let p99 = stats("rules, update plus draw per key at 160x40", v);
    assert!(p99 < BOUND, "{p99:?}");
    hs.keys("g m");
    let v = time_keys(&mut hs, &["j", "k"], 50);
    let p99 = stats("masked, update plus draw per key at 160x40", v);
    assert!(p99 < BOUND, "{p99:?}");
}
