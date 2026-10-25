//! A single-line text input.
//!
//! Search bars, picker filters, header fields and prompts all use
//! [`LineInput`]. The cursor is a byte offset that always sits on a
//! grapheme boundary, so editing never splits a combining mark, an emoji
//! or a CJK character, and the visible window is measured in display
//! cells, the same way the frame is drawn.

use unicode_segmentation::UnicodeSegmentation;

use crate::keymap::{Key, KeyName};
use crate::text::{grapheme_width, width};

/// What a key did to a [`LineInput`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputResult {
    /// The text changed.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// The key is not an editing key; the caller may use it.
    Ignored,
}

/// One line of editable text with a cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    cursor: usize,
}

impl LineInput {
    /// An input holding `text`, with the cursor at the end.
    pub fn new(text: impl Into<String>) -> LineInput {
        let mut input = LineInput::default();
        input.set(text);
        input
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor as a byte offset into [`LineInput::text`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// True when there is no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text and puts the cursor at the end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = one_line(&text.into());
        self.cursor = self.text.len();
    }

    /// Empties the input.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Takes the text out, leaving the input empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Inserts text at the cursor. Line breaks and other control
    /// characters become spaces, because the input is one line.
    pub fn insert_str(&mut self, s: &str) {
        let clean = one_line(s);
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }

    fn word_back(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(' ');
        trimmed
            .rfind([' ', ',', ';'])
            .map_or(0, |i| i + 1)
            .min(self.cursor)
    }

    fn word_forward(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skip = after.len() - after.trim_start_matches(' ').len();
        let rest = &after[skip..];
        let end = rest.find([' ', ',', ';']).unwrap_or(rest.len());
        self.cursor + skip + end
    }

    /// Applies an editing key. Printable characters insert; Backspace,
    /// Delete, arrows, Home and End edit and move; Ctrl-a, Ctrl-e, Ctrl-u,
    /// Ctrl-k and Ctrl-w work as in a shell. Everything else is
    /// [`InputResult::Ignored`].
    pub fn handle(&mut self, key: &Key) -> InputResult {
        if let Some(c) = key.text() {
            let mut buf = [0u8; 4];
            self.insert_str(c.encode_utf8(&mut buf));
            return InputResult::Changed;
        }
        let ctrl = key.ctrl;
        let changed = |input: &mut LineInput, from: usize, to: usize| {
            if from == to {
                return InputResult::Moved;
            }
            input.text.replace_range(from..to, "");
            input.cursor = from;
            InputResult::Changed
        };
        match key.name {
            KeyName::Backspace if ctrl || key.alt => {
                let from = self.word_back();
                changed(self, from, self.cursor)
            }
            KeyName::Backspace => {
                let from = self.prev_boundary();
                changed(self, from, self.cursor)
            }
            KeyName::Delete => {
                let to = self.next_boundary();
                changed(self, self.cursor, to)
            }
            KeyName::Char('w') if ctrl => {
                let from = self.word_back();
                changed(self, from, self.cursor)
            }
            KeyName::Char('u') if ctrl => changed(self, 0, self.cursor),
            KeyName::Char('k') if ctrl => changed(self, self.cursor, self.text.len()),
            KeyName::Left if ctrl || key.alt => {
                self.cursor = self.word_back();
                InputResult::Moved
            }
            KeyName::Right if ctrl || key.alt => {
                self.cursor = self.word_forward();
                InputResult::Moved
            }
            KeyName::Left | KeyName::Char('b') if key.name == KeyName::Left || ctrl => {
                self.cursor = self.prev_boundary();
                InputResult::Moved
            }
            KeyName::Right | KeyName::Char('f') if key.name == KeyName::Right || ctrl => {
                self.cursor = self.next_boundary();
                InputResult::Moved
            }
            KeyName::Home => {
                self.cursor = 0;
                InputResult::Moved
            }
            KeyName::Char('a') if ctrl => {
                self.cursor = 0;
                InputResult::Moved
            }
            KeyName::End => {
                self.cursor = self.text.len();
                InputResult::Moved
            }
            KeyName::Char('e') if ctrl => {
                self.cursor = self.text.len();
                InputResult::Moved
            }
            _ => InputResult::Ignored,
        }
    }

    /// The part of the text that fits in `cols` cells, scrolled so the
    /// cursor is visible, and the cursor's column inside it.
    pub fn view(&self, cols: usize) -> (String, usize) {
        if cols == 0 {
            return (String::new(), 0);
        }
        let before = width(&self.text[..self.cursor]);
        // Keep one cell for the cursor at the end of the text.
        let start_cells = (before + 1).saturating_sub(cols);
        let mut out = String::new();
        let mut x = 0usize;
        let mut used = 0usize;
        let mut cursor_x = 0usize;
        let mut cursor_set = false;
        for (i, g) in self.text.grapheme_indices(true) {
            let w = grapheme_width(g);
            if i == self.cursor {
                cursor_x = used;
                cursor_set = true;
            }
            if x < start_cells {
                x += w;
                continue;
            }
            if used + w > cols {
                break;
            }
            out.push_str(g);
            used += w;
            x += w;
        }
        if !cursor_set {
            cursor_x = used.min(cols.saturating_sub(1));
        }
        (out, cursor_x)
    }
}

/// Folds text to one line: control characters, tabs and line breaks
/// become single spaces.
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = c == ' ';
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        s.parse().unwrap()
    }

    fn typed(s: &str) -> LineInput {
        let mut i = LineInput::default();
        for c in s.chars() {
            assert_eq!(i.handle(&Key::char(c)), InputResult::Changed);
        }
        i
    }

    #[test]
    fn typing_and_deleting_by_grapheme() {
        let mut i = typed("ae\u{301}東");
        assert_eq!(i.text(), "ae\u{301}東");
        i.handle(&k("backspace"));
        assert_eq!(i.text(), "ae\u{301}");
        i.handle(&k("backspace"));
        assert_eq!(i.text(), "a", "the combining mark goes with its base");
        i.handle(&k("left"));
        i.handle(&k("delete"));
        assert_eq!(i.text(), "");
        assert_eq!(i.handle(&k("backspace")), InputResult::Moved);
    }

    #[test]
    fn shell_keys() {
        let mut i = typed("from:ada to:bob");
        i.handle(&k("ctrl-w"));
        assert_eq!(i.text(), "from:ada ");
        i.handle(&k("ctrl-a"));
        assert_eq!(i.cursor(), 0);
        i.handle(&k("ctrl-k"));
        assert_eq!(i.text(), "");
        let mut i = typed("one two");
        i.handle(&k("home"));
        i.handle(&k("ctrl-right"));
        assert_eq!(i.cursor(), 3);
        i.handle(&k("end"));
        i.handle(&k("ctrl-u"));
        assert!(i.is_empty());
        assert_eq!(i.handle(&k("enter")), InputResult::Ignored);
        assert_eq!(i.handle(&k("ctrl-s")), InputResult::Ignored);
    }

    #[test]
    fn paste_is_one_line() {
        let mut i = LineInput::new("a");
        i.insert_str("b\r\nc\td\u{1b}[31m");
        assert_eq!(i.text(), "ab c d [31m");
        assert_eq!(i.take(), "ab c d [31m");
        assert!(i.is_empty());
    }

    #[test]
    fn view_scrolls_to_the_cursor() {
        let i = LineInput::new("abcdefghij");
        let (v, x) = i.view(5);
        assert_eq!((v.as_str(), x), ("ghij", 4));
        let mut i = i;
        i.handle(&k("home"));
        assert_eq!(i.view(5), ("abcde".to_owned(), 0));
        let wide = LineInput::new("東京都庁");
        let (v, x) = wide.view(5);
        assert!(width(&v) <= 5 && x <= 4, "{v:?} {x}");
        assert_eq!(LineInput::default().view(0), (String::new(), 0));
    }
}
