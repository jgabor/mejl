//! Quote, attribution and signature detection, and folds.
//!
//! Classification works on logical lines (source lines for plain text,
//! html2text output lines for HTML). Folds are computed on rendered lines,
//! which inherit the class of the logical line they came from.

use crate::{Fold, FoldKind};

/// Quote runs with more quoted lines than this are folded.
const QUOTE_FOLD_AFTER: usize = 3;

/// A bare `--` (without the trailing space RFC 3676 asks for) only starts a
/// signature when at most this many lines follow it, so a `--` used as a
/// section break in a long message is left alone.
const WEAK_SIGNATURE_MAX_LINES: usize = 15;

/// Attribution text longer than this is not an attribution.
const MAX_ATTRIBUTION_BYTES: usize = 400;

/// What a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Body,
    Blank,
    Quote,
    Attribution,
    SigDelimiter,
    Signature,
}

/// Who wrote a quoted block and when, as far as the attribution says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Attribution {
    pub(crate) name: Option<String>,
    pub(crate) date: Option<String>,
}

/// One logical line as the classifier sees it.
pub(crate) struct LineInfo<'a> {
    /// Quote depth from the line's prefix.
    pub(crate) depth: u8,
    /// The text after the quote prefix, not trimmed.
    pub(crate) content: &'a str,
}

/// Classification of logical lines.
#[derive(Debug, Default)]
pub(crate) struct Classified {
    pub(crate) kinds: Vec<Kind>,
    /// Effective quote depth. A quoted header tail ("From: ... Sent: ...")
    /// has no `>` prefix but counts as depth 1.
    pub(crate) depths: Vec<u8>,
    /// Set on the first line of each quote run.
    pub(crate) labels: Vec<Option<Attribution>>,
}

/// A rendered line as the fold pass sees it.
pub(crate) struct Row<'a> {
    pub(crate) kind: Kind,
    pub(crate) label: Option<&'a Attribution>,
}

pub(crate) fn classify(lines: &[LineInfo<'_>]) -> Classified {
    let n = lines.len();
    let mut kinds: Vec<Kind> = lines
        .iter()
        .map(|l| {
            if l.depth > 0 {
                Kind::Quote
            } else if l.content.trim().is_empty() {
                Kind::Blank
            } else {
                Kind::Body
            }
        })
        .collect();
    let mut depths: Vec<u8> = lines.iter().map(|l| l.depth).collect();
    let mut labels: Vec<Option<Attribution>> = vec![None; n];

    if let Some((start, label)) = find_header_tail(lines, &kinds) {
        for k in start..n {
            if kinds[k] != Kind::Blank {
                kinds[k] = Kind::Quote;
                depths[k] = depths[k].max(1);
            }
        }
        labels[start] = Some(label);
    }

    for i in 0..n {
        if kinds[i] != Kind::Quote || labels[i].is_some() {
            continue;
        }
        let prev = (0..i).rev().find(|&k| kinds[k] != Kind::Blank);
        if prev.is_some_and(|k| kinds[k] == Kind::Quote) {
            continue;
        }
        labels[i] = Some(attribute_run(lines, &mut kinds, i));
    }

    mark_signatures(lines, &mut kinds);
    Classified {
        kinds,
        depths,
        labels,
    }
}

/// Finds the attribution for the quote run starting at `start`, marking
/// attribution lines above the run.
fn attribute_run(lines: &[LineInfo<'_>], kinds: &mut [Kind], start: usize) -> Attribution {
    // Above the run, allowing one blank line in between.
    let mut end = start;
    if end > 0 && kinds[end - 1] == Kind::Blank {
        end -= 1;
    }
    // Longest first, so a wrapped attribution is read whole. A block of
    // several lines must open like an attribution, so a short reply line
    // right above it is never folded away with the quote.
    for count in (1..=3).rev() {
        if end < count {
            continue;
        }
        let range = end - count..end;
        if !range.clone().all(|k| kinds[k] == Kind::Body) {
            continue;
        }
        if count > 1 && !opens_attribution(lines[end - count].content) {
            continue;
        }
        let joined = join(lines, range.clone());
        if let Some(a) = parse_attribution(&joined) {
            for k in range {
                kinds[k] = Kind::Attribution;
            }
            return a;
        }
    }
    // Inside the run, as Apple Mail and some HTML clients write it.
    let inner: Vec<usize> = (start..lines.len())
        .take_while(|&k| kinds[k] == Kind::Quote)
        .filter(|&k| !lines[k].content.trim().is_empty())
        .take(3)
        .collect();
    for count in 1..=inner.len() {
        let joined: Vec<&str> = inner[..count]
            .iter()
            .map(|&k| lines[k].content.trim())
            .collect();
        if let Some(a) = parse_attribution(&joined.join(" ")) {
            return a;
        }
    }
    Attribution::default()
}

/// Words attributions start with in common client languages.
const LEADS: &[&str] = &[
    "On ", "Le ", "Am ", "El ", "Den ", "Il ", "Op ", "Em ", "W dniu ",
];

fn opens_attribution(line: &str) -> bool {
    let t = line.trim_start();
    LEADS.iter().any(|l| t.starts_with(l))
}

fn join(lines: &[LineInfo<'_>], range: std::ops::Range<usize>) -> String {
    let parts: Vec<&str> = lines[range].iter().map(|l| l.content.trim()).collect();
    parts.join(" ")
}

/// Finds an unquoted reply header block (Outlook style "From:", "Sent:",
/// "Subject:") and returns where the quoted tail starts.
fn find_header_tail(lines: &[LineInfo<'_>], kinds: &[Kind]) -> Option<(usize, Attribution)> {
    for i in 0..lines.len() {
        if kinds[i] != Kind::Body || !starts_with_ci(lines[i].content.trim(), "from:") {
            continue;
        }
        let window = &lines[i + 1..lines.len().min(i + 7)];
        let date_line = window.iter().find(|l| {
            let t = l.content.trim();
            starts_with_ci(t, "sent:") || starts_with_ci(t, "date:")
        });
        let has_subject = window
            .iter()
            .any(|l| starts_with_ci(l.content.trim(), "subject:"));
        let (Some(date_line), true) = (date_line, has_subject) else {
            continue;
        };
        let forwarded = lines[i.saturating_sub(3)..i]
            .iter()
            .any(|l| l.content.to_ascii_lowercase().contains("forwarded"));
        if forwarded {
            continue;
        }
        let start = if i > 0 && is_separator(lines[i - 1].content) && kinds[i - 1] == Kind::Body {
            i - 1
        } else {
            i
        };
        let from = lines[i].content.trim();
        let name = name_after(&from[5..]);
        let date = extract_date(date_line.content);
        return Some((start, Attribution { name, date }));
    }
    None
}

fn is_separator(line: &str) -> bool {
    let t = line.trim();
    if t.to_ascii_lowercase().contains("original message") {
        return true;
    }
    t.chars().count() >= 5 && t.chars().all(|c| matches!(c, '_' | '-' | '=' | '─' | '━'))
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn mark_signatures(lines: &[LineInfo<'_>], kinds: &mut [Kind]) {
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let strong = lines[i].content == "-- ";
        let weak = lines[i].content.trim_end() == "--";
        if kinds[i] != Kind::Body || !(strong || weak) {
            i += 1;
            continue;
        }
        let region_end = (i + 1..n)
            .find(|&k| matches!(kinds[k], Kind::Quote | Kind::Attribution))
            .unwrap_or(n);
        let last = (i + 1..region_end).rev().find(|&k| kinds[k] != Kind::Blank);
        let Some(last) = last else {
            i += 1;
            continue;
        };
        let count = last - i;
        if weak && !strong && count > WEAK_SIGNATURE_MAX_LINES {
            i += 1;
            continue;
        }
        kinds[i] = Kind::SigDelimiter;
        for kind in &mut kinds[i + 1..=last] {
            *kind = Kind::Signature;
        }
        i = region_end;
    }
}

/// Folds over rendered rows.
pub(crate) fn folds(rows: &[Row<'_>], fold_quotes: bool) -> Vec<Fold> {
    let n = rows.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        match rows[i].kind {
            Kind::Quote => {
                let mut end = i;
                let mut k = i + 1;
                while k < n {
                    match rows[k].kind {
                        Kind::Quote => {
                            end = k;
                            k += 1;
                        }
                        Kind::Blank => {
                            let mut m = k;
                            while m < n && rows[m].kind == Kind::Blank {
                                m += 1;
                            }
                            if m < n && rows[m].kind == Kind::Quote {
                                k = m;
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                let quoted = end - i + 1;
                let mut start = i;
                if start >= 2
                    && rows[start - 1].kind == Kind::Blank
                    && rows[start - 2].kind == Kind::Attribution
                {
                    start -= 1;
                }
                while start > 0 && rows[start - 1].kind == Kind::Attribution {
                    start -= 1;
                }
                if fold_quotes && quoted > QUOTE_FOLD_AFTER {
                    out.push(Fold {
                        kind: FoldKind::Quote,
                        start_line: start,
                        line_count: end - start + 1,
                        summary: quote_summary(quoted, rows[i].label),
                    });
                }
                i = end + 1;
            }
            Kind::SigDelimiter => {
                let mut end = i;
                while end + 1 < n && rows[end + 1].kind == Kind::Signature {
                    end += 1;
                }
                let count = end - i;
                if count > 0 {
                    out.push(Fold {
                        kind: FoldKind::Signature,
                        start_line: i,
                        line_count: count + 1,
                        summary: format!("signature, {count} {}", plural(count, "line")),
                    });
                }
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

fn quote_summary(count: usize, label: Option<&Attribution>) -> String {
    let mut s = format!("{count} quoted {}", plural(count, "line"));
    if let Some(name) = label.and_then(|a| a.name.as_deref()) {
        s.push_str(" from ");
        s.push_str(name);
        if let Some(date) = label.and_then(|a| a.date.as_deref()) {
            s.push_str(", ");
            s.push_str(date);
        }
    }
    s
}

/// Words that end an attribution in common client languages.
const VERBS: &[&str] = &[
    "wrote",
    "writes",
    "schrieb",
    "a écrit",
    "escribió",
    "scrisse",
    "schreef",
    "skrev",
    "kirjoitti",
    "napisał",
    "escreveu",
];

/// Parses an attribution such as "On Wed, Oct 21, 2026 at 8:31 AM Mira
/// Holt <mira@example.org> wrote:". Returns `None` when the text is not
/// one.
pub(crate) fn parse_attribution(text: &str) -> Option<Attribution> {
    let t = text.trim();
    if t.len() > MAX_ATTRIBUTION_BYTES || !t.ends_with(':') {
        return None;
    }
    // ASCII lowercasing keeps byte offsets valid for `t`.
    let lower = t.to_ascii_lowercase();
    let (pos, verb) = VERBS
        .iter()
        .filter_map(|v| find_word(&lower, v).map(|p| (p, *v)))
        .max_by_key(|(p, _)| *p)?;
    let before = &t[..pos];
    let after = t[pos + verb.len()..].trim().trim_end_matches(':').trim();
    let name = if after.is_empty() {
        name_before(before)
    } else {
        name_after(after)
    };
    Some(Attribution {
        name,
        date: extract_date(before),
    })
}

/// Last position of `word` in `hay` that starts a word and ends one.
fn find_word(hay: &str, word: &str) -> Option<usize> {
    let mut search_end = hay.len();
    while let Some(p) = hay[..search_end].rfind(word) {
        let starts = p == 0 || hay[..p].ends_with(|c: char| !c.is_alphanumeric());
        let ends = hay[p + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if starts && ends {
            return Some(p);
        }
        search_end = p;
    }
    None
}

/// The name in front of the verb: what follows the time, or the last comma.
fn name_before(before: &str) -> Option<String> {
    let (cut, email) = cut_address(before);
    let tail = match time_end(cut) {
        Some(end) => &cut[end..],
        None => match cut.rfind(',') {
            Some(p) => &cut[p + 1..],
            None => cut,
        },
    };
    clean_name(tail).or(email)
}

/// The name after the verb, as German and Swedish clients write it.
pub(crate) fn name_after(after: &str) -> Option<String> {
    let (cut, email) = cut_address(after);
    clean_name(cut).or(email).or_else(|| {
        let t = after.trim();
        (t.contains('@') && !t.contains(' ')).then(|| t.to_owned())
    })
}

/// Splits "Name <addr>" into "Name " and the address.
fn cut_address(s: &str) -> (&str, Option<String>) {
    match s.find('<') {
        Some(p) => {
            let addr = s[p + 1..]
                .split('>')
                .next()
                .map(str::trim)
                .filter(|a| a.contains('@') && !a.is_empty())
                .map(str::to_owned);
            let head = &s[..p];
            let head = head.strip_suffix('(').unwrap_or(head);
            let head = match head.rfind('(') {
                Some(q) if head[q..].contains('@') => &head[..q],
                _ => head,
            };
            (head.trim_end().trim_end_matches('('), addr)
        }
        None => (s, None),
    }
}

/// Byte offset just after the last clock time ("8:31", "08:31:05 PM").
fn time_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' && i > 0 && b[i - 1].is_ascii_digit() {
            let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits == 2 {
                let mut end = i + 3;
                if b.get(end) == Some(&b':')
                    && b.get(end + 1).is_some_and(u8::is_ascii_digit)
                    && b.get(end + 2).is_some_and(u8::is_ascii_digit)
                {
                    end += 3;
                }
                let rest = &s[end..];
                let trimmed = rest.trim_start();
                let skipped = rest.len() - trimmed.len();
                for suffix in ["a.m.", "p.m.", "am", "pm", "uhr"] {
                    if starts_with_ci(trimmed, suffix)
                        && trimmed[suffix.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !c.is_alphanumeric())
                    {
                        end += skipped + suffix.len();
                        break;
                    }
                }
                found = Some(end);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    found
}

fn clean_name(s: &str) -> Option<String> {
    let t = s.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, ',' | '"' | '\'' | '(' | ')' | ':' | ';' | '<' | '>')
    });
    let len = t.chars().count();
    if len == 0 || len > 64 || !t.chars().any(char::is_alphabetic) {
        return None;
    }
    Some(t.to_owned())
}

const MONTHS: &[(&str, &[&str])] = &[
    ("Jan", &["jan", "january", "januari", "januar", "janv"]),
    ("Feb", &["feb", "february", "februari", "februar", "févr"]),
    ("Mar", &["mar", "march", "mars", "märz", "mär"]),
    ("Apr", &["apr", "april", "avr"]),
    ("May", &["may", "mai", "maj"]),
    ("Jun", &["jun", "june", "juni", "juin"]),
    ("Jul", &["jul", "july", "juli", "juil"]),
    ("Aug", &["aug", "august", "augusti", "août"]),
    ("Sep", &["sep", "sept", "september", "septembre"]),
    ("Oct", &["oct", "october", "okt", "oktober", "octobre"]),
    ("Nov", &["nov", "november", "novembre"]),
    (
        "Dec",
        &["dec", "december", "dez", "dezember", "déc", "décembre"],
    ),
];

/// Finds a "month day" pair in either order and returns it as "Oct 21".
pub(crate) fn extract_date(s: &str) -> Option<String> {
    let words: Vec<String> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let day = |w: Option<&String>| -> Option<u8> {
        w.and_then(|w| w.parse::<u8>().ok())
            .filter(|d| (1..=31).contains(d))
    };
    for (i, w) in words.iter().enumerate() {
        let Some((abbr, _)) = MONTHS.iter().find(|(_, names)| names.contains(&w.as_str())) else {
            continue;
        };
        let next = day(words.get(i + 1));
        let prev = i.checked_sub(1).and_then(|p| day(words.get(p)));
        if let Some(d) = next.or(prev) {
            return Some(format!("{abbr} {d}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(s: &str) -> Option<(Option<String>, Option<String>)> {
        parse_attribution(s).map(|a| (a.name, a.date))
    }

    fn named(name: &str, date: Option<&str>) -> Option<(Option<String>, Option<String>)> {
        Some((Some(name.to_owned()), date.map(str::to_owned)))
    }

    #[test]
    fn parses_common_attributions() {
        assert_eq!(
            attr("On Wed, Oct 21, 2026 at 8:31 AM Mira Holt <mira@example.org> wrote:"),
            named("Mira Holt", Some("Oct 21"))
        );
        assert_eq!(
            attr("On Oct 21, 2026, at 08:31, Mira Holt <mira@example.org> wrote:"),
            named("Mira Holt", Some("Oct 21"))
        );
        assert_eq!(
            attr("On 10/21/26 8:31 PM, Mira Holt wrote:"),
            named("Mira Holt", None)
        );
        assert_eq!(attr("Mira Holt wrote:"), named("Mira Holt", None));
        assert_eq!(
            attr("Am Mi., 21. Okt. 2026 um 08:31 Uhr schrieb Mira Holt <mira@example.org>:"),
            named("Mira Holt", Some("Oct 21"))
        );
        assert_eq!(
            attr("Den ons 21 okt. 2026 kl 08:31 skrev Mira Holt <mira@example.org>:"),
            named("Mira Holt", Some("Oct 21"))
        );
        assert_eq!(
            attr("Le 21 oct. 2026 à 08:31, Mira Holt <mira@example.org> a écrit :"),
            named("Mira Holt", Some("Oct 21"))
        );
        assert_eq!(
            attr("On Mon, 19 Oct 2026, <mira@example.org> wrote:"),
            named("mira@example.org", Some("Oct 19"))
        );
    }

    #[test]
    fn rejects_non_attributions() {
        assert_eq!(attr("Here is what I wrote"), None);
        assert_eq!(attr("The list is as follows:"), None);
        assert_eq!(attr("I rewrote it:"), None);
        assert_eq!(attr(&format!("{} wrote:", "x".repeat(500))), None);
    }

    #[test]
    fn attribution_without_name_still_parses() {
        assert_eq!(
            attr("On Oct 21, 2026 at 08:31 wrote:"),
            Some((None, Some("Oct 21".into())))
        );
    }

    fn info(lines: &[&'static str]) -> Vec<LineInfo<'static>> {
        lines
            .iter()
            .map(|l| {
                let depth = l.chars().take_while(|c| *c == '>').count() as u8;
                let content = l[depth as usize..]
                    .strip_prefix(' ')
                    .unwrap_or(&l[depth as usize..]);
                LineInfo { depth, content }
            })
            .collect()
    }

    #[test]
    fn classifies_reply_with_signature() {
        let lines = info(&[
            "Sounds good.",
            "",
            "Juniper",
            "-- ",
            "Juniper Tran",
            "Garden club",
            "",
            "On Oct 21, 2026, at 08:31, Mira Holt wrote:",
            "> one",
            "> two",
        ]);
        let c = classify(&lines);
        use Kind::*;
        assert_eq!(
            c.kinds,
            [
                Body,
                Blank,
                Body,
                SigDelimiter,
                Signature,
                Signature,
                Blank,
                Attribution,
                Quote,
                Quote
            ]
        );
        assert_eq!(
            c.labels[8].as_ref().and_then(|a| a.name.as_deref()),
            Some("Mira Holt")
        );
    }

    #[test]
    fn wrapped_attribution_is_read_whole_and_reply_lines_stay() {
        let lines = info(&[
            "Thanks!",
            "On Wed, Oct 21, 2026 at 8:31",
            "AM Mira Holt",
            "<mira@example.org> wrote:",
            "> one",
        ]);
        let c = classify(&lines);
        use Kind::*;
        assert_eq!(
            c.kinds,
            [Body, Attribution, Attribution, Attribution, Quote]
        );
        let label = c.labels[4].clone().unwrap_or_default();
        assert_eq!(label.name.as_deref(), Some("Mira Holt"));
        let lines = info(&["Thanks!", "Mira Holt wrote:", "> one"]);
        assert_eq!(classify(&lines).kinds, [Body, Attribution, Quote]);
    }

    #[test]
    fn weak_delimiter_needs_a_short_tail() {
        let mut lines = vec!["Intro", "--"];
        lines.extend(std::iter::repeat_n("content", 20));
        let c = classify(&info(&lines));
        assert!(!c.kinds.contains(&Kind::SigDelimiter));
        let c = classify(&info(&["Bye", "--", "Name"]));
        assert_eq!(c.kinds[1], Kind::SigDelimiter);
    }

    #[test]
    fn delimiter_without_content_is_not_a_signature() {
        let c = classify(&info(&["Bye", "-- ", ""]));
        assert_eq!(c.kinds[1], Kind::Body);
    }

    #[test]
    fn outlook_header_block_becomes_quoted_tail() {
        let lines = info(&[
            "Fine by me.",
            "",
            "________________________________",
            "From: Mira Holt <mira@example.org>",
            "Sent: Wednesday, October 21, 2026 08:31",
            "To: Juniper Tran",
            "Subject: Plot rota",
            "",
            "Original text",
        ]);
        let c = classify(&lines);
        assert_eq!(c.kinds[0], Kind::Body);
        assert_eq!(c.kinds[2], Kind::Quote);
        assert_eq!(c.kinds[7], Kind::Blank);
        assert_eq!(c.kinds[8], Kind::Quote);
        assert_eq!(c.depths[8], 1);
        let label = c.labels[2].clone().unwrap_or_default();
        assert_eq!(label.name.as_deref(), Some("Mira Holt"));
        assert_eq!(label.date.as_deref(), Some("Oct 21"));
    }

    #[test]
    fn forwarded_header_block_stays_visible() {
        let lines = info(&[
            "---------- Forwarded message ---------",
            "From: Mira Holt <mira@example.org>",
            "Date: Wed, Oct 21, 2026",
            "Subject: Plot rota",
            "",
            "Text",
        ]);
        let c = classify(&lines);
        assert!(!c.kinds.contains(&Kind::Quote));
    }

    fn rows(kinds: &[Kind]) -> Vec<Row<'static>> {
        kinds
            .iter()
            .map(|&kind| Row { kind, label: None })
            .collect()
    }

    #[test]
    fn folds_long_quote_runs_with_attribution() {
        use Kind::*;
        let r = rows(&[
            Body,
            Attribution,
            Blank,
            Quote,
            Quote,
            Blank,
            Quote,
            Quote,
            Body,
        ]);
        let f = folds(&r, true);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].start_line, 1);
        assert_eq!(f[0].line_count, 7);
        assert_eq!(f[0].summary, "5 quoted lines");
    }

    #[test]
    fn short_quotes_and_disabled_folding_stay_open() {
        use Kind::*;
        let r = rows(&[Quote, Quote, Quote, Body]);
        assert!(folds(&r, true).is_empty());
        let r = rows(&[Quote, Quote, Quote, Quote]);
        assert!(folds(&r, false).is_empty());
        assert_eq!(folds(&r, true).len(), 1);
    }

    #[test]
    fn folds_signatures() {
        use Kind::*;
        let r = rows(&[Body, SigDelimiter, Signature, Signature, Signature]);
        let f = folds(&r, true);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start_line, f[0].line_count), (1, 4));
        assert_eq!(f[0].summary, "signature, 3 lines");
        let r = rows(&[SigDelimiter, Signature]);
        assert_eq!(folds(&r, true)[0].summary, "signature, 1 line");
    }

    #[test]
    fn extracts_dates() {
        assert_eq!(extract_date("Wed, 21 Oct 2026"), Some("Oct 21".into()));
        assert_eq!(extract_date("October 3"), Some("Oct 3".into()));
        assert_eq!(extract_date("10/21/26"), None);
        assert_eq!(extract_date("Mar 99"), None);
        assert_eq!(extract_date("martes 21"), None);
    }
}
