//! Display width and line wrapping.
//!
//! Widths are measured per extended grapheme cluster with
//! `UnicodeWidthStr`, which is how ratatui fills buffer cells. Lines are
//! only ever broken between graphemes.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{Span, SpanStyle, StyledLine};

/// Shown in place of a grapheme that is wider than the whole line, which
/// only happens at widths below 2.
const TOO_WIDE: &str = "\u{FFFD}";

/// Width of `s` in terminal columns.
pub(crate) fn str_width(s: &str) -> usize {
    s.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// Builds a line span by span, merging neighbours with the same style.
#[derive(Debug, Default)]
pub(crate) struct LineBuilder {
    spans: Vec<Span>,
    width: usize,
}

impl LineBuilder {
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn push(&mut self, text: &str, style: SpanStyle) {
        self.push_measured(text, str_width(text), style);
    }

    fn push_measured(&mut self, text: &str, width: usize, style: SpanStyle) {
        if text.is_empty() {
            return;
        }
        self.width += width;
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.spans.push(Span {
                text: text.to_owned(),
                style,
            }),
        }
    }

    pub(crate) fn push_spans(&mut self, spans: &[Span]) {
        for s in spans {
            self.push(&s.text, s.style);
        }
    }

    /// Finishes the line with trailing spaces removed.
    pub(crate) fn finish(mut self) -> StyledLine {
        while let Some(last) = self.spans.last_mut() {
            let trimmed = last.text.trim_end_matches(' ').len();
            last.text.truncate(trimmed);
            if last.text.is_empty() {
                self.spans.pop();
            } else {
                break;
            }
        }
        StyledLine { spans: self.spans }
    }
}

/// One grapheme with its style, borrowed from the input spans.
#[derive(Clone, Copy)]
struct Cell<'a> {
    text: &'a str,
    width: usize,
    style: SpanStyle,
}

impl Cell<'_> {
    fn is_space(&self) -> bool {
        self.text == " "
    }
}

fn cells(spans: &[Span]) -> Vec<Cell<'_>> {
    spans
        .iter()
        .flat_map(|s| {
            s.text.graphemes(true).map(move |g| Cell {
                text: g,
                width: g.width(),
                style: s.style,
            })
        })
        .collect()
}

/// Word wraps `content` to `width` columns.
///
/// Every output line starts with `first_prefix` (the first line) or
/// `cont_prefix` (the others). A prefix that leaves no room for content is
/// dropped. Runs of spaces inside a line are kept, so plain text alignment
/// survives; spaces at a wrap point are dropped. Words longer than a line
/// are split between graphemes. An empty `content` yields one line holding
/// only the prefix.
pub(crate) fn wrap(
    content: &[Span],
    first_prefix: &[Span],
    cont_prefix: &[Span],
    width: usize,
) -> Vec<StyledLine> {
    let width = width.max(1);
    let fit = |p: &[Span]| -> (usize, bool) {
        let w: usize = p.iter().map(|s| str_width(&s.text)).sum();
        if w < width { (w, true) } else { (0, false) }
    };
    let (first_w, first_ok) = fit(first_prefix);
    let (cont_w, cont_ok) = fit(cont_prefix);

    let mut out = Vec::new();
    let new_line = |first: bool| {
        let mut b = LineBuilder::default();
        if first && first_ok {
            b.push_spans(first_prefix);
        } else if !first && cont_ok {
            b.push_spans(cont_prefix);
        }
        b
    };
    let avail = |first: bool| width - if first { first_w } else { cont_w };

    let cells = cells(content);
    let mut line = new_line(true);
    let mut first = true;
    // Content width on the current line, without the prefix.
    let mut used = 0usize;
    let mut i = 0;
    while i < cells.len() {
        // Gather a run of spaces and the word after it.
        let space_start = i;
        while i < cells.len() && cells[i].is_space() {
            i += 1;
        }
        let spaces = &cells[space_start..i];
        let word_start = i;
        while i < cells.len() && !cells[i].is_space() {
            i += 1;
        }
        let word = &cells[word_start..i];
        let spaces_w: usize = spaces.iter().map(|c| c.width).sum();
        let word_w: usize = word.iter().map(|c| c.width).sum();

        if word.is_empty() {
            // Trailing spaces end the logical line and are dropped.
            break;
        }
        let room = avail(first);
        if used + spaces_w + word_w <= room {
            push_cells(&mut line, spaces);
            push_cells(&mut line, word);
            used += spaces_w + word_w;
            continue;
        }
        if used > 0 {
            out.push(std::mem::replace(&mut line, new_line(false)).finish());
            first = false;
            used = 0;
            if word_w <= avail(false) {
                push_cells(&mut line, word);
                used += word_w;
                continue;
            }
        } else if first && spaces_w > 0 && spaces_w < room {
            // Leading indentation stays in front of a word that has to be
            // split anyway.
            push_cells(&mut line, spaces);
            used += spaces_w;
        }
        // The word is longer than a line: split it between graphemes.
        for cell in word {
            let room = avail(first);
            if used + cell.width > room && used > 0 {
                out.push(std::mem::replace(&mut line, new_line(false)).finish());
                first = false;
                used = 0;
            }
            let room = avail(first);
            if cell.width > room {
                line.push_measured(TOO_WIDE, 1, cell.style);
                used += 1;
            } else {
                line.push_measured(cell.text, cell.width, cell.style);
                used += cell.width;
            }
        }
    }
    out.push(line.finish());
    out
}

fn push_cells(line: &mut LineBuilder, cells: &[Cell<'_>]) {
    for c in cells {
        line.push_measured(c.text, c.width, c.style);
    }
}

/// Splits a line that is wider than `width` between graphemes. Lines that
/// fit are returned unchanged.
pub(crate) fn clamp(line: StyledLine, width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    if line.width() <= width {
        return vec![line];
    }
    let mut out = Vec::new();
    let mut cur = LineBuilder::default();
    for cell in cells(&line.spans) {
        if cur.width() + cell.width > width && cur.width() > 0 {
            out.push(std::mem::take(&mut cur).finish());
        }
        if cell.width > width {
            cur.push_measured(TOO_WIDE, 1, cell.style);
        } else {
            cur.push_measured(cell.text, cell.width, cell.style);
        }
    }
    out.push(cur.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpanKind;

    fn plain(text: &str) -> Vec<Span> {
        vec![Span {
            text: text.to_owned(),
            style: SpanStyle::default(),
        }]
    }

    fn texts(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(StyledLine::text).collect()
    }

    #[test]
    fn widths_follow_graphemes() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("中文"), 4);
        assert_eq!(str_width("👨‍👩‍👧"), 2);
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(str_width("🇸🇪"), 2);
    }

    #[test]
    fn wraps_words() {
        let lines = wrap(&plain("the quick brown fox jumps"), &[], &[], 10);
        assert_eq!(texts(&lines), ["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn keeps_inner_spaces_and_indentation() {
        let lines = wrap(&plain("  a   b"), &[], &[], 20);
        assert_eq!(texts(&lines), ["  a   b"]);
    }

    #[test]
    fn empty_content_gives_prefix_line() {
        let prefix = plain("> ");
        let lines = wrap(&[], &prefix, &prefix, 10);
        assert_eq!(texts(&lines), [">"]);
    }

    #[test]
    fn splits_long_words_between_graphemes() {
        let lines = wrap(&plain("see https://example.com/abcdef"), &[], &[], 8);
        assert_eq!(
            texts(&lines),
            ["see", "https://", "example.", "com/abcd", "ef"]
        );
    }

    #[test]
    fn never_splits_wide_characters() {
        let lines = wrap(&plain("中文中文中"), &[], &[], 3);
        assert_eq!(texts(&lines), ["中", "文", "中", "文", "中"]);
        for l in &lines {
            assert!(l.width() <= 3);
        }
    }

    #[test]
    fn replaces_graphemes_wider_than_the_line() {
        let lines = wrap(&plain("中a"), &[], &[], 1);
        assert_eq!(texts(&lines), ["\u{FFFD}", "a"]);
    }

    #[test]
    fn repeats_prefix_on_continuation_lines() {
        let prefix = plain(">> ");
        let lines = wrap(&plain("one two three four"), &prefix, &prefix, 10);
        assert_eq!(texts(&lines), [">> one two", ">> three", ">> four"]);
    }

    #[test]
    fn drops_prefix_that_leaves_no_room() {
        let prefix = plain(">>>> ");
        let lines = wrap(&plain("ab"), &prefix, &prefix, 4);
        assert_eq!(texts(&lines), ["ab"]);
    }

    #[test]
    fn keeps_styles_across_splits() {
        let link = SpanStyle::kind(SpanKind::Link(1));
        let content = vec![Span {
            text: "https://example.com/long".into(),
            style: link,
        }];
        let lines = wrap(&content, &[], &[], 10);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(l.spans.iter().all(|s| s.style == link));
        }
    }

    #[test]
    fn clamp_splits_only_when_needed() {
        let line = StyledLine {
            spans: plain("abcdef"),
        };
        assert_eq!(clamp(line.clone(), 6), vec![line.clone()]);
        assert_eq!(texts(&clamp(line, 4)), ["abcd", "ef"]);
        let wide = StyledLine {
            spans: plain("中中中"),
        };
        assert_eq!(texts(&clamp(wide, 3)), ["中", "中", "中"]);
    }
}
