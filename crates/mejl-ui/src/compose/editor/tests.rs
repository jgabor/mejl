//! The editor against `ratatui-textarea`, the widget it replaced: the same
//! keys must give the same text, cursor and screen.

use ratatui::style::Modifier;
use ratatui_textarea::{Input, Key as TaKey, TextArea, WrapMode};

use super::*;

const CURSOR: Style = Style::new().add_modifier(Modifier::REVERSED);

/// The mapping compose used to feed the textarea.
fn ta_input(key: &Key) -> Option<Input> {
    let k = match key.name {
        KeyName::Char(c) => TaKey::Char(c),
        KeyName::Enter => TaKey::Enter,
        KeyName::Backspace => TaKey::Backspace,
        KeyName::Delete => TaKey::Delete,
        KeyName::Left => TaKey::Left,
        KeyName::Right => TaKey::Right,
        KeyName::Up => TaKey::Up,
        KeyName::Down => TaKey::Down,
        KeyName::Home => TaKey::Home,
        KeyName::End => TaKey::End,
        KeyName::PageUp => TaKey::PageUp,
        KeyName::PageDown => TaKey::PageDown,
        KeyName::Tab => TaKey::Tab,
        KeyName::F(n) => TaKey::F(n),
        KeyName::Esc | KeyName::BackTab | KeyName::Insert => return None,
    };
    Some(Input {
        key: k,
        ctrl: key.ctrl,
        alt: key.alt,
        shift: key.shift,
    })
}

fn reference(text: &str) -> TextArea<'static> {
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let mut t = TextArea::new(lines);
    t.set_wrap_mode(WrapMode::WordOrGlyph);
    t.set_cursor_line_style(Style::default());
    t.set_max_histories(HISTORY);
    t.set_cursor_style(CURSOR);
    t
}

fn ours(text: &str) -> Editor {
    let mut e = Editor::new(text);
    e.set_cursor_style(CURSOR);
    e
}

fn k(name: KeyName, ctrl: bool, alt: bool, shift: bool) -> Key {
    Key {
        name,
        ctrl,
        alt,
        shift,
    }
}

/// Every key the body editor sees in compose. Ctrl-z and Ctrl-y never
/// reach it (compose maps them to undo and redo), and Tab, Esc and
/// `Ctrl-o` belong to the keymap; Tab is still here because the editor
/// handles it the same when a user unbinds it.
fn keys() -> Vec<Key> {
    use KeyName as N;
    let mut v = Vec::new();
    for c in ['a', 'b', ' ', 'x', '-', '.', '犬', 'é', '\t'] {
        v.push(Key::char(c));
    }
    for name in [
        N::Enter,
        N::Backspace,
        N::Delete,
        N::Left,
        N::Right,
        N::Up,
        N::Down,
        N::Home,
        N::End,
        N::PageUp,
        N::PageDown,
        N::Tab,
    ] {
        v.push(k(name, false, false, false));
        v.push(k(name, false, false, true));
    }
    for name in [N::Left, N::Right, N::Up, N::Down] {
        v.push(k(name, true, false, false));
        v.push(k(name, true, true, false));
        v.push(k(name, true, false, true));
    }
    for c in [
        'a', 'e', 'f', 'b', 'n', 'p', 'h', 'd', 'k', 'j', 'w', 'u', 'r', 'x', 'c', 'm', 'v',
    ] {
        v.push(Key::ctrl(c));
    }
    for c in ['f', 'b', 'n', 'p'] {
        v.push(k(N::Char(c), false, false, true));
        v.push(k(N::Char(c), true, false, true));
    }
    for c in ['d', 'h', 'f', 'b', '<', '>', ']', '[', 'n', 'p', 'v'] {
        v.push(k(N::Char(c), false, true, false));
    }
    v.push(k(N::Backspace, false, true, false));
    v.push(k(N::Delete, false, true, false));
    v
}

/// A small seeded generator, so no random crate is needed and failures
/// repeat.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }
}

const TEXTS: &[&str] = &[
    "",
    "hello world",
    "Line one is long enough to wrap at the narrow widths\n\nthird line\n",
    "a\tb\tc\n\tindent\nabcdefghijklmnopqrstuvwxyz0123456789\n",
    "犬猫 wide 中文字符和ASCII mixed e\u{301}x\n> quoted line\n-- \nsig",
    "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve",
    "x  y   z    spaces   and.punct,here!(ok)\n\n\npara two line one\npara two line two",
];

fn render_ta(t: &TextArea<'_>, area: Rect) -> Buffer {
    let mut b = Buffer::empty(area);
    t.render(area, &mut b);
    b
}

fn render_ours(e: &Editor, area: Rect) -> (Buffer, (usize, usize)) {
    let mut b = Buffer::empty(area);
    let at = e.render(area, &mut b);
    (b, at)
}

/// The first screen row as a count of rows from the start of the text,
/// which is how the textarea keeps it.
fn absolute_top(e: &Editor) -> usize {
    let (row, i) = e.top.get();
    (0..row).map(|r| e.segs(r).len()).sum::<usize>() + i
}

fn set_absolute_top(e: &Editor, mut n: usize) {
    for r in 0..e.lines.len() {
        let rows = e.segs(r).len();
        if n < rows {
            e.top.set((r, n));
            return;
        }
        n -= rows;
    }
}

#[test]
fn same_as_the_textarea_for_random_keys() {
    let keys = keys();
    let sizes = [(12u16, 6u16), (7, 3), (30, 12), (4, 2)];
    let mut rng = Lcg(0x6d65_6a6c);
    let mut steps = 0;
    let mut other_top = 0;
    for (ti, text) in TEXTS.iter().enumerate() {
        for &(w, h) in &sizes {
            let area = Rect::new(0, 0, w, h);
            let mut t = reference(text);
            let mut e = ours(text);
            render_ta(&t, area);
            render_ours(&e, area);
            let mut log: Vec<Key> = Vec::new();
            for step in 0..400 {
                let key = keys[rng.next() % keys.len()];
                log.push(key);
                let changed_t = ta_input(&key).is_some_and(|i| t.input(i));
                let changed_e = e.input(&key);
                let ctx = || {
                    format!(
                        "text {ti} at {w}x{h}, step {step}, last keys {:?}, lines {:?}",
                        &log[log.len().saturating_sub(4)..],
                        e.lines(),
                    )
                };
                assert_eq!(e.lines(), t.lines(), "{}", ctx());
                assert_eq!(changed_e, changed_t, "changed flag, {}", ctx());
                let tc = t.cursor();
                assert_eq!(e.cursor(), (tc.0, tc.1), "cursor, {}", ctx());
                let tb = render_ta(&t, area);
                let (mut eb, mut at) = render_ours(&e, area);
                let (top, _) = t.scroll_offset();
                steps += 1;
                if absolute_top(&e) != usize::from(top) {
                    // The textarea keeps the first row as a row number, so
                    // when lines above it split or join it shows other
                    // text there. The editor keeps the line. Both must
                    // show the cursor; then line them up and go on.
                    other_top += 1;
                    assert!(at.0 < usize::from(h), "cursor on screen, {}", ctx());
                    if at.1 < usize::from(w) {
                        let cell = &eb[(at.1 as u16, at.0 as u16)];
                        assert!(
                            cell.modifier.contains(Modifier::REVERSED),
                            "cursor drawn at {at:?}, {}",
                            ctx()
                        );
                    }
                    set_absolute_top(&e, usize::from(top));
                    (eb, at) = render_ours(&e, area);
                }
                assert_eq!(eb, tb, "screen, {}", ctx());
                let sc = t.screen_cursor();
                assert_eq!(
                    at,
                    (sc.row - usize::from(top), sc.col),
                    "screen cursor, {}",
                    ctx()
                );
            }
        }
    }
    // The first row differs only after rare edits above it.
    assert!(other_top * 20 < steps, "{other_top} of {steps}");
}

#[test]
fn compose_helpers_match_the_textarea() {
    let text = "first\nsecond line\nthird";
    let mut t = reference(text);
    let mut e = ours(text);
    t.move_cursor(ratatui_textarea::CursorMove::Bottom);
    t.move_cursor(ratatui_textarea::CursorMove::End);
    e.move_cursor(Move::Bottom);
    e.move_cursor(Move::End);
    t.insert_newline();
    e.insert_newline();
    assert!(t.insert_str("quoted\r\nmore"));
    assert!(e.insert_str("quoted\r\nmore"));
    t.move_cursor(ratatui_textarea::CursorMove::Jump(1, 3));
    e.move_cursor(Move::Jump(1, 3));
    assert_eq!(e.lines(), t.lines());
    assert_eq!(e.cursor(), (t.cursor().0, t.cursor().1));
    t.select_all();
    e.select_all();
    assert!(t.cut());
    assert!(e.cut());
    assert!(e.insert_str("new body"));
    assert!(t.insert_str("new body"));
    assert_eq!(e.lines(), t.lines());
    assert!(!e.insert_str(""));
    for _ in 0..3 {
        assert_eq!(e.undo(), t.undo());
        assert_eq!(e.lines(), t.lines());
        assert_eq!(e.cursor(), (t.cursor().0, t.cursor().1));
    }
    assert_eq!(e.redo(), t.redo());
    assert_eq!(e.lines(), t.lines());
}

#[test]
fn undo_keeps_two_hundred_steps() {
    let mut e = ours("");
    for _ in 0..250 {
        e.input(&Key::char('a'));
    }
    let mut n = 0;
    while e.undo() {
        n += 1;
    }
    assert_eq!(n, HISTORY);
    assert_eq!(e.lines(), ["a".repeat(50)]);
    assert!(e.redo());
}

#[test]
fn wrap_rules() {
    let pieces = |line: &str, w: usize| -> Vec<String> {
        wrap(line, w)
            .iter()
            .map(|s| line[s.start..s.end].to_owned())
            .collect()
    };
    assert_eq!(pieces("helloworld", 4), ["hell", "owor", "ld"]);
    assert_eq!(pieces("ab犬猫", 4), ["ab犬", "猫"]);
    assert_eq!(pieces("\tX", 2), ["\t", "X"]);
    assert_eq!(pieces("the quick brown", 10), ["the quick ", "brown"]);
    assert_eq!(pieces("", 10), [""]);
    assert_eq!(
        pieces("no wrap at zero width", 0),
        ["no wrap at zero width"]
    );
}

#[test]
fn edits_far_down_a_long_body_touch_only_their_lines() {
    let text: String = (0..5_000)
        .map(|i| format!("line {i} with some words\n"))
        .collect();
    let mut e = ours(&text);
    let area = Rect::new(0, 0, 20, 10);
    e.move_cursor(Move::Jump(4_000, 4));
    render_ours(&e, area);
    e.input(&k(KeyName::Enter, false, false, false));
    e.input(&Key::char('X'));
    let (b, at) = render_ours(&e, area);
    assert_eq!(e.lines()[4_000], "line");
    assert_eq!(e.lines()[4_001], "X 4000 with some words");
    assert_eq!(e.cursor(), (4_001, 1));
    let row: String = (0..20)
        .map(|x| b[(x, at.0 as u16)].symbol().to_owned())
        .collect();
    assert!(row.starts_with("X 4000"), "{row:?}");
    assert!(e.undo() && e.undo());
    assert_eq!(e.lines()[4_000], "line 4000 with some words");
    assert_eq!(e.lines().len(), 5_001);
}
