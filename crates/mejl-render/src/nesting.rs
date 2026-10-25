//! A cheap nesting limit applied to HTML before it is parsed.
//!
//! html5ever scans the stack of open elements for many start tags, so a
//! document of nested `<div>` or `<blockquote>` tags costs time quadratic
//! in its depth: 50 000 nested blockquotes took about 3 s in release. This
//! pass drops start tags (and their end tags) once an estimated depth is
//! reached, keeping their content. It is an estimate, not a parser:
//! elements whose end tag is optional or that have none are not counted,
//! so unclosed `<p>` and `<li>` do not inflate it.

use std::borrow::Cow;
use std::collections::HashMap;

/// Elements that never have content.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr", "param", "keygen", "frame",
];

/// Elements whose end tag may be left out. Mail often does.
const OPTIONAL_END: &[&str] = &[
    "html", "head", "body", "p", "li", "dt", "dd", "td", "th", "tr", "thead", "tbody", "tfoot",
    "colgroup", "option", "optgroup", "rb", "rt", "rp", "rtc", "caption",
];

/// Elements whose content is raw text up to their end tag.
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];

/// Returns `html` with start tags beyond `max` nesting levels removed.
pub(crate) fn limit_nesting(html: &str, max: usize) -> Cow<'_, str> {
    let b = html.as_bytes();
    let mut out: Option<String> = None;
    // Bytes up to here are already copied to `out` (when it exists).
    let mut copied = 0;
    let mut depth = 0usize;
    let mut dropped: HashMap<String, usize> = HashMap::new();
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let lt = i + off;
        let next = b.get(lt + 1).copied().unwrap_or(0);
        if html[lt..].starts_with("<!--") {
            i = html[lt + 4..]
                .find("-->")
                .map_or(b.len(), |e| lt + 4 + e + 3);
            continue;
        }
        let closing = next == b'/';
        let name_start = if closing { lt + 2 } else { lt + 1 };
        if !b.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            i = lt + 1;
            continue;
        }
        let name_end = name_start
            + b[name_start..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-' || **c == b':')
                .count();
        let name = html[name_start..name_end].to_ascii_lowercase();
        let tag_end = tag_end(b, name_end);
        let raw = RAW_TEXT.contains(&name.as_str());
        // Raw text elements are never dropped: their content would turn
        // into visible text.
        let counted =
            !raw && !VOID.contains(&name.as_str()) && !OPTIONAL_END.contains(&name.as_str());
        let mut drop_tag = false;
        if closing {
            if counted {
                match dropped.get_mut(&name) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        drop_tag = true;
                    }
                    _ => depth = depth.saturating_sub(1),
                }
            }
        } else if counted {
            if depth >= max {
                *dropped.entry(name.clone()).or_default() += 1;
                drop_tag = true;
            } else {
                depth += 1;
            }
        }
        if drop_tag {
            let s = out.get_or_insert_with(|| String::with_capacity(html.len()));
            s.push_str(&html[copied..lt]);
            copied = tag_end;
        }
        i = tag_end;
        if !closing && raw {
            // Skip the raw text so tags inside a script are not counted.
            let close = format!("</{name}");
            i = find_ci(&b[i..], close.as_bytes()).map_or(b.len(), |p| i + p);
        }
    }
    match out {
        Some(mut s) => {
            s.push_str(&html[copied..]);
            Cow::Owned(s)
        }
        None => Cow::Borrowed(html),
    }
}

/// Index just past the `>` that ends a tag, skipping quoted attribute
/// values. An unterminated tag runs to the end of the input.
fn tag_end(b: &[u8], from: usize) -> usize {
    let mut quote = None;
    for (k, &c) in b.iter().enumerate().skip(from) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return k + 1,
            None => {}
        }
    }
    b.len()
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_shallow_documents_alone() {
        let html = "<div><p>a<p>b<ul><li>x<li>y</ul><img src=x><br></div>";
        assert!(matches!(limit_nesting(html, 3), Cow::Borrowed(_)));
    }

    #[test]
    fn drops_tags_past_the_limit_and_keeps_content() {
        let html = "<div><div><div><div>deep</div></div></div></div>after";
        assert_eq!(limit_nesting(html, 2), "<div><div>deep</div></div>after");
    }

    #[test]
    fn handles_quotes_comments_and_raw_text() {
        let html =
            r#"<div title="a > b"><!-- <div> --><script>"<div><div>"</script><div>x</div></div>"#;
        assert_eq!(
            limit_nesting(html, 1),
            r#"<div title="a > b"><!-- <div> --><script>"<div><div>"</script>x</div>"#
        );
    }

    #[test]
    fn survives_junk() {
        for html in [
            "<",
            "</",
            "<div",
            "<div title=\"x",
            "<<<>>>",
            "</div></div>",
            "<!--",
        ] {
            let _ = limit_nesting(html, 1);
        }
        assert_eq!(limit_nesting("<b><b>x", 1), "<b>x");
    }
}
