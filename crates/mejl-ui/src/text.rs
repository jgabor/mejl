//! Display width helpers.
//!
//! Every width here is measured per extended grapheme cluster with
//! `unicode-width`, the same way ratatui fills buffer cells. Truncation and
//! padding work on graphemes and display columns, never on `char`s or bytes,
//! so CJK names, emoji and combining marks never split or misalign a column.
//!
//! Mail text is hostile: subjects and names can carry control characters,
//! bidi overrides that reorder a row visually, zero-width characters used
//! for tracking, and long ZWJ sequences that terminals disagree on.
//! [`sanitize`] removes or replaces all of these before anything is measured.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Shown in place of a grapheme that would be wider than two cells.
const REPLACEMENT: &str = "\u{FFFD}";

/// True for characters that are removed outright: bidi embeddings,
/// overrides and isolates, directional marks, and invisible characters
/// often used to fingerprint or pad mail.
pub fn is_dropped(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{200B}'
            | '\u{2060}'
            | '\u{FEFF}'
            | '\u{00AD}'
    )
}

/// Width of one grapheme in cells, matching ratatui's `cell_width`.
///
/// ratatui adds one cell for each halfwidth katakana sound mark, because
/// `unicode-width` gives them zero width inside a cluster while terminals
/// draw them as a cell of their own.
pub fn grapheme_width(g: &str) -> usize {
    if g.len() == 1 {
        return 1;
    }
    let marks = g
        .chars()
        .filter(|c| matches!(c, '\u{FF9E}' | '\u{FF9F}'))
        .count();
    g.width() + marks
}

/// Display width of a string in cells.
pub fn width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// Makes untrusted text safe to place in one row.
///
/// - Control characters, including newlines and tabs, become spaces, and
///   runs of whitespace collapse to one space.
/// - Bidi controls and invisible characters are removed.
/// - Zero-width graphemes are removed, because ratatui skips them and the
///   measured width would otherwise disagree with what is drawn.
/// - Graphemes wider than two cells become U+FFFD, matching the wcwidth
///   rule most terminals follow.
///
/// Leading and trailing whitespace is trimmed.
pub fn sanitize(s: &str) -> Cow<'_, str> {
    let clean = !s.starts_with(' ')
        && !s.ends_with(' ')
        && !s.contains("  ")
        && s.chars()
            .all(|c| !is_dropped(c) && (c == ' ' || !(c.is_control() || c.is_whitespace())))
        && s.graphemes(true)
            .all(|g| (1..=2).contains(&grapheme_width(g)));
    if clean {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for g in s.graphemes(true) {
        let filtered: String = g.chars().filter(|c| !is_dropped(*c)).collect();
        if filtered.is_empty() {
            continue;
        }
        if filtered
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        let w = grapheme_width(&filtered);
        if w == 0 {
            continue;
        }
        if w > 2 {
            out.push_str(REPLACEMENT);
        } else {
            out.push_str(&filtered);
        }
        last_space = false;
    }
    while out.ends_with(' ') {
        out.pop();
    }
    Cow::Owned(out)
}

/// Cuts `s` to at most `max` cells, ending with `ellipsis` when anything was
/// cut. Returns the text and its width. The result is never padded, and it
/// may be one cell short of `max` when a wide grapheme did not fit.
pub fn truncate(s: &str, max: usize, ellipsis: &str) -> (String, usize) {
    let full = width(s);
    if full <= max {
        return (s.to_owned(), full);
    }
    let ew = width(ellipsis);
    if max < ew {
        return (String::new(), 0);
    }
    let budget = max - ew;
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = grapheme_width(g);
        if used + w > budget {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push_str(ellipsis);
    (out, used + ew)
}

/// Truncates and pads `s` to exactly `cols` cells, left aligned.
pub fn fit(s: &str, cols: usize, ellipsis: &str) -> String {
    let (mut out, w) = truncate(s, cols, ellipsis);
    out.extend(std::iter::repeat_n(' ', cols.saturating_sub(w)));
    out
}

/// Truncates and pads `s` to exactly `cols` cells, right aligned.
pub fn rfit(s: &str, cols: usize, ellipsis: &str) -> String {
    let (out, w) = truncate(s, cols, ellipsis);
    let mut padded = " ".repeat(cols.saturating_sub(w));
    padded.push_str(&out);
    padded
}

/// Splits text into lines of at most `cols` cells at spaces, breaking
/// overlong words between graphemes. Used for short UI text such as help
/// descriptions, not for message bodies (mejl-render wraps those).
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_w = 0;
    for word in s.split(' ').filter(|w| !w.is_empty()) {
        let ww = width(word);
        if line_w > 0 && line_w + 1 + ww <= cols {
            line.push(' ');
            line.push_str(word);
            line_w += 1 + ww;
            continue;
        }
        if line_w > 0 {
            lines.push(std::mem::take(&mut line));
            line_w = 0;
        }
        if ww <= cols {
            line.push_str(word);
            line_w = ww;
            continue;
        }
        for g in word.graphemes(true) {
            let gw = grapheme_width(g);
            if line_w + gw > cols && line_w > 0 {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            line.push_str(g);
            line_w += gw;
        }
    }
    if line_w > 0 || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELL: &str = "\u{22EF}";

    #[test]
    fn widths_follow_graphemes() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("東京"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("👍"), 2);
        assert_eq!(width("ｶﾞ"), 2);
    }

    #[test]
    fn sanitize_borrows_clean_text() {
        assert!(matches!(sanitize("Juniper Tran"), Cow::Borrowed(_)));
        assert!(matches!(sanitize("東京 👍"), Cow::Borrowed(_)));
    }

    #[test]
    fn sanitize_strips_bidi_and_controls() {
        assert_eq!(sanitize("a\u{202E}gnp.exe"), "agnp.exe");
        assert_eq!(sanitize("line\none\ttab"), "line one tab");
        assert_eq!(sanitize("  padded   text  "), "padded text");
        assert_eq!(sanitize("in\u{200B}visible"), "invisible");
        assert_eq!(sanitize("\u{2067}x\u{2069}"), "x");
        assert_eq!(sanitize("\u{0007}"), "");
    }

    #[test]
    fn sanitize_keeps_zwj_emoji_and_combining_marks() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let out = sanitize(family);
        assert!(width(&out) <= 2, "{out:?}");
        assert_eq!(sanitize("Ame\u{301}lie"), "Ame\u{301}lie");
    }

    #[test]
    fn truncate_by_width_not_chars() {
        assert_eq!(truncate("東京都庁", 5, ELL), ("東京⋯".to_owned(), 5));
        assert_eq!(truncate("東京都庁", 4, ELL), ("東⋯".to_owned(), 3));
        assert_eq!(truncate("short", 10, ELL), ("short".to_owned(), 5));
        assert_eq!(truncate("abcdef", 0, ELL), (String::new(), 0));
        assert_eq!(truncate("abcdef", 1, ELL), (ELL.to_owned(), 1));
    }

    #[test]
    fn fit_pads_to_exact_width() {
        for s in ["", "a", "東京都庁舎", "👍👍👍", "Juniper Tran (3)"] {
            for cols in 0..12 {
                assert_eq!(width(&fit(s, cols, ELL)), cols, "{s:?} at {cols}");
                assert_eq!(width(&rfit(s, cols, ELL)), cols, "{s:?} at {cols}");
            }
        }
        assert_eq!(fit("Juniper Tran", 8, ELL), "Juniper⋯");
        assert_eq!(rfit("14:02", 6, ELL), " 14:02");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_long_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("", 5), vec![""]);
        assert!(wrap("東京都庁舎 東京", 4).iter().all(|l| width(l) <= 4));
    }
}
