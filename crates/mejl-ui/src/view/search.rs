//! The search screen, after the search mock-up: the query, the operator
//! chips and skipped terms, the merged results, and syntax help.

use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use unicode_segmentation::UnicodeSegmentation;

use super::{Hint, MailView, Painter};
use crate::action::Action;
use crate::data::Highlight;
use crate::dates;
use crate::hit::HitTarget;
use crate::reader::DocSpan;
use crate::search::{SYNTAX, SearchState, ServerSearch, Source, chips, issue_text};
use crate::text::{fit, grapheme_width, sanitize, truncate, width};
use crate::theme::StyleName;

/// Splits highlighted text into spans, sanitising each piece on its own
/// so the byte ranges still line up. Bad ranges are skipped.
pub(crate) fn highlight_spans(h: &Highlight, base: StyleName, hl: StyleName) -> Vec<DocSpan> {
    let mut ranges: Vec<std::ops::Range<usize>> = h
        .matches
        .iter()
        .filter(|r| {
            r.start < r.end
                && r.end <= h.text.len()
                && h.text.is_char_boundary(r.start)
                && h.text.is_char_boundary(r.end)
        })
        .cloned()
        .collect();
    ranges.sort_by_key(|r| r.start);
    let mut out = Vec::new();
    let mut pos = 0usize;
    let clean = |s: &str| -> String {
        // Keep single spaces at the edges, which sanitize would trim.
        let lead = s.starts_with(char::is_whitespace);
        let trail = s.ends_with(char::is_whitespace) && s.trim().len() < s.len();
        let mid = sanitize(s);
        let mut t = String::new();
        if lead && !mid.is_empty() {
            t.push(' ');
        }
        t.push_str(&mid);
        if trail && !mid.is_empty() {
            t.push(' ');
        }
        if mid.is_empty() && !s.is_empty() && s.chars().all(char::is_whitespace) {
            t.push(' ');
        }
        t
    };
    for r in ranges {
        if r.start < pos {
            continue;
        }
        if r.start > pos {
            out.push(DocSpan::new(clean(&h.text[pos..r.start]), base));
        }
        out.push(DocSpan::new(clean(&h.text[r.clone()]), hl));
        pos = r.end;
    }
    if pos < h.text.len() {
        out.push(DocSpan::new(clean(&h.text[pos..]), base));
    }
    out
}

/// Splits a filter into two rows of at most `rooms.0` and `rooms.1`
/// columns. The first row ends after the last comma that fits, or where
/// the room ends when none does; what does not fit on the second row ends
/// in `ell`.
pub(crate) fn wrap_filter(text: &str, rooms: (usize, usize), ell: &str) -> (String, String) {
    if width(text) <= rooms.0 {
        return (text.to_owned(), String::new());
    }
    // The byte where the room ends, and the byte after the last comma
    // before it.
    let mut used = 0usize;
    let mut end = 0usize;
    let mut comma = None;
    for (i, g) in text.grapheme_indices(true) {
        used += grapheme_width(g);
        if used > rooms.0 {
            break;
        }
        end = i + g.len();
        if g == "," {
            comma = Some(end);
        }
    }
    let (first, rest) = text.split_at(comma.unwrap_or(end));
    let (second, _) = truncate(rest, rooms.1, ell);
    (first.to_owned(), second)
}

impl MailView<'_> {
    pub(super) fn search(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(st) = s.search.as_deref() else {
            return;
        };
        let ell = s.config.theme.glyphs.ellipsis;
        self.screen_top(p, "Search", "all mailboxes");
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let fr = l.frame.right().saturating_sub(1);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            "Search",
            p.style(StyleName::PaneTitleFocused),
            fr,
        );
        let inner = Rect::new(
            l.frame.x + 1,
            l.frame.y + 1,
            l.frame.width.saturating_sub(2),
            l.frame.height.saturating_sub(2),
        );
        if inner.height < 5 || inner.width < 30 {
            return;
        }
        let max = inner.right().saturating_sub(1);
        // Query line.
        let y = inner.y;
        let x = p.put(inner.x + 1, y, "/ ", p.style(StyleName::PopupTitle), max);
        let tag = "Fastmail search syntax";
        let tag_x = max.saturating_sub(width(tag) as u16);
        let room = usize::from(tag_x.saturating_sub(x + 2));
        let (text, cx) = st.input.view(room.max(1));
        if st.input.is_empty() {
            p.put(x, y, "type to search", p.style(StyleName::Dim), tag_x);
        } else {
            p.put(x, y, &text, p.style(StyleName::Text), tag_x);
        }
        p.put(tag_x, y, tag, p.style(StyleName::Dim), max + 1);
        let cur = Position::new(x + cx as u16, y);
        if !st.results_focused {
            p.patch(
                Rect::new(cur.x, cur.y, 1, 1),
                p.style(StyleName::Text).add_modifier(Modifier::REVERSED),
            );
            p.cursor = Some(cur);
        }
        p.hseparator(l.frame.x, fr, y + 1, border);
        // Chips and skipped terms.
        let skipped: Vec<String> = st.local.issues.iter().map(|i| i.term.clone()).collect();
        let cs = chips(st.input.text(), &skipped);
        let y = y + 2;
        p.put(inner.x + 1, y, "Filters", p.style(StyleName::Dim), max);
        let mut cx = inner.x + 10;
        if cs.is_empty() {
            p.put(cx, y, "none yet", p.style(StyleName::Dim), max);
        }
        for c in &cs {
            let style = if c.skipped {
                StyleName::StatusWarning
            } else {
                StyleName::Text
            };
            cx = p.put(
                cx,
                y,
                &format!("[{}]", sanitize(&c.text)),
                p.style(style),
                max,
            );
            cx = p.put(cx, y, "  ", p.style(StyleName::Text), max);
        }
        let y = y + 1;
        p.put(inner.x + 1, y, "Local", p.style(StyleName::Dim), max);
        if st.local.issues.is_empty() {
            p.put(
                inner.x + 10,
                y,
                if st.input.is_empty() {
                    "results appear as you type"
                } else {
                    "every term runs in the local index"
                },
                p.style(StyleName::Dim),
                max,
            );
        } else {
            let parts: Vec<String> = st
                .local
                .issues
                .iter()
                .map(|i| format!("{} ({})", sanitize(&i.term), issue_text(i.kind)))
                .collect();
            let text = format!("skipped {}; the server search runs them", parts.join(", "));
            let (t, _) = truncate(&text, usize::from(max.saturating_sub(inner.x + 10)), ell);
            p.put(inner.x + 10, y, &t, p.style(StyleName::StatusWarning), max);
        }
        // What the server search sends. The filter comes from mejl-sync's
        // `filter_preview`, the code that builds the request, through
        // `MailData::search_filter_preview`. The sort is `newest_first` in
        // mejl-sync's sync.rs and the app asks for collapsed threads in
        // its `Cmd::ServerSearch` arm. With a filter the block takes three
        // rows, whatever its length, so the results do not jump.
        let y = y + 1;
        p.put(inner.x + 1, y, "JMAP", p.style(StyleName::Dim), max);
        let x = p.put(
            inner.x + 10,
            y,
            "Email/query",
            p.style(StyleName::Text),
            max,
        );
        let sort = "sort: receivedAt descending   collapseThreads: true";
        let y = match &st.jmap {
            None => {
                p.put(x, y, &format!("  {sort}"), p.style(StyleName::Dim), max);
                y
            }
            Some(json) => {
                // The second row and the sort start under `filter:`, as in
                // the mock-up.
                let x = p.put(x, y, "  ", p.style(StyleName::Dim), max);
                let x = p.put(x, y, "filter: ", p.style(StyleName::Dim), max);
                let under = x.saturating_sub(width("filter: ") as u16);
                if st.input.is_empty() {
                    p.put(x, y, "appears as you type", p.style(StyleName::Dim), max);
                } else {
                    let rooms = (
                        usize::from(max.saturating_sub(x)),
                        usize::from(max.saturating_sub(under)),
                    );
                    let (first, second) = wrap_filter(&sanitize(json), rooms, ell);
                    p.put(x, y, &first, p.style(StyleName::Text), max);
                    p.put(under, y + 1, &second, p.style(StyleName::Text), max);
                }
                p.put(under, y + 2, sort, p.style(StyleName::Dim), max);
                y + 2
            }
        };
        p.hseparator(l.frame.x, fr, y + 1, border);
        // Results.
        let syntax_h: u16 = 10;
        let show_syntax = inner.height >= 26;
        let results_bottom = if show_syntax {
            inner.bottom().saturating_sub(syntax_h)
        } else {
            inner.bottom()
        };
        let y = y + 2;
        self.results_heading(p, st, inner.x + 1, y, max);
        let header_y = y + 2;
        if header_y < results_bottom {
            p.put(
                inner.x + 2,
                header_y,
                &format!(
                    "{}{}{}{}",
                    fit("source", 8, ell),
                    fit("date", 12, ell),
                    fit("from", 22, ell),
                    "subject"
                ),
                p.style(StyleName::Dim),
                max,
            );
        }
        let first = header_y + 1;
        let note_rows: u16 = if show_syntax { 3 } else { 0 };
        let rows_h = usize::from(results_bottom.saturating_sub(first + note_rows));
        let offset = st.cursor.saturating_sub(rows_h.saturating_sub(1));
        let now = s.now_zoned();
        for (k, r) in st.rows.iter().enumerate().skip(offset).take(rows_h) {
            let ry = first + (k - offset) as u16;
            let line = Rect::new(inner.x, ry, inner.width, 1);
            p.hits.push(line, HitTarget::Row(k));
            if k == st.cursor {
                p.put(
                    inner.x,
                    ry,
                    s.config.theme.glyphs.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            let source = match r.source {
                Source::Local => "local",
                Source::Server => "server",
            };
            let main = if r.row.unread {
                StyleName::ListUnread
            } else {
                StyleName::ListRow
            };
            let mut x = inner.x + 2;
            x = p.put(x, ry, &fit(source, 8, ell), p.style(StyleName::Dim), max);
            x = p.put(
                x,
                ry,
                &fit(&dates::pane_date(r.row.date, &now), 12, ell),
                p.style(StyleName::ListDate),
                max,
            );
            x = p.put(
                x,
                ry,
                &fit(&sanitize(r.row.from.display()), 21, ell),
                p.style(main),
                max,
            );
            x = p.put(x, ry, " ", p.style(main), max);
            let mut spans = highlight_spans(&r.subject, main, StyleName::HelpMatch);
            if !r.snippet.text.trim().is_empty() {
                spans.push(DocSpan::new("  ", StyleName::ListPreview));
                spans.extend(highlight_spans(
                    &r.snippet,
                    StyleName::ListPreview,
                    StyleName::HelpMatch,
                ));
            }
            let room = usize::from(max.saturating_sub(x));
            let mut used = 0usize;
            for sp in spans {
                let w = width(&sp.text);
                if used + w > room {
                    let (t, _) = truncate(&sp.text, room.saturating_sub(used), ell);
                    p.put(x, ry, &t, p.style(sp.style), max);
                    break;
                }
                x = p.put(x, ry, &sp.text, p.style(sp.style), max);
                used += w;
            }
            if k == st.cursor && st.results_focused {
                p.patch(line, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(inner.x, ry));
            }
        }
        if st.rows.is_empty() && first < results_bottom {
            let msg = if st.input.is_empty() {
                "Type a query: words, \"phrases\" or operators such as from: and is:unread."
            } else {
                "No local matches. Enter searches the server too."
            };
            p.put(inner.x + 2, first, msg, p.style(StyleName::Dim), max);
        }
        if show_syntax {
            let note_y = results_bottom.saturating_sub(2);
            p.put(
                inner.x + 2,
                note_y,
                "Local results cover cached mail. Older matches come from the server and are cached when opened.",
                p.style(StyleName::Dim),
                max,
            );
            let sy = results_bottom;
            p.hseparator(l.frame.x, fr, sy, border);
            p.title(
                l.frame.x + 2,
                sy,
                "Syntax",
                p.style(StyleName::PaneTitle),
                fr,
            );
            for (i, (a, b, c, d)) in SYNTAX.iter().enumerate() {
                let ry = sy + 2 + i as u16;
                let mut x = inner.x + 2;
                x = p.put(x, ry, &fit(a, 16, ell), p.style(StyleName::HintKey), max);
                x = p.put(x, ry, &fit(b, 42, ell), p.style(StyleName::Text), max);
                x = p.put(x, ry, &fit(c, 17, ell), p.style(StyleName::HintKey), max);
                p.put(x, ry, d, p.style(StyleName::Text), max);
            }
            p.put(
                inner.x + 2,
                sy + 2 + SYNTAX.len() as u16 + 1,
                "Terms the local index cannot run are listed above, so results are never silently wider than asked.",
                p.style(StyleName::Dim),
                max,
            );
        }
        let n = st.rows.len();
        let left = vec![DocSpan::new(
            match n {
                0 => "no results".to_owned(),
                1 => "1 result".to_owned(),
                n => format!("{n} results"),
            },
            StyleName::StatusText,
        )];
        self.status_line(p, Some(left), Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    fn results_heading(&self, p: &mut Painter<'_>, st: &SearchState, x: u16, y: u16, max: u16) {
        let local = st.rows.iter().filter(|r| r.source == Source::Local).count();
        let x = p.put(x, y, "Results  ", p.style(StyleName::PaneTitle), max);
        let server = match &st.server {
            Some(ServerSearch::Searching { query }) if query == st.input.text().trim() => {
                ", searching the server".to_owned()
            }
            Some(ServerSearch::Failed { query, .. }) if query == st.input.text().trim() => {
                ", the server search failed".to_owned()
            }
            _ => match st.server_count() {
                Some(n) => format!(", then {n} more from the server"),
                None => String::new(),
            },
        };
        p.put(
            x,
            y,
            &format!("{local} local{server}"),
            p.style(StyleName::Text),
            max,
        );
        let hint = if st.results_focused {
            "Enter open as a list"
        } else {
            "Enter search the server"
        };
        let hx = max.saturating_sub(width(hint) as u16);
        p.put(hx, y, hint, p.style(StyleName::Dim), max + 1);
    }

    pub(super) fn search_hints(&self) -> Vec<Hint> {
        let focused = self
            .state
            .search
            .as_ref()
            .is_some_and(|s| s.results_focused);
        let one = |a: Action, label: &'static str, short: &'static str, important: bool| {
            self.keys_for(a).map(|k| Hint {
                keys: k,
                label,
                short,
                target: HitTarget::Action(a),
                important,
            })
        };
        vec![
            one(
                Action::Open,
                if focused { "open" } else { "search server" },
                if focused { "open" } else { "server" },
                true,
            ),
            Some(Hint {
                keys: "Up/Down".into(),
                label: "results",
                short: "results",
                target: HitTarget::Popup,
                important: true,
            }),
            one(Action::NextField, "results or query", "focus", false),
            one(Action::SearchServer, "server", "server", false),
            one(Action::SaveSearch, "save search", "save", true),
            one(Action::Back, "close", "close", true),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_wrap_after_a_comma_and_end_in_an_ellipsis() {
        let f = r#"{"operator":"AND","conditions":[{"from":"larkspur"},{"hasAttachment":true}]}"#;
        assert_eq!(
            wrap_filter(f, (200, 200), "…"),
            (f.to_owned(), String::new())
        );
        let (a, b) = wrap_filter(f, (60, 60), "…");
        assert_eq!(a, r#"{"operator":"AND","conditions":[{"from":"larkspur"},"#);
        assert_eq!(b, r#"{"hasAttachment":true}]}"#);
        // The second row is cut to its room.
        let (_, b) = wrap_filter(f, (60, 10), "…");
        assert_eq!(b, r#"{"hasAtta…"#);
        // No comma in reach: break where the room ends.
        let (a, b) = wrap_filter(r#"{"text":"kettlekettle"}"#, (10, 40), "…");
        assert_eq!(
            (a.as_str(), b.as_str()),
            (r#"{"text":"k"#, r#"ettlekettle"}"#)
        );
        // Wide characters count two columns and are never split.
        let (a, b) = wrap_filter(r#"{"text":"東京"}"#, (10, 40), "…");
        assert_eq!((a.as_str(), b.as_str()), (r#"{"text":""#, r#"東京"}"#));
    }

    #[test]
    fn highlights_survive_sanitising() {
        let h = Highlight {
            text: "Re:  Your\u{202E} order shipped".into(),
            matches: vec![19..26, 100..120, std::ops::Range { start: 3, end: 1 }],
        };
        let spans = highlight_spans(&h, StyleName::Text, StyleName::HelpMatch);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(!text.contains('\u{202E}'));
        let hl: Vec<&str> = spans
            .iter()
            .filter(|s| s.style == StyleName::HelpMatch)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(hl, ["shipped"]);
        let none = highlight_spans(&Highlight::default(), StyleName::Text, StyleName::HelpMatch);
        assert!(none.is_empty());
        // A range inside a multi-byte character is ignored.
        let h = Highlight {
            text: "東京".into(),
            matches: std::iter::once(1..3).collect(),
        };
        assert_eq!(
            highlight_spans(&h, StyleName::Text, StyleName::HelpMatch).len(),
            1
        );
    }
}
