//! Link numbering and the footnote list.

use std::collections::HashMap;

use crate::width::{str_width, wrap};
use crate::{Link, Span, SpanKind, SpanStyle, StyledLine};

/// Longest link text kept in [`Link::text`], in characters.
const MAX_LINK_TEXT: usize = 200;

/// Longest URL shown in a footnote line, in characters. Tracking redirects
/// can run to kilobytes; [`Link::url`] keeps the full target.
const MAX_SHOWN_URL: usize = 1024;

/// Schemes that become links. Anything else (javascript:, data:, file:,
/// relative and fragment links) stays plain text, so the UI never hands
/// such a target to the system opener.
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "ftp", "mailto", "tel"];

/// Returns the URL cleaned for display and opening, or `None` when it is
/// not something mejl should open.
pub(crate) fn clean_url(raw: &str) -> Option<String> {
    let url: String = raw
        .trim()
        .chars()
        .filter(|c| !crate::text::is_dropped(*c) && !c.is_whitespace())
        .collect();
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    if !ALLOWED_SCHEMES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(scheme))
    {
        return None;
    }
    if url.len() == colon + 1 {
        return None;
    }
    Some(url)
}

/// Numbers links in order of first appearance, one number per URL.
#[derive(Debug, Default, Clone)]
pub(crate) struct LinkTable {
    links: Vec<Link>,
    by_url: HashMap<String, usize>,
}

impl LinkTable {
    /// Returns the 1-based index for `url`, adding it when new.
    pub(crate) fn intern(&mut self, url: &str) -> usize {
        if let Some(&index) = self.by_url.get(url) {
            return index;
        }
        let index = self.links.len() + 1;
        self.links.push(Link {
            index,
            url: url.to_owned(),
            text: String::new(),
        });
        self.by_url.insert(url.to_owned(), index);
        index
    }

    /// Appends visible text for link `index` until the text is long enough.
    pub(crate) fn add_text(&mut self, index: usize, text: &str) {
        let Some(link) = index.checked_sub(1).and_then(|i| self.links.get_mut(i)) else {
            return;
        };
        if link.text.chars().count() >= MAX_LINK_TEXT {
            return;
        }
        if !link.text.is_empty() && !link.text.ends_with(' ') {
            link.text.push(' ');
        }
        link.text.push_str(text);
    }

    pub(crate) fn into_links(self) -> Vec<Link> {
        self.links
            .into_iter()
            .map(|mut l| {
                let collapsed: Vec<&str> = l.text.split_whitespace().collect();
                let joined = collapsed.join(" ");
                l.text = joined.chars().take(MAX_LINK_TEXT).collect();
                l
            })
            .collect()
    }
}

/// The marker text for link `index`.
pub(crate) fn marker(index: usize) -> String {
    format!("[{index}]")
}

/// Lines for the footnote list: a blank separator, then "[n] url" for each
/// link with long URLs wrapped under the URL start.
pub(crate) fn footnote_lines(links: &[Link], width: usize) -> Vec<StyledLine> {
    if links.is_empty() {
        return Vec::new();
    }
    let mut out = vec![StyledLine::default()];
    for link in links {
        let mut shown: String = link.url.chars().take(MAX_SHOWN_URL).collect();
        if shown.len() < link.url.len() {
            shown.push('\u{2026}');
        }
        let url = vec![Span {
            text: shown,
            style: SpanStyle::kind(SpanKind::Link(link.index)),
        }];
        let head = format!("{} ", marker(link.index));
        let head_w = str_width(&head);
        let marker_style = SpanStyle::kind(SpanKind::FootnoteMarker(link.index));
        if head_w * 2 <= width {
            let first = vec![Span {
                text: head,
                style: marker_style,
            }];
            let indent = vec![Span {
                text: " ".repeat(head_w),
                style: SpanStyle::default(),
            }];
            out.extend(wrap(&url, &first, &indent, width));
        } else {
            // Too narrow for a hanging indent: let the marker wrap like text.
            let mut content = vec![Span {
                text: marker(link.index),
                style: marker_style,
            }];
            content.extend(url);
            out.extend(wrap(&content, &[], &[], width));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_url_allows_only_safe_schemes() {
        assert_eq!(
            clean_url(" https://example.com/a "),
            Some("https://example.com/a".into())
        );
        assert_eq!(
            clean_url("HTTPS://example.com"),
            Some("HTTPS://example.com".into())
        );
        assert_eq!(
            clean_url("mailto:info@example.com"),
            Some("mailto:info@example.com".into())
        );
        assert_eq!(clean_url("javascript:alert(1)"), None);
        assert_eq!(clean_url("data:text/html,hi"), None);
        assert_eq!(clean_url("file:///etc/passwd"), None);
        assert_eq!(clean_url("#top"), None);
        assert_eq!(clean_url("/relative/path"), None);
        assert_eq!(clean_url("https:"), None);
        assert_eq!(
            clean_url("https://exa\u{1b}mple.com/\u{202E}x"),
            Some("https://example.com/x".into())
        );
    }

    #[test]
    fn intern_dedupes_by_url() {
        let mut t = LinkTable::default();
        assert_eq!(t.intern("https://a.example.com"), 1);
        assert_eq!(t.intern("https://b.example.com"), 2);
        assert_eq!(t.intern("https://a.example.com"), 1);
        t.add_text(1, "Read");
        t.add_text(1, "more");
        t.add_text(9, "ignored");
        let links = t.into_links();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Read more");
        assert_eq!(links[1].index, 2);
    }

    #[test]
    fn footnotes_hang_under_the_url() {
        let links = vec![Link {
            index: 1,
            url: "https://example.com/a/very/long/path".into(),
            text: String::new(),
        }];
        let lines = footnote_lines(&links, 20);
        let texts: Vec<String> = lines.iter().map(StyledLine::text).collect();
        assert_eq!(
            texts,
            [
                "",
                "[1] https://example.",
                "    com/a/very/long/",
                "    path"
            ]
        );
    }

    #[test]
    fn footnotes_cap_shown_url() {
        let url = format!("https://example.com/{}", "a".repeat(5000));
        let links = vec![Link {
            index: 1,
            url: url.clone(),
            text: String::new(),
        }];
        let lines = footnote_lines(&links, 80);
        let shown: usize = lines.iter().map(|l| l.width()).sum();
        assert!(shown < 1200);
        assert!(lines.last().is_some_and(|l| l.text().ends_with('\u{2026}')));
    }
}
