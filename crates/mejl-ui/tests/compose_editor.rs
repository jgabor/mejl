//! The compose body editor with a long body: key-to-frame time that does
//! not grow with the body, and the text after edits. Run in release to see
//! the real numbers:
//!
//! `cargo test -p mejl-ui --release --test compose_editor -- --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::{Harness, key_event};
use mejl_ui::{Cmd, Event, Screen};

fn stats(name: &str, mut v: Vec<Duration>) -> (Duration, Duration) {
    v.sort_unstable();
    let p50 = v[v.len() / 2];
    let p99 = v[(v.len() * 99 / 100).min(v.len() - 1)];
    eprintln!("{name}: p50 {p50:?}, p99 {p99:?}, max {:?}", v[v.len() - 1]);
    (p50, p99)
}

fn long_body(lines: usize) -> String {
    let mut body = String::new();
    for i in 0..lines {
        body.push_str(&format!(
            "Line {i}: the quick brown fox jumps over the lazy dog, and then some more words to wrap.\n"
        ));
    }
    body
}

/// Compose with the cursor in the body after a pasted body.
fn compose_with(lines: usize, w: u16, h: u16) -> Harness {
    let mut hs = Harness::demo(w, h);
    hs.keys("c tab tab tab");
    hs.event(Event::Paste(long_body(lines)));
    assert_eq!(*hs.state.screen(), Screen::Compose);
    hs
}

/// Update plus draw per key, the key-to-frame time without the terminal.
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

const KEYS: &[&str] = &[
    "x",
    "backspace",
    "up",
    "down",
    "space",
    "backspace",
    "enter",
    "backspace",
];

/// The editor used to re-wrap the whole body on every edit, so a key at
/// 5000 lines cost about ten times one at 500. Now the cost must not grow
/// with the body. The ratio holds in debug builds and on a busy machine;
/// the absolute bound only catches gross regressions.
#[test]
fn key_to_frame_does_not_grow_with_the_body() {
    let mut medians = Vec::new();
    for lines in [500, 5_000] {
        let mut hs = compose_with(lines, 160, 40);
        // Warm up, then measure at the end of the body and at the top.
        time_keys(&mut hs, KEYS, 2);
        let mut v = time_keys(&mut hs, KEYS, 20);
        hs.keys("alt-<");
        v.extend(time_keys(&mut hs, KEYS, 20));
        let (p50, p99) = stats(
            &format!("compose, {lines} line body, update plus draw per key at 160x40"),
            v,
        );
        assert!(
            p99 < Duration::from_millis(30),
            "{lines} lines: p99 {p99:?}"
        );
        medians.push(p50);
    }
    let (small, big) = (medians[0], medians[1]);
    assert!(
        big < small * 3,
        "5000 lines {big:?} against 500 lines {small:?}"
    );
}

#[test]
fn a_5000_line_body_at_80x24_stays_fast() {
    let mut hs = compose_with(5_000, 80, 24);
    let v = time_keys(&mut hs, KEYS, 20);
    let (_, p99) = stats(
        "compose, 5000 line body, update plus draw per key at 80x24",
        v,
    );
    assert!(p99 < Duration::from_millis(30), "p99 {p99:?}");
}

#[test]
fn edits_in_a_5000_line_body_land_where_the_cursor_is() {
    let mut hs = compose_with(5_000, 80, 24);
    let start = hs.state.compose_body().unwrap();
    // The paste leaves the cursor on the empty line after the last one.
    hs.keys("up ctrl-e");
    for k in "!?".chars() {
        hs.event(Event::Key(key_event(&k.to_string())));
    }
    let body = hs.state.compose_body().unwrap();
    let lines: Vec<&str> = body.lines().collect();
    let last = lines
        .iter()
        .position(|l| l.starts_with("Line 4999:"))
        .unwrap();
    assert!(lines[last].ends_with("wrap.!?"), "{:?}", lines[last]);
    assert!(
        hs.text().contains("wrap.!?"),
        "the edited line is on screen:\n{}",
        hs.text()
    );
    // To the top, split the first line, and check the screen follows.
    hs.keys("alt-< ctrl-e enter");
    let body = hs.state.compose_body().unwrap();
    let first = body.lines().position(|l| l.starts_with("Line 0:")).unwrap();
    let after: Vec<&str> = body.lines().skip(first).take(3).collect();
    assert!(after[0].ends_with("wrap."), "{after:?}");
    assert_eq!(after[1], "", "{after:?}");
    assert!(after[2].starts_with("Line 1:"), "{after:?}");
    assert!(hs.text().contains("Line 1:"), "{}", hs.text());
    // Undo all three edits with the editor's own undo key (Ctrl-z now
    // suspends mejl everywhere); the body is as pasted.
    hs.keys("ctrl-u ctrl-u ctrl-u");
    assert_eq!(hs.state.compose_body().unwrap(), start);
}

#[test]
fn body_undo_has_keys_next_to_suspend() {
    let mut hs = compose_with(3, 80, 24);
    let start = hs.state.compose_body().unwrap();
    // Ctrl-_ and Ctrl-/ with the kitty protocol, and Ctrl-7, which is how
    // crossterm reports their legacy byte 0x1F.
    for undo in ["ctrl-_", "ctrl-/", "ctrl-7"] {
        hs.keys("x");
        assert_ne!(hs.state.compose_body().unwrap(), start);
        let cmds = hs.keys(undo);
        assert_eq!(hs.state.compose_body().unwrap(), start, "{undo} undoes");
        assert!(
            cmds.iter().any(|c| matches!(c, Cmd::WakeAt(_))),
            "{undo} restarts the autosave timer: {cmds:?}"
        );
        assert!(!cmds.contains(&Cmd::Suspend), "{undo} does not suspend");
        hs.keys("ctrl-y");
        assert!(
            hs.state.compose_body().unwrap().contains('x'),
            "ctrl-y redoes after {undo}"
        );
        hs.keys("ctrl-u");
        assert_eq!(hs.state.compose_body().unwrap(), start);
    }
    // Ctrl-z suspends and leaves the text alone.
    hs.keys("x");
    let typed = hs.state.compose_body().unwrap();
    let cmds = hs.keys("ctrl-z");
    assert!(cmds.contains(&Cmd::Suspend));
    assert_eq!(hs.state.compose_body().unwrap(), typed);
    // Alt with the same keys is not undo.
    hs.keys("alt-_");
    assert_eq!(hs.state.compose_body().unwrap(), typed);
}

#[test]
fn compose_help_lists_the_body_undo_keys() {
    let mut hs = compose_with(3, 120, 36);
    hs.keys("ctrl-o ?");
    let text = hs.text();
    assert!(text.contains("Keys: compose"), "{text}");
    assert!(text.contains("Body editor"), "{text}");
    assert!(
        text.contains("^_, ^/, ^u") && text.contains("Undo the last edit in the body"),
        "{text}"
    );
    assert!(text.contains("^y, ^r"), "{text}");
    hs.event(Event::Paste("redo the".into()));
    let text = hs.text();
    assert!(text.contains("Redo the last undone edit"), "{text}");
    assert!(!text.contains("Undo the last edit in the body"), "{text}");
}
