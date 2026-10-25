//! `text/plain` bodies.

use linkify::{LinkFinder, LinkKind};

use crate::classify::{self, Attribution, Kind, LineInfo};
use crate::layout::{self, Rendered};
use crate::links::{LinkTable, clean_url, marker};
use crate::text::{push_clean, split_lines};
use crate::width::{str_width, wrap};
use crate::{Fold, Link, LinkStyle, Settings, Span, SpanKind, SpanStyle, StyledLine};

/// Plain parts at least this long are never "very short".
const SHORT_PLAIN_CHARS: usize = 40;

/// Only this much HTML is scanned when comparing it with a short plain part.
const HTML_SCAN_BYTES: usize = 256 * 1024;

/// Phrases of plain parts that only point at the HTML version.
const PLACEHOLDERS: &[&str] = &[
    "html version",
    "view this email in your browser",
    "view it in your browser",
    "view this message in your browser",
    "view the online version",
    "does not support html",
    "html-capable",
    "html capable",
    "enable html",
    "requires html",
];

/// A plain body split into classified, linkified logical lines.
#[derive(Debug, Clone)]
pub(crate) struct PlainDoc {
    lines: Vec<PlainLine>,
    labels: Vec<Option<Attribution>>,
    links: Vec<Link>,
}

#[derive(Debug, Clone)]
struct PlainLine {
    depth: u8,
    kind: Kind,
    /// Content after the quote prefix, as styled pieces.
    pieces: Vec<Span>,
}

impl PlainDoc {
    pub(crate) fn new(text: &str, settings: Settings) -> Self {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let cleaned: Vec<String> = split_lines(text)
            .map(|line| {
                let mut out = String::with_capacity(line.len());
                push_clean(&mut out, line, &mut 0, false);
                out
            })
            .collect();
        let first = cleaned.iter().position(|l| !l.trim().is_empty());
        let last = cleaned.iter().rposition(|l| !l.trim().is_empty());
        let cleaned = match (first, last) {
            (Some(a), Some(b)) => &cleaned[a..=b],
            _ => &[][..],
        };

        let infos: Vec<LineInfo<'_>> = cleaned
            .iter()
            .map(|l| {
                let (depth, content) = split_quote(l);
                LineInfo { depth, content }
            })
            .collect();
        let classified = classify::classify(&infos);
        let merged = reflow(cleaned, &infos, &classified);

        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url]);
        let mut table = LinkTable::default();
        let mut labels = Vec::with_capacity(merged.len());
        let lines = merged
            .into_iter()
            .map(|m| {
                labels.push(classified.labels[m.first].clone());
                PlainLine {
                    depth: classified.depths[m.first],
                    kind: classified.kinds[m.first],
                    pieces: linkify(&m.content, &finder, &mut table, settings.link_style),
                }
            })
            .collect();
        let mut links = table.into_links();
        for link in &mut links {
            if link.text.is_empty() {
                link.text.clone_from(&link.url);
            }
        }
        PlainDoc {
            lines,
            labels,
            links,
        }
    }

    pub(crate) fn render(
        &self,
        width: usize,
        settings: Settings,
    ) -> (Vec<StyledLine>, Vec<Link>, Vec<Fold>) {
        let mut rows = Vec::with_capacity(self.lines.len());
        for (line, label) in self.lines.iter().zip(&self.labels) {
            let prefix = quote_prefix(line.depth, width);
            let wrapped = wrap(&line.pieces, &prefix, &prefix, width);
            for (n, l) in wrapped.into_iter().enumerate() {
                rows.push(Rendered {
                    line: l,
                    kind: line.kind,
                    depth: line.depth,
                    label: if n == 0 { label.as_ref() } else { None },
                });
            }
        }
        let out = layout::assemble(rows, self.links.clone(), settings, width);
        (out.lines, out.links, out.folds)
    }
}

/// A paragraph made of one or more source lines.
struct Merged {
    /// Index of the first source line.
    first: usize,
    content: String,
}

/// Source lines narrower than this are never treated as wrapped.
const MIN_WRAP_COLUMN: usize = 50;

/// At least this many lines must come close to the widest line before the
/// widest line is trusted as the sender's wrap column.
const MIN_WRAPPED_LINES: usize = 2;

/// Joins lines the sender's client hard wrapped, so the paragraph can be
/// wrapped again at the reader width.
///
/// The wrap column is the widest body or quoted source line, quote markers
/// included and lines ending in `:` left out, and only counts when other lines come within 15 percent of it.
/// A break is a wrap only when the next line's first word would not have
/// fit: `width(line) + 1 + width(next word)` exceeds the wrap column, and
/// the line fills at least three quarters of it. List items, indented
/// lines, lines ending in `:`, signatures and attributions are never
/// joined.
fn reflow(source: &[String], infos: &[LineInfo<'_>], c: &classify::Classified) -> Vec<Merged> {
    let full: Vec<usize> = source.iter().map(|l| str_width(l.trim_end())).collect();
    let text_line = |i: usize| matches!(c.kinds[i], Kind::Body | Kind::Quote);
    // Lines ending in ':' are left out of the estimate: attributions inside
    // quotes are long single lines that no client wraps.
    let measured = |i: usize| text_line(i) && !infos[i].content.trim_end().ends_with(':');
    let wrap_at = (0..infos.len())
        .filter(|&i| measured(i))
        .map(|i| full[i])
        .max()
        .unwrap_or(0);
    let near = (0..infos.len())
        .filter(|&i| measured(i) && full[i] * 100 >= wrap_at * 85)
        .count();
    let trusted = wrap_at >= MIN_WRAP_COLUMN && near >= MIN_WRAPPED_LINES;
    let joins = |i: usize, j: usize| -> bool {
        let kind = c.kinds[i];
        if !trusted
            || !text_line(i)
            || c.kinds[j] != kind
            || c.depths[i] != c.depths[j]
            || c.labels[j].is_some()
        {
            return false;
        }
        let line = infos[i].content.trim_end();
        let next = infos[j].content;
        if line.is_empty()
            || line.ends_with(':')
            || next.trim().is_empty()
            || next.starts_with(char::is_whitespace)
            || is_list_item(next)
            || is_list_item(line)
            || is_layout_line(line)
            || is_layout_line(next)
        {
            return false;
        }
        let next_word = next.split(' ').next().unwrap_or("");
        full[i] * 4 >= wrap_at * 3 && full[i] + 1 + str_width(next_word) > wrap_at
    };
    let mut out: Vec<Merged> = Vec::with_capacity(infos.len());
    for (i, info) in infos.iter().enumerate() {
        if i > 0
            && joins(i - 1, i)
            && let Some(last) = out.last_mut()
        {
            let trimmed = last.content.trim_end().len();
            last.content.truncate(trimmed);
            last.content.push(' ');
            last.content.push_str(info.content);
            continue;
        }
        out.push(Merged {
            first: i,
            content: info.content.to_owned(),
        });
    }
    out
}

/// Lines that are aligned columns or rules rather than prose: two spaces
/// in a row after the indent, or no letters or digits at all.
fn is_layout_line(line: &str) -> bool {
    let t = line.trim();
    t.contains("  ") || !t.chars().any(char::is_alphanumeric)
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if ["- ", "* ", "+ ", "\u{2022} "]
        .iter()
        .any(|m| t.starts_with(m))
    {
        return true;
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// Splits the `>` prefix off a line. Spaces between the `>` characters are
/// allowed ("> > text"), as is one space after the last.
fn split_quote(line: &str) -> (u8, &str) {
    let b = line.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    let mut end = 0;
    while i < b.len() {
        if b[i] == b'>' {
            depth += 1;
            i += 1;
            end = i;
        } else if b[i] == b' ' && depth > 0 {
            let mut j = i;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if j < b.len() && b[j] == b'>' && j - i <= 2 {
                i = j;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    let content = &line[end..];
    let content = if depth > 0 {
        content.strip_prefix(' ').unwrap_or(content)
    } else {
        content
    };
    (u8::try_from(depth).unwrap_or(u8::MAX), content)
}

/// The prefix shown in front of quoted lines, shortened so that it never
/// takes more than half the width.
fn quote_prefix(depth: u8, width: usize) -> Vec<Span> {
    let shown = usize::from(depth).min((width / 2).saturating_sub(1));
    if shown == 0 {
        return Vec::new();
    }
    vec![Span {
        text: format!("{} ", ">".repeat(shown)),
        style: SpanStyle::default(),
    }]
}

fn linkify(
    content: &str,
    finder: &LinkFinder,
    table: &mut LinkTable,
    style: LinkStyle,
) -> Vec<Span> {
    let mut pieces = Vec::new();
    let mut push = |text: &str, style: SpanStyle| {
        if !text.is_empty() {
            pieces.push(Span {
                text: text.to_owned(),
                style,
            });
        }
    };
    let mut last = 0;
    for found in finder.links(content) {
        let Some(url) = clean_url(found.as_str()) else {
            continue;
        };
        push(&content[last..found.start()], SpanStyle::default());
        let index = table.intern(&url);
        push(found.as_str(), SpanStyle::kind(SpanKind::Link(index)));
        if style == LinkStyle::Footnotes {
            push(
                &marker(index),
                SpanStyle::kind(SpanKind::FootnoteMarker(index)),
            );
        }
        last = found.end();
    }
    push(&content[last..], SpanStyle::default());
    pieces
}

/// True when a plain part should give way to the HTML part.
pub(crate) fn is_degenerate(plain: &str, html: &str) -> bool {
    let t = plain.trim();
    if t.is_empty() {
        return true;
    }
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    let mut rest = String::new();
    let mut last = 0;
    let mut any_link = false;
    for link in finder.links(t) {
        rest.push_str(&t[last..link.start()]);
        last = link.end();
        any_link = true;
    }
    rest.push_str(&t[last..]);
    if any_link && rest.trim().is_empty() {
        return true;
    }
    if t.len() < 600 {
        let lower = t.to_lowercase();
        if PLACEHOLDERS.iter().any(|p| lower.contains(p)) {
            return true;
        }
    }
    let plain_chars = t.chars().filter(|c| !c.is_whitespace()).count();
    if plain_chars < SHORT_PLAIN_CHARS {
        let (head, _) = crate::text::truncate(html, HTML_SCAN_BYTES);
        let html_chars = crate::html::crude_text(head)
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();
        return html_chars > plain_chars * 4 + 100;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_quote_counts_depth() {
        assert_eq!(split_quote("> a"), (1, "a"));
        assert_eq!(split_quote(">> a"), (2, "a"));
        assert_eq!(split_quote("> > a"), (2, "a"));
        assert_eq!(split_quote(">>a"), (2, "a"));
        assert_eq!(split_quote(">  indented"), (1, " indented"));
        assert_eq!(split_quote("a > b"), (0, "a > b"));
        assert_eq!(split_quote(">"), (1, ""));
        assert_eq!(split_quote(" > not a quote"), (0, " > not a quote"));
        let deep = ">".repeat(1000);
        assert_eq!(split_quote(&deep).0, 255);
    }

    #[test]
    fn quote_prefix_fits_half_the_width() {
        assert_eq!(quote_prefix(2, 80)[0].text, ">> ");
        assert_eq!(quote_prefix(50, 20)[0].text, format!("{} ", ">".repeat(9)));
        assert!(quote_prefix(3, 2).is_empty());
        assert!(quote_prefix(0, 80).is_empty());
    }

    fn merged(text: &str) -> Vec<String> {
        let source: Vec<String> = text.lines().map(str::to_owned).collect();
        let lines: Vec<&str> = text.lines().collect();
        let infos: Vec<LineInfo<'_>> = lines
            .iter()
            .map(|l| {
                let (depth, content) = split_quote(l);
                LineInfo { depth, content }
            })
            .collect();
        let c = classify::classify(&infos);
        reflow(&source, &infos, &c)
            .into_iter()
            .map(|m| m.content)
            .collect()
    }

    #[test]
    fn reflow_joins_hard_wrapped_paragraphs() {
        let text = "The compost delivery is on the 9th, so whoever is on that week please\n\
                    be there by 8. I will leave the shed key with Noor, who has the spare.\n\
                    Short line.\n\
                    \n\
                    > Quoted text wrapped by the sender at seventy two columns or so, which\n\
                    > continues here.";
        assert_eq!(
            merged(text),
            [
                "The compost delivery is on the 9th, so whoever is on that week please be there by 8. I will leave the shed key with Noor, who has the spare. Short line.",
                "",
                "Quoted text wrapped by the sender at seventy two columns or so, which continues here.",
            ]
        );
    }

    #[test]
    fn reflow_keeps_short_lines_lists_and_signatures() {
        let text = "Shopping list for the allotment work day, please bring what you can:\n\
                    - gloves\n\
                    - twine and a pair of secateurs if you have a spare pair at home\n\
                    Thanks\n\
                    -- \n\
                    Juniper Tran, garden club secretary, plot seven, Allotment Lane\n\
                    Harbourside";
        assert_eq!(merged(text).len(), 7);
        let short = "Hi,\nsee you soon.\nJ";
        assert_eq!(merged(short).len(), 3);
    }

    #[test]
    fn reflow_keeps_aligned_tables_and_rules() {
        let text = "Your order from Larkspur Outfitters, shipped today to Quay Road office.\n\
                    Item                               Qty      Price   Total excl VAT\n\
                    ======================================================================\n\
                    Merino hiking socks, 2-pack          1      18.00            14.40\n\
                    Trail bottle, 750 ml                 2      24.00            19.20\n\
                    ----------------------------------------------------------------------\n\
                    Thanks for shopping with us, and see you on the trail again soon now.";
        assert_eq!(merged(text).len(), 7);
    }

    #[test]
    fn degenerate_plain_parts() {
        let html = "<p>Thanks!</p>";
        assert!(!is_degenerate("Thanks!", html));
        assert!(is_degenerate("   \n ", html));
        assert!(is_degenerate("https://example.com/view/123", html));
        assert!(is_degenerate(
            "To view this email in your browser, open https://example.com/v",
            html
        ));
        let long_html = format!("<p>{}</p>", "Plenty of words in the HTML part. ".repeat(20));
        assert!(is_degenerate("Hi", &long_html));
        assert!(!is_degenerate(
            "A real plain text reply with a few sentences in it, enough to stand alone.",
            &long_html
        ));
    }
}
