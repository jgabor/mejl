//! Input cleanup shared by both renderers.

use unicode_width::UnicodeWidthChar;

/// Tab stops every 8 columns, as terminals and most mail readers use.
const TAB_STOP: usize = 8;

/// Cuts `s` to at most `max` bytes at a character boundary.
pub(crate) fn truncate(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// Characters that are never shown.
///
/// Control characters could drive the terminal (an escaped `&#27;` in HTML
/// survives parsing as a real ESC). Bidi embedding, override and isolate
/// controls can reorder text in terminals that do bidi, which is a known
/// way to disguise link targets. Directional marks (U+200E, U+200F) are
/// harmless and stay.
pub(crate) fn is_dropped(c: char) -> bool {
    (c.is_control() && c != '\t') || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Invisible filler that newsletters pad preheaders with. Only stripped
/// from HTML output. Zero width joiners stay because emoji sequences need
/// them.
pub(crate) fn is_html_filler(c: char) -> bool {
    matches!(
        c,
        '\u{034F}' | '\u{200B}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'
    )
}

/// Appends `s` to `out`, dropping unsafe characters and expanding tabs.
///
/// `col` is the display column at which `s` starts. It is advanced, so a
/// line built from several pieces keeps correct tab stops.
pub(crate) fn push_clean(out: &mut String, s: &str, col: &mut usize, drop_filler: bool) {
    for c in s.chars() {
        if c == '\t' {
            let spaces = TAB_STOP - (*col % TAB_STOP);
            out.extend(std::iter::repeat_n(' ', spaces));
            *col += spaces;
        } else if is_dropped(c) || (drop_filler && is_html_filler(c)) {
            continue;
        } else {
            out.push(c);
            *col += c.width().unwrap_or(0);
        }
    }
}

/// Splits text into lines, treating CRLF, LF and a lone CR as breaks.
pub(crate) fn split_lines(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').flat_map(|line| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        line.split('\r')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), ("h", true));
        assert_eq!(truncate("héllo", 3), ("hé", true));
        assert_eq!(truncate("abc", 3), ("abc", false));
        assert_eq!(truncate("abc", 0), ("", true));
        assert_eq!(truncate("中", 2), ("", true));
    }

    #[test]
    fn push_clean_expands_tabs_to_stops() {
        let mut out = String::new();
        let mut col = 0;
        push_clean(&mut out, "a\tb", &mut col, false);
        assert_eq!(out, "a       b");
        assert_eq!(col, 9);
        let mut out = String::new();
        let mut col = 0;
        push_clean(&mut out, "中\tx", &mut col, false);
        assert_eq!(out, "中      x");
    }

    #[test]
    fn push_clean_drops_escapes_and_bidi_overrides() {
        let mut out = String::new();
        let mut col = 0;
        push_clean(
            &mut out,
            "a\u{1b}[31mb\u{202E}c\u{7f}d\u{9b}e\u{200F}",
            &mut col,
            false,
        );
        assert_eq!(out, "a[31mbcde\u{200F}");
    }

    #[test]
    fn push_clean_drops_filler_only_when_asked() {
        let mut out = String::new();
        push_clean(&mut out, "a\u{200B}\u{034F}b\u{200D}", &mut 0, true);
        assert_eq!(out, "ab\u{200D}");
        let mut out = String::new();
        push_clean(&mut out, "a\u{200B}b", &mut 0, false);
        assert_eq!(out, "a\u{200B}b");
    }

    #[test]
    fn split_lines_handles_all_line_endings() {
        let lines: Vec<&str> = split_lines("a\r\nb\nc\rd\r\n").collect();
        assert_eq!(lines, ["a", "b", "c", "d", ""]);
    }
}
