//! Text helpers: header formatting and parsing, case folding, HTML to text,
//! previews, highlighting and safe truncation.

use jiff::Timestamp;

use crate::model::Address;

/// Case-folds one char to one char, so folded strings keep char positions.
/// Chars whose lowercase form is longer than one char are kept as they are.
fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Case-folds a string char by char.
pub(crate) fn fold(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

/// Case-insensitive substring test.
pub(crate) fn contains_folded(haystack: &str, needle_folded: &str) -> bool {
    if needle_folded.is_empty() {
        return true;
    }
    fold(haystack).contains(needle_folded)
}

/// Formats one address for a header field, quoting the display name when it
/// holds characters that are special in RFC 5322.
pub(crate) fn format_address(a: &Address) -> String {
    match a.name.as_deref() {
        Some(name) if !name.is_empty() => {
            let needs_quotes = name
                .chars()
                .any(|c| "()<>[]:;@\\,.\"".contains(c) || c.is_control());
            if needs_quotes {
                let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{escaped}\" <{}>", a.email)
            } else {
                format!("{name} <{}>", a.email)
            }
        }
        _ => a.email.clone(),
    }
}

pub(crate) fn format_address_list(list: &[Address]) -> String {
    list.iter()
        .map(format_address)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses an RFC 5322 address list leniently. Groups are flattened,
/// comments are dropped, and entries without an address are skipped.
pub(crate) fn parse_address_list(input: &str) -> Vec<Address> {
    let mut out = Vec::new();
    let mut phrase = String::new();
    let mut angle = String::new();
    let mut had_angle = false;
    let mut in_quote = false;
    let mut in_angle = false;
    let mut comment_depth = 0usize;
    let mut chars = input.chars();

    let mut finish = |phrase: &mut String, angle: &mut String, had_angle: &mut bool| {
        let collapsed = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        let (name, email) = if *had_angle {
            let name = if collapsed.is_empty() {
                None
            } else {
                Some(collapsed)
            };
            (name, angle.trim().to_owned())
        } else if collapsed.contains('@') && !collapsed.contains([' ', '<', '>']) {
            (None, collapsed)
        } else {
            // Not an address: a bare phrase, or an unterminated quote.
            (None, String::new())
        };
        if !email.is_empty() {
            out.push(Address { name, email });
        }
        phrase.clear();
        angle.clear();
        *had_angle = false;
    };

    while let Some(c) = chars.next() {
        if comment_depth > 0 {
            match c {
                '(' => comment_depth += 1,
                ')' => comment_depth -= 1,
                '\\' => {
                    chars.next();
                }
                _ => {}
            }
            continue;
        }
        if in_quote {
            match c {
                '"' => in_quote = false,
                '\\' => {
                    if let Some(next) = chars.next() {
                        phrase.push(next);
                    }
                }
                _ => phrase.push(c),
            }
            continue;
        }
        if in_angle {
            if c == '>' {
                in_angle = false;
            } else {
                angle.push(c);
            }
            continue;
        }
        match c {
            '"' => in_quote = true,
            '(' => comment_depth = 1,
            '<' => {
                in_angle = true;
                had_angle = true;
                angle.clear();
            }
            ',' | ';' => finish(&mut phrase, &mut angle, &mut had_angle),
            // A colon outside quotes and angles ends a group name.
            ':' => {
                phrase.clear();
                angle.clear();
                had_angle = false;
            }
            _ => phrase.push(c),
        }
    }
    finish(&mut phrase, &mut angle, &mut had_angle);
    out
}

/// Formats message ids as `<a@b> <c@d>`.
pub(crate) fn format_message_ids(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `<a@b> <c@d>` into ids. Returns `None` when anything else is in
/// the value, as RFC 8621 asks for the `asMessageIds` form.
pub(crate) fn parse_message_ids(value: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let after_open = rest.strip_prefix('<')?;
        let close = after_open.find('>')?;
        let id = after_open.get(..close)?.trim();
        if id.is_empty() || !id.contains('@') {
            return None;
        }
        out.push(id.to_owned());
        rest = after_open
            .get(close + 1..)?
            .trim_start_matches([' ', '\t', ',']);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Parses `<url>, <url>` as in List-* headers.
pub(crate) fn parse_urls(value: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(open) = rest.find('<') {
        let after = rest.get(open + 1..)?;
        let close = after.find('>')?;
        out.push(after.get(..close)?.trim().to_owned());
        rest = after.get(close + 1..)?;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// RFC 5322 date, always in UTC.
pub(crate) fn format_rfc5322_date(ts: Timestamp) -> String {
    jiff::fmt::rfc2822::DateTimePrinter::new()
        .timestamp_to_string(&ts)
        .unwrap_or_else(|_| ts.to_string())
}

pub(crate) fn parse_rfc5322_date(value: &str) -> Option<Timestamp> {
    jiff::fmt::rfc2822::DateTimeParser::new()
        .parse_timestamp(value.trim())
        .ok()
}

/// Formats a timestamp as an RFC 3339 `UTCDate`, as JMAP requires.
pub(crate) fn utc_date(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Parses a JMAP `UTCDate`. RFC 8620 requires the `Z` suffix.
pub(crate) fn parse_utc_date(s: &str) -> Option<Timestamp> {
    if !s.ends_with('Z') && !s.ends_with('z') {
        return None;
    }
    s.parse::<Timestamp>().ok()
}

/// Strips tags and decodes the common entities. Good enough for previews
/// and search, not for display.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut chars = html.char_indices().peekable();
    // ASCII lowercasing keeps byte offsets, so `i` indexes both strings.
    let lower = html.to_ascii_lowercase();
    let mut skip_until: Option<&str> = None;
    while let Some((i, c)) = chars.next() {
        if let Some(end) = skip_until {
            if lower.get(i..).is_some_and(|rest| rest.starts_with(end)) {
                skip_until = None;
            }
            continue;
        }
        if c == '<' {
            let rest = lower.get(i..).unwrap_or("");
            if rest.starts_with("<style") {
                skip_until = Some("</style");
            } else if rest.starts_with("<script") {
                skip_until = Some("</script");
            }
            let block = [
                "<br", "<p", "</p", "<div", "</div", "<tr", "<li", "<h1", "<h2", "<h3",
            ]
            .iter()
            .any(|t| rest.starts_with(t));
            for (_, c2) in chars.by_ref() {
                if c2 == '>' {
                    break;
                }
            }
            out.push(if block { '\n' } else { ' ' });
            continue;
        }
        if c == '&' {
            let rest = html.get(i..).unwrap_or("");
            let entities = [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&#39;", '\''),
                ("&apos;", '\''),
                ("&nbsp;", ' '),
            ];
            if let Some((ent, ch)) = entities.iter().find(|(e, _)| rest.starts_with(e)) {
                out.push(*ch);
                for _ in 1..ent.len() {
                    chars.next();
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// A preview as JMAP defines it: up to 256 characters of plain text with
/// whitespace collapsed. Quoted lines are left out.
pub(crate) fn preview(text: &str) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    'lines: for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('>') {
            continue;
        }
        if trimmed == "--" || line == "-- " {
            break;
        }
        for word in line.split_whitespace() {
            for c in (if out.is_empty() { "" } else { " " })
                .chars()
                .chain(word.chars())
            {
                if count == 256 {
                    break 'lines;
                }
                out.push(c);
                count += 1;
            }
        }
    }
    out
}

/// Truncates to at most `max` bytes without splitting a char. For HTML the
/// cut also avoids landing inside a tag.
pub(crate) fn truncate_utf8(s: &str, max: usize, html: bool) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = s.get(..end).unwrap_or("");
    if html
        && let Some(open) = cut.rfind('<')
        && cut.get(open..).is_some_and(|tail| !tail.contains('>'))
    {
        cut = cut.get(..open).unwrap_or("");
    }
    (cut, true)
}

fn escape_html_into(out: &mut String, s: impl Iterator<Item = char>) {
    for c in s {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

/// Char ranges in `text` where any of the folded terms occur, merged.
fn match_ranges(chars: &[char], terms: &[String]) -> Vec<(usize, usize)> {
    let folded: Vec<char> = chars.iter().copied().map(fold_char).collect();
    let mut ranges = Vec::new();
    for term in terms {
        let t: Vec<char> = term.chars().collect();
        if t.is_empty() || t.len() > folded.len() {
            continue;
        }
        for start in 0..=folded.len() - t.len() {
            if folded.get(start..start + t.len()) == Some(t.as_slice()) {
                ranges.push((start, start + t.len()));
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// HTML-escapes `chars` and wraps each match in `<mark>`.
fn highlight_chars(chars: &[char], ranges: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for &(s, e) in ranges {
        escape_html_into(&mut out, chars.get(pos..s).unwrap_or(&[]).iter().copied());
        out.push_str("<mark>");
        escape_html_into(&mut out, chars.get(s..e).unwrap_or(&[]).iter().copied());
        out.push_str("</mark>");
        pos = e;
    }
    escape_html_into(&mut out, chars.get(pos..).unwrap_or(&[]).iter().copied());
    out
}

/// The whole text, escaped and highlighted, or `None` when no term matches.
pub(crate) fn highlight(text: &str, terms: &[String]) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let ranges = match_ranges(&chars, terms);
    if ranges.is_empty() {
        return None;
    }
    Some(highlight_chars(&chars, &ranges))
}

/// A window of about 200 chars around the first match, escaped and
/// highlighted, or `None` when no term matches.
pub(crate) fn highlight_window(text: &str, terms: &[String]) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();
    let ranges = match_ranges(&chars, terms);
    let first = ranges.first()?.0;
    let start = first.saturating_sub(60);
    let end = (start + 200).min(chars.len());
    let window = chars.get(start..end)?;
    let local: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|(s, _)| *s >= start && *s < end)
        .map(|(s, e)| (s - start, (*e).min(end) - start))
        .collect();
    Some(highlight_chars(window, &local))
}

/// Strips leading `Re:`, `Fwd:` and `Fw:` markers, for subject sorting.
pub(crate) fn base_subject(s: &str) -> String {
    let mut rest = s.trim();
    loop {
        let folded = fold(rest);
        let stripped = ["re:", "fwd:", "fw:"]
            .iter()
            .find(|p| folded.starts_with(*p))
            .map(|p| rest.get(p.len()..).unwrap_or("").trim_start());
        match stripped {
            Some(next) => rest = next,
            None => return fold(rest),
        }
    }
}

/// True when `name` is a valid header field name: printable ASCII except
/// the colon.
pub(crate) fn is_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| (33..=126).contains(&b) && b != b':')
}

/// Escapes everything but the unreserved characters of RFC 3986, for query
/// strings and form bodies.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes and `+` as space, for query strings. Invalid
/// escapes are kept as written.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(b'%') => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok());
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            Some(b'+') => {
                out.push(b' ');
                i += 1;
            }
            Some(b) => {
                out.push(*b);
                i += 1;
            }
            None => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    #[test]
    fn address_lists_round_trip() {
        let list = vec![
            addr(Some("Juniper Tran"), "juniper@garden.example.com"),
            addr(None, "bare@example.org"),
            addr(Some("Holt, Mira"), "mira@example.org"),
            addr(Some("Odd \"quote\" \\ name"), "odd@example.net"),
            addr(Some("Zoë Ångström"), "zoe@example.com"),
        ];
        let formatted = format_address_list(&list);
        assert_eq!(parse_address_list(&formatted), list);
    }

    #[test]
    fn address_parser_handles_groups_comments_and_junk() {
        let parsed = parse_address_list(
            "Friends: a@example.com, \"B\" <b@example.com>;, (comment) c@example.com (x), , <>, Name Only",
        );
        assert_eq!(
            parsed,
            vec![
                addr(None, "a@example.com"),
                addr(Some("B"), "b@example.com"),
                addr(None, "c@example.com"),
            ]
        );
        assert!(parse_address_list("").is_empty());
        assert!(parse_address_list("\"unterminated <x@example.com>").is_empty());
        assert_eq!(
            parse_address_list("<x@example.com"),
            vec![addr(None, "x@example.com")]
        );
    }

    #[test]
    fn message_ids() {
        assert_eq!(
            parse_message_ids(" <a@b> <c@d>"),
            Some(vec!["a@b".to_owned(), "c@d".to_owned()])
        );
        assert_eq!(parse_message_ids("a@b"), None);
        assert_eq!(parse_message_ids("<a@b> junk"), None);
        assert_eq!(parse_message_ids("<>"), None);
        assert_eq!(parse_message_ids(""), None);
        assert_eq!(
            format_message_ids(&["x@y".to_owned(), "z@w".to_owned()]),
            "<x@y> <z@w>"
        );
    }

    #[test]
    fn urls() {
        assert_eq!(
            parse_urls("<mailto:u@example.com>, <https://example.com/u>"),
            Some(vec![
                "mailto:u@example.com".to_owned(),
                "https://example.com/u".to_owned()
            ])
        );
        assert_eq!(parse_urls("none"), None);
        assert_eq!(parse_urls("<unclosed"), None);
    }

    #[test]
    fn dates_round_trip() {
        let ts: Timestamp = "2026-10-23T09:12:00Z".parse().unwrap();
        let s = format_rfc5322_date(ts);
        assert_eq!(parse_rfc5322_date(&s), Some(ts));
        assert_eq!(utc_date(ts), "2026-10-23T09:12:00Z");
        assert_eq!(parse_utc_date("2026-10-23T09:12:00Z"), Some(ts));
        assert_eq!(parse_utc_date("2026-10-23T09:12:00+01:00"), None);
        assert_eq!(parse_utc_date("yesterday"), None);
        assert_eq!(parse_rfc5322_date("not a date"), None);
    }

    #[test]
    fn html_text_and_preview() {
        let html = "<html><style>p{x}</style><p>Hi &amp; welcome</p><script>bad()</script><br>Bye&nbsp;now";
        let text = html_to_text(html);
        assert!(text.contains("Hi & welcome"));
        assert!(text.contains("Bye now"));
        assert!(!text.contains("bad()"));
        assert!(!text.contains("p{x}"));
        let p = preview("Hello\n\n  there\n> quoted\nend\n-- \nsig");
        assert_eq!(p, "Hello there end");
        let long = "word ".repeat(200);
        assert_eq!(preview(&long).chars().count(), 256);
        assert_eq!(html_to_text("<unclosed"), " ");
    }

    #[test]
    fn truncation_respects_chars_and_tags() {
        assert_eq!(truncate_utf8("héllo", 2, false), ("h", true));
        assert_eq!(truncate_utf8("héllo", 3, false), ("hé", true));
        assert_eq!(truncate_utf8("abc", 10, false), ("abc", false));
        assert_eq!(
            truncate_utf8("<p>ab</p><a href", 14, true),
            ("<p>ab</p>", true)
        );
        assert_eq!(truncate_utf8("€€", 1, false), ("", true));
    }

    #[test]
    fn highlighting_escapes_and_marks() {
        let terms = vec!["plot".to_owned()];
        assert_eq!(
            highlight("Garden <club> PLOT rota", &terms).as_deref(),
            Some("Garden &lt;club&gt; <mark>PLOT</mark> rota")
        );
        assert_eq!(highlight("nothing", &terms), None);
        let terms = vec!["ab".to_owned(), "bc".to_owned()];
        assert_eq!(
            highlight("abc", &terms).as_deref(),
            Some("<mark>abc</mark>")
        );
        let long = format!("{} needle {}", "x ".repeat(100), "y ".repeat(100));
        let w = highlight_window(&long, &["needle".to_owned()]).unwrap();
        assert!(w.contains("<mark>needle</mark>"));
        assert!(w.chars().count() < 260);
        assert_eq!(highlight("", &terms), None);
    }

    #[test]
    fn base_subjects() {
        assert_eq!(base_subject("Re: RE: Fwd: Hello"), "hello");
        assert_eq!(base_subject("fw:x"), "x");
        assert_eq!(base_subject("Regarding"), "regarding");
    }

    #[test]
    fn header_names_and_percent_decoding() {
        assert!(is_header_name("X-FM-Masked-Email"));
        assert!(!is_header_name("Bad:Name"));
        assert!(!is_header_name("Sp ace"));
        assert!(!is_header_name(""));
        assert_eq!(percent_decode("Email%2CMailbox+x"), "Email,Mailbox x");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%E2%82%AC"), "€");
        let raw = "a+b /c=d&€~";
        assert_eq!(percent_encode(raw), "a%2Bb%20%2Fc%3Dd%26%E2%82%AC~");
        assert_eq!(percent_decode(&percent_encode(raw)), raw);
    }

    #[test]
    fn folding_keeps_positions() {
        // İ lowercases to two chars, so it is kept as it is.
        assert_eq!(fold("ÀBC İ"), "àbc İ");
        assert_eq!(fold("ABC").chars().count(), 3);
        assert!(contains_folded("Hello World", "world"));
        assert!(contains_folded("x", ""));
    }
}
