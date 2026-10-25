//! The compose body editor: lines of text, a cursor, a selection, undo
//! and redo, and soft wrap by display width.
//!
//! It replaces `ratatui-textarea` for the body. That widget re-wraps every
//! line of the buffer after each edit, so typing slowed down in proportion
//! to the length of the body and passed the frame budget at about 800
//! lines. Here an edit only touches the lines it changes. Drawing, scrolling
//! and moving the cursor by screen rows wrap only the lines on screen and
//! the ones next to the cursor, so a key costs the same at 50 lines and at
//! 50 000.
//!
//! The keys, the wrap rules, the drawing and the undo steps follow the
//! textarea's Emacs-like defaults, so nothing changes for the user. The
//! tests drive both editors with the same keys and compare them.
//!
//! Positions are `(row, column)` in characters, like the textarea's.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::keymap::{Key, KeyName};

/// Display width of a tab stop.
const TAB: usize = 4;
/// Undo steps kept.
const HISTORY: usize = 200;

/// A position in the text: row, then column in characters.
pub(crate) type Pos = (usize, usize);

/// Cursor moves the compose code asks for directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    /// First line, keeping the column where it fits.
    Top,
    /// Last line, keeping the column where it fits.
    Bottom,
    /// Start of the line.
    Head,
    /// End of the line.
    End,
    /// A position, clamped to the text.
    Jump(usize, usize),
}

/// The moves the keys make. Rows here are screen rows after wrapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Forward,
    Back,
    Up,
    Down,
    Head,
    End,
    Top,
    Bottom,
    WordForward,
    WordBack,
    ParagraphForward,
    ParagraphBack,
}

/// One wrapped piece of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seg {
    start: usize,
    end: usize,
    start_col: usize,
    end_col: usize,
    last: bool,
}

/// One undo step: the text between `at` and the end of `removed` was
/// replaced by `inserted`. Each list is the text split at line breaks.
#[derive(Clone, Debug)]
struct Edit {
    at: Pos,
    removed: Vec<String>,
    inserted: Vec<String>,
    before: Pos,
    after: Pos,
}

/// The body editor.
#[derive(Clone, Debug)]
pub(crate) struct Editor {
    lines: Vec<String>,
    /// Words on each line, kept with the lines so the status line does
    /// not rescan the body on every frame.
    words: Vec<u32>,
    cursor: Pos,
    selection: Option<Pos>,
    history: VecDeque<Edit>,
    /// Steps in `history` that are applied; the rest can be redone.
    applied: usize,
    cursor_style: Style,
    select_style: Style,
    /// The first screen row drawn: a line and a wrapped piece of it.
    top: Cell<(usize, usize)>,
    /// Width and height of the last drawn area. Wrapping and paging use
    /// them; zero before the first draw means no wrapping.
    size: Cell<(u16, u16)>,
}

fn char_width(c: char, col: usize) -> usize {
    if c == '\t' {
        TAB - col % TAB
    } else {
        c.width().unwrap_or(0)
    }
}

/// Display width of text that starts at column `from`, as the end column.
fn width_to(text: &str, mut col: usize) -> usize {
    for c in text.chars() {
        col += char_width(c, col);
    }
    col
}

fn width(text: &str) -> usize {
    width_to(text, 0)
}

fn col_of_offset(text: &str, chars: usize) -> usize {
    let mut col = 0;
    for c in text.chars().take(chars) {
        col += char_width(c, col);
    }
    col
}

fn offset_of_col(text: &str, screen_col: usize) -> usize {
    let mut col = 0;
    let mut chars = 0;
    for c in text.chars() {
        if col >= screen_col {
            break;
        }
        let w = char_width(c, col);
        if col + w > screen_col {
            break;
        }
        col += w;
        chars += 1;
    }
    chars
}

/// Byte offset of a character column, or the end of the line.
fn byte_of(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

fn count_words(line: &str) -> u32 {
    u32::try_from(line.split_whitespace().count()).unwrap_or(u32::MAX)
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Splits `start..end` of `line` into pieces at most `max` columns wide,
/// by grapheme.
fn split_glyphs(line: &str, start: usize, end: usize, max: usize, out: &mut Vec<(usize, usize)>) {
    let mut seg_start = start;
    while seg_start < end {
        let mut seg_end = seg_start;
        let mut seg_width = 0;
        for (offset, g) in line[seg_start..end].grapheme_indices(true) {
            let g_end = seg_start + offset + g.len();
            let next = width_to(g, seg_width);
            let g_width = next.saturating_sub(seg_width);
            if seg_end != seg_start && seg_width + g_width > max {
                break;
            }
            seg_end = g_end;
            seg_width = next;
            if seg_width > max {
                break;
            }
        }
        if seg_end == seg_start {
            match line[seg_start..end].chars().next() {
                Some(c) => seg_end = seg_start + c.len_utf8(),
                None => break,
            }
        }
        out.push((seg_start, seg_end));
        seg_start = seg_end;
    }
}

/// Byte ranges of a line wrapped at word boundaries, falling back to
/// graphemes for words wider than `max`.
fn wrap_ranges(line: &str, max: usize) -> Vec<(usize, usize)> {
    let max = max.max(1);
    let words: Vec<(usize, usize)> = line
        .split_word_bound_indices()
        .map(|(s, t)| (s, s + t.len()))
        .collect();
    let Some(first) = words.first() else {
        return vec![(0, 0)];
    };
    let mut out = Vec::new();
    let mut i = 0;
    let mut seg_start = first.0;
    let mut seg_end = seg_start;
    let mut seg_width = 0;
    while let Some(&(w_start, w_end)) = words.get(i) {
        if seg_end == seg_start {
            seg_start = w_start;
        }
        let w = width_to(&line[w_start..w_end], seg_width).saturating_sub(seg_width);
        if seg_width + w <= max {
            seg_end = w_end;
            seg_width += w;
            i += 1;
            continue;
        }
        if seg_end > seg_start {
            out.push((seg_start, seg_end));
            seg_start = seg_end;
            seg_width = 0;
            continue;
        }
        split_glyphs(line, w_start, w_end, max, &mut out);
        i += 1;
        seg_start = w_end;
        seg_end = w_end;
        seg_width = 0;
    }
    if seg_end > seg_start {
        out.push((seg_start, seg_end));
    }
    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

/// The wrapped pieces of a line. Width zero means no wrapping.
fn wrap(line: &str, max: usize) -> Vec<Seg> {
    let ranges = if max == 0 {
        vec![(0, line.len())]
    } else {
        wrap_ranges(line, max)
    };
    let n = ranges.len();
    let mut col = 0;
    ranges
        .into_iter()
        .enumerate()
        .map(|(i, (start, end))| {
            let end_col = col + chars(&line[start..end]);
            let s = Seg {
                start,
                end,
                start_col: col,
                end_col,
                last: i + 1 == n,
            };
            col = end_col;
            s
        })
        .collect()
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CharKind {
    Space,
    Punct,
    Other,
}

impl CharKind {
    fn of(c: char) -> CharKind {
        if c.is_whitespace() {
            CharKind::Space
        } else if c != '_' && c.is_ascii_punctuation() {
            CharKind::Punct
        } else {
            CharKind::Other
        }
    }
}

fn word_start_forward(line: &str, from: usize) -> Option<usize> {
    let mut it = line.chars().enumerate().skip(from);
    let mut prev = CharKind::of(it.next()?.1);
    for (col, c) in it {
        let cur = CharKind::of(c);
        if cur != CharKind::Space && prev != cur {
            return Some(col);
        }
        prev = cur;
    }
    None
}

fn word_end_forward(line: &str, from: usize) -> Option<usize> {
    let mut it = line.chars().enumerate().skip(from);
    let mut prev = CharKind::of(it.next()?.1);
    for (col, c) in it {
        let cur = CharKind::of(c);
        if prev != CharKind::Space && prev != cur {
            return Some(col);
        }
        prev = cur;
    }
    None
}

fn word_start_backward(line: &str, from: usize) -> Option<usize> {
    let idx = byte_of(line, from);
    let mut it = line[..idx].chars().rev().enumerate();
    let mut cur = CharKind::of(it.next()?.1);
    for (i, c) in it {
        let next = CharKind::of(c);
        if cur != CharKind::Space && next != cur {
            return Some(from - i);
        }
        cur = next;
    }
    (cur != CharKind::Space).then_some(0)
}

/// Where text ends when it is put at `at`.
fn end_of(at: Pos, text: &[String]) -> Pos {
    match text {
        [one] => (at.0, at.1 + chars(one)),
        [.., last] => (at.0 + text.len() - 1, chars(last)),
        [] => at,
    }
}

impl Editor {
    /// An editor holding `text`, cursor at the start.
    pub(crate) fn new(text: &str) -> Editor {
        let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        Editor {
            words: lines.iter().map(|l| count_words(l)).collect(),
            lines,
            cursor: (0, 0),
            selection: None,
            history: VecDeque::new(),
            applied: 0,
            cursor_style: Style::default(),
            select_style: Style::default().bg(Color::LightBlue),
            top: Cell::new((0, 0)),
            size: Cell::new((0, 0)),
        }
    }

    /// The lines of text.
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Words on each line, split at whitespace.
    pub(crate) fn line_words(&self) -> &[u32] {
        &self.words
    }

    /// The cursor as (row, column in characters).
    pub(crate) fn cursor(&self) -> Pos {
        self.cursor
    }

    /// The style of the cursor cell.
    pub(crate) fn set_cursor_style(&mut self, style: Style) {
        self.cursor_style = style;
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |l| chars(l))
    }

    fn wrap_width(&self) -> usize {
        usize::from(self.size.get().0)
    }

    fn segs(&self, row: usize) -> Vec<Seg> {
        match self.lines.get(row) {
            Some(l) => wrap(l, self.wrap_width()),
            None => vec![Seg {
                start: 0,
                end: 0,
                start_col: 0,
                end_col: 0,
                last: true,
            }],
        }
    }

    fn frag(&self, row: usize, s: &Seg) -> &str {
        self.lines
            .get(row)
            .and_then(|l| l.get(s.start..s.end))
            .unwrap_or("")
    }

    /// The piece of the line holding `col`, and the cursor's screen column
    /// in it.
    fn locate(&self, (row, col): Pos) -> (Vec<Seg>, usize, usize) {
        let segs = self.segs(row);
        let i = segs
            .iter()
            .position(|s| {
                if s.last {
                    s.start_col <= col && col <= s.end_col
                } else {
                    s.start_col <= col && col < s.end_col
                }
            })
            .unwrap_or(0);
        let s = segs[i];
        let sc = col_of_offset(self.frag(row, &s), col.saturating_sub(s.start_col));
        (segs, i, sc)
    }

    /// The column the cursor can reach on a screen row: the end for the
    /// last piece of a line, before the last character otherwise.
    fn max_col(&self, row: usize, s: &Seg) -> usize {
        let f = self.frag(row, s);
        if s.last {
            width(f)
        } else {
            col_of_offset(f, (s.end_col - s.start_col).saturating_sub(1))
        }
    }

    fn to_data(&self, row: usize, s: &Seg, screen_col: usize) -> Pos {
        (
            row,
            s.start_col + offset_of_col(self.frag(row, s), screen_col),
        )
    }

    /// The screen row before (row, piece), if any.
    fn prev_row(&self, row: usize, i: usize) -> Option<(usize, Seg, usize)> {
        if i > 0 {
            let segs = self.segs(row);
            return Some((row, segs[i - 1], i - 1));
        }
        let row = row.checked_sub(1)?;
        let segs = self.segs(row);
        let i = segs.len() - 1;
        Some((row, segs[i], i))
    }

    /// The screen row after (row, piece), if any.
    fn next_row(&self, row: usize, i: usize, count: usize) -> Option<(usize, Seg, usize)> {
        if i + 1 < count {
            let segs = self.segs(row);
            return Some((row, segs[i + 1], i + 1));
        }
        if row + 1 >= self.lines.len() {
            return None;
        }
        let segs = self.segs(row + 1);
        Some((row + 1, segs[0], 0))
    }

    fn step(&self, m: Step) -> Option<Pos> {
        let (row, col) = self.cursor;
        let line = self.lines.get(row).map_or("", String::as_str);
        match m {
            Step::Forward => {
                let (segs, i, sc) = self.locate(self.cursor);
                if sc >= width(self.frag(row, &segs[i])) {
                    self.next_row(row, i, segs.len())
                        .map(|(r, s, _)| self.to_data(r, &s, 0))
                } else {
                    Some((row, col + 1))
                }
            }
            Step::Back => {
                let (_, i, sc) = self.locate(self.cursor);
                if sc == 0 {
                    let (r, s, _) = self.prev_row(row, i)?;
                    Some(self.to_data(r, &s, self.max_col(r, &s)))
                } else {
                    Some((row, col.saturating_sub(1)))
                }
            }
            Step::Up => {
                let (_, i, sc) = self.locate(self.cursor);
                let (r, s, _) = self.prev_row(row, i)?;
                Some(self.to_data(r, &s, sc.min(self.max_col(r, &s))))
            }
            Step::Down => {
                let (segs, i, sc) = self.locate(self.cursor);
                let (r, s, _) = self.next_row(row, i, segs.len())?;
                Some(self.to_data(r, &s, sc.min(self.max_col(r, &s))))
            }
            Step::Head => Some((row, 0)),
            Step::End => Some((row, chars(line))),
            Step::Top => Some((0, col.min(self.line_len(0)))),
            Step::Bottom => {
                let last = self.lines.len() - 1;
                Some((last, col.min(self.line_len(last))))
            }
            Step::WordForward => Some(match word_start_forward(line, col) {
                Some(c) => (row, c),
                None if row + 1 < self.lines.len() => (row + 1, 0),
                None => (row, chars(line)),
            }),
            Step::WordBack => Some(match word_start_backward(line, col) {
                Some(c) => (row, c),
                None if row > 0 => (row - 1, self.line_len(row - 1)),
                None => (row, 0),
            }),
            Step::ParagraphForward => {
                let mut prev_empty = line.is_empty();
                for r in row + 1..self.lines.len() {
                    let empty = self.lines[r].is_empty();
                    if !empty && prev_empty {
                        return Some((r, col.min(self.line_len(r))));
                    }
                    prev_empty = empty;
                }
                let last = self.lines.len() - 1;
                Some((last, col.min(self.line_len(last))))
            }
            Step::ParagraphBack => {
                let start = row.checked_sub(1)?;
                let mut prev_empty = self.lines[start].is_empty();
                for r in (0..start).rev() {
                    let empty = self.lines[r].is_empty();
                    if empty && !prev_empty {
                        return Some((r + 1, col.min(self.line_len(r + 1))));
                    }
                    prev_empty = empty;
                }
                Some((0, col.min(self.line_len(0))))
            }
        }
    }

    fn go(&mut self, m: Step, shift: bool) {
        if let Some(next) = self.step(m) {
            self.place(next, shift);
        }
    }

    fn place(&mut self, next: Pos, shift: bool) {
        if shift {
            if self.selection.is_none() {
                self.selection = Some(self.cursor);
            }
        } else {
            self.selection = None;
        }
        self.cursor = next;
    }

    /// Moves the cursor, keeping a selection that is in progress.
    pub(crate) fn move_cursor(&mut self, m: Move) {
        let shift = self.selection.is_some();
        let next = match m {
            Move::Top => self.step(Step::Top),
            Move::Bottom => self.step(Step::Bottom),
            Move::Head => self.step(Step::Head),
            Move::End => self.step(Step::End),
            Move::Jump(row, col) => {
                let row = row.min(self.lines.len() - 1);
                Some((row, col.min(self.line_len(row))))
            }
        };
        if let Some(next) = next {
            self.place(next, shift);
        }
    }

    /// Selects everything, cursor at the end.
    pub(crate) fn select_all(&mut self) {
        self.move_cursor(Move::Jump(usize::MAX, usize::MAX));
        self.selection = Some((0, 0));
    }

    // Editing.

    /// Replaces the text from `at` to `end` with `text` and returns what
    /// was there. Keeps the first screen row on the same text.
    fn splice(&mut self, at: Pos, end: Pos, text: &[String]) -> Vec<String> {
        let so = byte_of(&self.lines[at.0], at.1);
        let eo = byte_of(&self.lines[end.0], end.1);
        let removed: Vec<String> = if at.0 == end.0 {
            vec![self.lines[at.0][so..eo].to_owned()]
        } else {
            let mut v = vec![self.lines[at.0][so..].to_owned()];
            v.extend(self.lines[at.0 + 1..end.0].iter().cloned());
            v.push(self.lines[end.0][..eo].to_owned());
            v
        };
        let head = &self.lines[at.0][..so];
        let tail = &self.lines[end.0][eo..];
        let mut new: Vec<String> = text.to_vec();
        if new.is_empty() {
            new.push(String::new());
        }
        new[0].insert_str(0, head);
        if let Some(last) = new.last_mut() {
            last.push_str(tail);
        }
        let added = new.len();
        let counts: Vec<u32> = new.iter().map(|l| count_words(l)).collect();
        self.words.splice(at.0..=end.0, counts);
        self.lines.splice(at.0..=end.0, new);
        let (top_row, top_seg) = self.top.get();
        if top_row > end.0 {
            let row = (top_row + added).saturating_sub(end.0 - at.0 + 1);
            self.top.set((row, top_seg));
        } else if top_row > at.0 {
            self.top.set((at.0, 0));
        }
        removed
    }

    /// Applies an edit and records it for undo.
    fn edit(&mut self, at: Pos, end: Pos, text: Vec<String>, before: Pos, after: Pos) {
        let removed = self.splice(at, end, &text);
        self.cursor = after;
        if self.history.len() == HISTORY {
            self.history.pop_front();
            self.applied = self.applied.saturating_sub(1);
        }
        self.history.truncate(self.applied);
        self.history.push_back(Edit {
            at,
            removed,
            inserted: text,
            before,
            after,
        });
        self.applied += 1;
    }

    fn put_cursor(&mut self, (row, col): Pos) {
        self.selection = None;
        let row = row.min(self.lines.len().saturating_sub(1));
        self.cursor = (row, col.min(self.line_len(row)));
    }

    /// Undoes the last edit. False when there is none.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(i) = self.applied.checked_sub(1) else {
            return false;
        };
        let e = self.history[i].clone();
        let end = end_of(e.at, &e.inserted);
        self.splice(e.at, end, &e.removed);
        self.applied = i;
        self.put_cursor(e.before);
        true
    }

    /// Redoes the last undone edit. False when there is none.
    pub(crate) fn redo(&mut self) -> bool {
        let Some(e) = self.history.get(self.applied).cloned() else {
            return false;
        };
        let end = end_of(e.at, &e.removed);
        self.splice(e.at, end, &e.inserted);
        self.applied += 1;
        self.put_cursor(e.after);
        true
    }

    fn selection_range(&self) -> Option<(Pos, Pos)> {
        let s = self.selection?;
        let e = self.cursor;
        match s.cmp(&e) {
            Ordering::Less => Some((s, e)),
            Ordering::Equal => None,
            Ordering::Greater => Some((e, s)),
        }
    }

    /// Deletes the selection, if there is one.
    fn delete_selection(&mut self) -> bool {
        let range = self.selection_range();
        self.selection = None;
        match range {
            Some((s, e)) => {
                self.edit(s, e, vec![String::new()], e, s);
                true
            }
            None => false,
        }
    }

    /// Cuts the selection. True when text changed.
    pub(crate) fn cut(&mut self) -> bool {
        self.delete_selection()
    }

    fn insert_char(&mut self, c: char) {
        self.delete_selection();
        let (row, col) = self.cursor;
        self.edit(
            (row, col),
            (row, col),
            vec![c.to_string()],
            (row, col),
            (row, col + 1),
        );
    }

    /// Splits the line at the cursor.
    pub(crate) fn insert_newline(&mut self) {
        self.delete_selection();
        let at = self.cursor;
        self.edit(
            at,
            at,
            vec![String::new(), String::new()],
            at,
            (at.0 + 1, 0),
        );
    }

    fn insert_tab(&mut self) -> bool {
        let changed = self.delete_selection();
        let (row, col) = self.cursor;
        let w: usize = self.lines[row]
            .chars()
            .take(col)
            .map(|c| c.width().unwrap_or(0))
            .sum();
        let pad = TAB - w % TAB;
        self.insert_text(vec![" ".repeat(pad)]) || changed
    }

    fn insert_text(&mut self, text: Vec<String>) -> bool {
        if text.len() == 1 && text[0].is_empty() {
            return false;
        }
        let at = self.cursor;
        let after = end_of(at, &text);
        self.edit(at, at, text, at, after);
        true
    }

    /// Inserts text at the cursor; `\n` and `\r\n` break lines. True when
    /// text changed.
    pub(crate) fn insert_str(&mut self, s: &str) -> bool {
        // Like the textarea, inserting nothing reports no change even
        // when a selection went.
        self.delete_selection();
        let text: Vec<String> = s
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
            .collect();
        self.insert_text(text)
    }

    fn delete_newline(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let row = self.cursor.0;
        if row == 0 {
            return false;
        }
        let prev = (row - 1, self.line_len(row - 1));
        self.edit(prev, (row, 0), vec![String::new()], (row, 0), prev);
        true
    }

    fn delete_char(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let (row, col) = self.cursor;
        if col == 0 {
            return self.delete_newline();
        }
        if col > self.line_len(row) {
            return false;
        }
        self.edit(
            (row, col - 1),
            (row, col),
            vec![String::new()],
            (row, col),
            (row, col - 1),
        );
        true
    }

    fn delete_next_char(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let before = self.cursor;
        self.go(Step::Forward, false);
        if before == self.cursor {
            return false;
        }
        self.delete_char()
    }

    /// Deletes up to `n` characters from `col` on the cursor's line. The
    /// cursor goes to `col`.
    fn delete_piece(&mut self, col: usize, n: usize) -> bool {
        let row = self.cursor.0;
        let len = self.line_len(row);
        if n == 0 || col >= len {
            return false;
        }
        let end = (row, col + n.min(len - col));
        self.edit((row, col), end, vec![String::new()], end, (row, col));
        true
    }

    fn delete_to_end(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.delete_piece(self.cursor.1, usize::MAX) {
            return true;
        }
        self.delete_next_char()
    }

    fn delete_to_head(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.delete_piece(0, self.cursor.1) {
            return true;
        }
        self.delete_newline()
    }

    fn delete_word(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let (row, col) = self.cursor;
        match word_start_backward(&self.lines[row], col) {
            Some(start) => self.delete_piece(start, col - start),
            None if col > 0 => self.delete_piece(0, col),
            None => self.delete_newline(),
        }
    }

    fn delete_next_word(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let (row, col) = self.cursor;
        let len = self.line_len(row);
        match word_end_forward(&self.lines[row], col) {
            Some(end) => self.delete_piece(col, end - col),
            None if col < len => self.delete_piece(col, len - col),
            None if row + 1 < self.lines.len() => {
                self.cursor = (row + 1, 0);
                self.delete_newline()
            }
            None => false,
        }
    }

    // Scrolling.

    fn rows_after(&self, (row, i): (usize, usize), n: usize) -> (usize, usize) {
        let mut at = (row, i);
        let mut count = self.segs(row).len();
        for _ in 0..n {
            match self.next_row(at.0, at.1, count) {
                Some((r, _, j)) => {
                    if r != at.0 {
                        count = self.segs(r).len();
                    }
                    at = (r, j);
                }
                None => break,
            }
        }
        at
    }

    fn rows_before(&self, (row, i): (usize, usize), n: usize) -> (usize, usize) {
        let mut at = (row, i);
        for _ in 0..n {
            match self.prev_row(at.0, at.1) {
                Some((r, _, j)) => at = (r, j),
                None => break,
            }
        }
        at
    }

    /// The first screen row, checked against the text.
    fn top_row(&self) -> (usize, usize) {
        let (row, i) = self.top.get();
        let row = row.min(self.lines.len() - 1);
        (row, i.min(self.segs(row).len() - 1))
    }

    /// Screen rows from `from` to `to`, counting at most `limit`.
    fn distance(&self, from: (usize, usize), to: (usize, usize), limit: usize) -> usize {
        let mut at = from;
        let mut n = 0;
        let mut count = self.segs(at.0).len();
        while at < to && n < limit {
            match self.next_row(at.0, at.1, count) {
                Some((r, _, j)) => {
                    if r != at.0 {
                        count = self.segs(r).len();
                    }
                    at = (r, j);
                    n += 1;
                }
                None => break,
            }
        }
        n
    }

    /// Scrolls a page and keeps the cursor on screen.
    fn page(&mut self, down: bool, shift: bool) {
        if shift && self.selection.is_none() {
            self.selection = Some(self.cursor);
        }
        let (w, h) = self.size.get();
        let h = usize::from(h);
        let top = self.top_row();
        let top = if down {
            self.rows_after(top, h)
        } else {
            self.rows_before(top, h)
        };
        self.top.set(top);
        // Keep the cursor between the first and last rows on screen.
        let (_, i, sc) = self.locate(self.cursor);
        let at = (self.cursor.0, i);
        let bottom = self.rows_after(top, h.saturating_sub(1));
        let target = if at < top {
            top
        } else if at > bottom {
            bottom
        } else {
            at
        };
        let s = self.segs(target.0)[target.1];
        let sc = sc.min(usize::from(w).saturating_sub(1));
        let sc = sc.min(self.max_col(target.0, &s));
        let next = self.to_data(target.0, &s, sc);
        self.place(next, shift);
    }

    // Keys.

    /// Handles a key with the textarea's Emacs-like table. True when the
    /// text changed.
    pub(crate) fn input(&mut self, key: &Key) -> bool {
        use KeyName as N;
        let (ctrl, alt, shift) = (key.ctrl, key.alt, key.shift);
        let plain = !ctrl && !alt;
        let only_ctrl = ctrl && !alt;
        let only_alt = !ctrl && alt;
        let both = ctrl && alt;
        let mv = |e: &mut Editor, m: Step| {
            e.go(m, shift);
            false
        };
        match key.name {
            N::Char('m') if only_ctrl => {
                self.insert_newline();
                true
            }
            N::Enter => {
                self.insert_newline();
                true
            }
            N::Char(c) if plain => {
                self.insert_char(c);
                true
            }
            N::Tab if plain => self.insert_tab(),
            N::Char('h') if only_ctrl => self.delete_char(),
            N::Backspace if plain => self.delete_char(),
            N::Char('d') if only_ctrl => self.delete_next_char(),
            N::Delete if plain => self.delete_next_char(),
            N::Char('k') if only_ctrl => self.delete_to_end(),
            N::Char('j') if only_ctrl => self.delete_to_head(),
            N::Char('w') if only_ctrl => self.delete_word(),
            N::Char('h') | N::Backspace if only_alt => self.delete_word(),
            N::Delete | N::Char('d') if only_alt => self.delete_next_word(),
            N::Char('n') if only_ctrl => mv(self, Step::Down),
            N::Down if plain => mv(self, Step::Down),
            N::Char('p') if only_ctrl => mv(self, Step::Up),
            N::Up if plain => mv(self, Step::Up),
            N::Char('f') if only_ctrl => mv(self, Step::Forward),
            N::Right if plain => mv(self, Step::Forward),
            N::Char('b') if only_ctrl => mv(self, Step::Back),
            N::Left if plain => mv(self, Step::Back),
            N::Char('a') if only_ctrl => mv(self, Step::Head),
            N::Home => mv(self, Step::Head),
            N::Left | N::Char('b') if both => mv(self, Step::Head),
            N::Char('e') if only_ctrl => mv(self, Step::End),
            N::End => mv(self, Step::End),
            N::Right | N::Char('f') if both => mv(self, Step::End),
            N::Char('<') if only_alt => mv(self, Step::Top),
            N::Up | N::Char('p') if both => mv(self, Step::Top),
            N::Char('>') if only_alt => mv(self, Step::Bottom),
            N::Down | N::Char('n') if both => mv(self, Step::Bottom),
            N::Char('f') if only_alt => mv(self, Step::WordForward),
            N::Right if only_ctrl => mv(self, Step::WordForward),
            N::Char('b') if only_alt => mv(self, Step::WordBack),
            N::Left if only_ctrl => mv(self, Step::WordBack),
            N::Char(']' | 'n') if only_alt => mv(self, Step::ParagraphForward),
            N::Down if only_ctrl => mv(self, Step::ParagraphForward),
            N::Char('[' | 'p') if only_alt => mv(self, Step::ParagraphBack),
            N::Up if only_ctrl => mv(self, Step::ParagraphBack),
            N::Char('u') if only_ctrl => self.undo(),
            N::Char('r') if only_ctrl => self.redo(),
            N::Char('x') if only_ctrl => self.cut(),
            N::Char('c') if only_ctrl => {
                // Copy: there is no paste key in compose, so it only ends
                // the selection.
                self.selection = None;
                false
            }
            N::Char('v') if only_ctrl => {
                self.page(true, shift);
                false
            }
            N::PageDown => {
                self.page(true, shift);
                false
            }
            N::Char('v') if only_alt => {
                self.page(false, shift);
                false
            }
            N::PageUp => {
                self.page(false, shift);
                false
            }
            _ => false,
        }
    }

    // Drawing.

    /// Moves the first screen row so the cursor is on screen, as the
    /// textarea does: only as far as needed.
    fn follow(&self, h: usize) -> (usize, usize) {
        let top = self.top_row();
        let (_, i, _) = self.locate(self.cursor);
        let at = (self.cursor.0, i);
        let top = if at < top {
            at
        } else if self.distance(top, at, h) >= h {
            self.rows_before(at, h - 1)
        } else {
            top
        };
        self.top.set(top);
        top
    }

    /// Draws the text into `area` and returns the cursor's row and column
    /// inside it.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) -> (usize, usize) {
        self.size.set((area.width, area.height));
        let h = usize::from(area.height);
        if h == 0 || area.width == 0 {
            return (0, 0);
        }
        let top = self.follow(h);
        let selection = self.selection_range();
        let mut lines = Vec::with_capacity(h);
        let mut at = Some((top.0, self.segs(top.0), top.1));
        let mut cursor = (0, 0);
        while let Some((row, segs, i)) = at.take() {
            if lines.len() == h {
                break;
            }
            let s = segs[i];
            if row == self.cursor.0 && self.in_seg(&s, self.cursor.1) {
                let sc = col_of_offset(self.frag(row, &s), self.cursor.1 - s.start_col);
                cursor = (lines.len(), sc);
            }
            lines.push(self.seg_line(row, &s, i == 0, selection));
            at = if i + 1 < segs.len() {
                Some((row, segs, i + 1))
            } else if row + 1 < self.lines.len() {
                Some((row + 1, self.segs(row + 1), 0))
            } else {
                None
            };
        }
        Paragraph::new(Text::from(lines)).render(area, buf);
        cursor
    }

    fn in_seg(&self, s: &Seg, col: usize) -> bool {
        if s.last {
            s.start_col <= col && col <= s.end_col
        } else {
            s.start_col <= col && col < s.end_col
        }
    }

    /// One screen row with the cursor and selection styles, built like the
    /// textarea's line highlighter.
    fn seg_line(&self, row: usize, s: &Seg, first: bool, sel: Option<(Pos, Pos)>) -> Line<'_> {
        let line = self.lines.get(row).map_or("", String::as_str);
        let frag = self.frag(row, s);
        let base = Style::default();
        // (offset, rank, style); rank orders boundaries at one offset:
        // end 0, selection 1, cursor 3.
        let mut marks: Vec<(usize, u8, Option<Style>)> = Vec::new();
        let mut cursor_at_end = false;
        let mut select_at_end = false;
        if row == self.cursor.0 && self.in_seg(s, self.cursor.1) {
            match frag.char_indices().nth(self.cursor.1 - s.start_col) {
                Some((i, c)) => {
                    marks.push((i, 3, Some(self.cursor_style)));
                    marks.push((i + c.len_utf8(), 0, None));
                }
                None => cursor_at_end = true,
            }
        }
        if let Some((start, end)) = sel {
            let so = byte_of(&self.lines[start.0], start.1);
            let eo = byte_of(&self.lines[end.0], end.1);
            let (a, b) = if first && s.last {
                if row == start.0 {
                    if start.0 == end.0 {
                        (so, eo)
                    } else {
                        select_at_end = true;
                        (so, line.len())
                    }
                } else if row == end.0 {
                    (0, eo)
                } else if start.0 < row && row < end.0 {
                    select_at_end = true;
                    (0, line.len())
                } else {
                    (0, 0)
                }
            } else if start.0 <= row && row <= end.0 {
                let a = if start.0 == row { so } else { 0 };
                let b = if end.0 == row { eo } else { line.len() };
                let a = a.max(s.start);
                let b = b.min(s.end);
                if s.last && b == s.end && row < end.0 {
                    select_at_end = true;
                }
                (a.saturating_sub(s.start), b.saturating_sub(s.start))
            } else {
                (0, 0)
            };
            if a < b {
                marks.push((a, 1, Some(self.select_style)));
                marks.push((b, 0, None));
            }
        }
        let mut col = 0;
        let mut spans: Vec<Span<'_>> = Vec::new();
        if marks.is_empty() {
            piece(frag, base, &mut col, &mut spans);
        } else {
            marks.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
            let mut style = base;
            let mut stack = Vec::new();
            let mut start = 0;
            for (end, _, next) in marks {
                if start < end {
                    piece(&frag[start..end], style, &mut col, &mut spans);
                }
                style = match next {
                    Some(st) => {
                        stack.push(style);
                        st
                    }
                    None => stack.pop().unwrap_or(base),
                };
                start = end;
            }
            if start != frag.len() {
                piece(&frag[start..], style, &mut col, &mut spans);
            }
        }
        if cursor_at_end {
            spans.push(Span::styled(" ", self.cursor_style));
        } else if select_at_end {
            spans.push(Span::styled(" ", self.select_style));
        }
        Line::from(spans)
    }
}

/// Adds a span for `text` with tabs expanded to the next stop. `col` is
/// the screen column the text starts at.
fn piece(text: &str, style: Style, col: &mut usize, spans: &mut Vec<Span<'_>>) {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\t' {
            let pad = TAB - *col % TAB;
            out.extend(std::iter::repeat_n(' ', pad));
            *col += pad;
        } else {
            out.push(c);
            *col += c.width().unwrap_or(0);
        }
    }
    if !out.is_empty() {
        spans.push(Span::styled(out, style));
    }
}

#[cfg(test)]
mod tests;
