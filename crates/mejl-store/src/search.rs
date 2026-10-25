//! Local search over the FTS5 index.
//!
//! The query language is the part of Fastmail's search syntax the local
//! index can answer:
//!
//! - plain words and `"quoted phrases"`, matched against subject, sender,
//!   recipients, preview and fetched body text;
//! - `from:`, `to:` (To, Cc and Bcc, as Fastmail means it), `subject:` and
//!   `body:`, each taking a word or a quoted phrase;
//! - `is:unread`, `is:read`, `is:pinned` (also `is:flagged`,
//!   `is:starred`), `is:answered`, `is:draft`;
//! - `has:attachment`;
//! - `in:<mailbox>` by role (`inbox`, `sent`, `spam` and so on), name or
//!   path such as `in:"Projects/Garden club"`;
//! - `before:`, `after:` and `date:` with `YYYY-MM-DD`, `YYYY-MM` or
//!   `YYYY`; `after:` includes the day it names, `before:` excludes it;
//! - `larger:` and `smaller:` (also `minsize:` and `maxsize:`) with an
//!   optional `k`, `m` or `g` suffix;
//! - `-` before any term to negate it, and `OR` between two text terms.
//!
//! Operators Fastmail has but the local index cannot answer (`cc:`,
//! `fromin:`, `filename:`, `memo:` and others), unknown values, unknown
//! mailboxes, bad dates and parentheses are reported in
//! [`SearchResults::issues`] and left out of the local query, so the caller
//! can say so and rely on the server search for them.
//!
//! As the user types, the last word is matched as a prefix (a single letter
//! after other words waits for the next letter). Messages only
//! in Trash or Spam are left out unless `in:` names a mailbox or
//! [`SearchOptions::include_trash_and_spam`] is set.
//!
//! Ranking is bm25 with subject weighted highest, then sender, recipients,
//! preview and body. To stay inside a frame on large mailboxes, a query
//! with more than 2,000 matches ranks only the 2,000 newest; if filters
//! then leave fewer hits than the limit, it widens to 20,000 and then to
//! every match. Below 2,000 matches the ranking is exact. Queries with only
//! filters return the newest matches first.

use jiff::ToSpan;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};

use crate::codec::usize_i64;
use crate::error::Result;
use crate::model::{Highlighted, MessageRow};
use crate::read::{RawRow, message_row, row_columns};

/// Options for a local search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Most hits to return.
    pub limit: usize,
    /// Match the last word as a prefix, for search as you type. Only when
    /// the query does not end in a space.
    pub prefix_last: bool,
    /// Also return messages that are only in Trash or Spam.
    pub include_trash_and_spam: bool,
    /// The time zone for `before:`, `after:` and `date:`.
    pub time_zone: TimeZone,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            prefix_last: true,
            include_trash_and_spam: false,
            time_zone: TimeZone::UTC,
        }
    }
}

/// Local search results, best first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    /// Hits, best first.
    pub hits: Vec<SearchHit>,
    /// Parts of the query the local index could not answer.
    pub issues: Vec<QueryIssue>,
    /// Whether any full-text term was used. Without one, hits are only
    /// filtered and come newest first.
    pub full_text: bool,
}

/// One local search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The message, as a list row.
    pub row: MessageRow,
    /// The subject with matched words marked.
    pub subject: Highlighted,
    /// The best matching fragment of any field, with matches marked.
    pub snippet: Highlighted,
}

/// A part of the query that was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIssue {
    /// The query text it is about.
    pub term: String,
    /// What is wrong with it.
    pub kind: IssueKind,
}

/// Why a part of the query was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// A Fastmail operator the local index cannot answer.
    UnsupportedOperator,
    /// A known operator with a value it does not take, such as `is:muted`.
    UnsupportedValue,
    /// `in:` names no cached mailbox.
    UnknownMailbox,
    /// A date that is not `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
    BadDate,
    /// A size that is not a number with an optional unit.
    BadSize,
    /// An operator with nothing after the colon.
    EmptyValue,
    /// `OR` not between two text terms.
    MisplacedOr,
    /// Parentheses, which group nothing here.
    Grouping,
}

const MARK_START: char = '\u{2}';
const MARK_END: char = '\u{3}';

const UNSUPPORTED: &[&str] = &[
    "cc",
    "bcc",
    "fromin",
    "filename",
    "memo",
    "keyword",
    "tag",
    "list",
    "deliveredto",
    "msgid",
    "header",
    "priority",
    "anywhere",
    "snoozed",
];

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Text {
        neg: bool,
        text: String,
        quoted: bool,
    },
    Op {
        neg: bool,
        key: String,
        value: String,
        quoted: bool,
        raw: String,
    },
    Or,
}

fn tokenize(query: &str, issues: &mut Vec<QueryIssue>) -> Vec<Token> {
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let mut grouped = false;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let neg = chars[i] == '-' && chars.get(i + 1).is_some_and(|c| !c.is_whitespace());
        if neg {
            i += 1;
        }
        if chars[i] == '"' {
            let (text, next) = read_quoted(&chars, i + 1);
            i = next;
            out.push(Token::Text {
                neg,
                text,
                quoted: true,
            });
            continue;
        }
        let mut raw = String::new();
        let mut quoted_value = None;
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == ':'
                && chars.get(i + 1) == Some(&'"')
                && !raw.is_empty()
                && raw.chars().all(|c| c.is_ascii_alphabetic())
            {
                let (value, next) = read_quoted(&chars, i + 2);
                quoted_value = Some(value);
                i = next;
                break;
            }
            raw.push(chars[i]);
            i += 1;
        }
        if let Some(value) = quoted_value {
            out.push(Token::Op {
                neg,
                key: raw.to_ascii_lowercase(),
                raw: format!("{raw}:\"{value}\""),
                value,
                quoted: true,
            });
            continue;
        }
        if raw.starts_with('(') || raw.ends_with(')') {
            grouped = true;
            raw = raw.trim_matches(|c| c == '(' || c == ')').to_owned();
            if raw.is_empty() {
                continue;
            }
        }
        if raw == "OR" && !neg {
            out.push(Token::Or);
            continue;
        }
        if let Some((key, value)) = raw.split_once(':')
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphabetic())
            && is_operator(&key.to_ascii_lowercase())
        {
            out.push(Token::Op {
                neg,
                key: key.to_ascii_lowercase(),
                value: value.to_owned(),
                quoted: false,
                raw: raw.clone(),
            });
            continue;
        }
        out.push(Token::Text {
            neg,
            text: raw,
            quoted: false,
        });
    }
    if grouped {
        issues.push(QueryIssue {
            term: "( )".into(),
            kind: IssueKind::Grouping,
        });
    }
    out
}

fn read_quoted(chars: &[char], mut i: usize) -> (String, usize) {
    let mut text = String::new();
    while i < chars.len() && chars[i] != '"' {
        text.push(chars[i]);
        i += 1;
    }
    // Step over the closing quote, if there is one.
    let next = if i < chars.len() { i + 1 } else { i };
    (text, next)
}

fn is_operator(key: &str) -> bool {
    matches!(
        key,
        "from"
            | "to"
            | "subject"
            | "body"
            | "is"
            | "has"
            | "in"
            | "before"
            | "after"
            | "date"
            | "larger"
            | "smaller"
            | "minsize"
            | "maxsize"
    ) || UNSUPPORTED.contains(&key)
}

#[derive(Debug, Clone, PartialEq)]
struct FtsTerm {
    column: Option<&'static str>,
    text: String,
    prefix: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Cond {
    Seen(bool),
    Flagged(bool),
    Answered(bool),
    Draft(bool),
    Attachment(bool),
    InMailbox {
        name: String,
        neg: bool,
    },
    Received {
        from: Option<i64>,
        until: Option<i64>,
        neg: bool,
    },
    Size {
        min: Option<i64>,
        max: Option<i64>,
        neg: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Parsed {
    /// AND of ORs.
    groups: Vec<Vec<FtsTerm>>,
    negated: Vec<FtsTerm>,
    conds: Vec<Cond>,
    issues: Vec<QueryIssue>,
}

fn parse(query: &str, options: &SearchOptions) -> Parsed {
    let mut parsed = Parsed::default();
    let tokens = tokenize(query, &mut parsed.issues);
    let ends_open = options.prefix_last && !query.ends_with(char::is_whitespace);
    let last = tokens.len().checked_sub(1);
    let mut pending_or = false;
    let mut last_positive_text = false;

    for (index, token) in tokens.iter().enumerate() {
        let is_last = Some(index) == last && ends_open;
        let term = match token {
            Token::Or => {
                if last_positive_text && !pending_or {
                    pending_or = true;
                } else {
                    parsed.issues.push(QueryIssue {
                        term: "OR".into(),
                        kind: IssueKind::MisplacedOr,
                    });
                }
                continue;
            }
            Token::Text { neg, text, quoted } => Some((
                *neg,
                FtsTerm {
                    column: None,
                    text: text.clone(),
                    prefix: is_last && !quoted && !neg,
                },
            )),
            Token::Op {
                neg,
                key,
                value,
                quoted,
                raw,
            } => {
                let column = match key.as_str() {
                    "from" => Some("from_text"),
                    "to" => Some("to_text"),
                    "subject" => Some("subject"),
                    "body" => Some("body_text"),
                    _ => None,
                };
                if value.trim().is_empty() {
                    parsed.issues.push(QueryIssue {
                        term: raw.clone(),
                        kind: IssueKind::EmptyValue,
                    });
                    None
                } else if let Some(column) = column {
                    Some((
                        *neg,
                        FtsTerm {
                            column: Some(column),
                            text: value.clone(),
                            prefix: is_last && !quoted && !neg,
                        },
                    ))
                } else {
                    match condition(key, value, *neg, &options.time_zone) {
                        Ok(cond) => parsed.conds.push(cond),
                        Err(kind) => parsed.issues.push(QueryIssue {
                            term: raw.clone(),
                            kind,
                        }),
                    }
                    None
                }
            }
        };
        match term {
            Some((false, term)) if has_words(&term.text) => {
                if pending_or && let Some(group) = parsed.groups.last_mut() {
                    group.push(term);
                } else {
                    parsed.groups.push(vec![term]);
                }
                pending_or = false;
                last_positive_text = true;
            }
            Some((true, term)) if has_words(&term.text) => {
                parsed.negated.push(term);
                close_or(&mut parsed, &mut pending_or);
                last_positive_text = false;
            }
            _ => {
                close_or(&mut parsed, &mut pending_or);
                last_positive_text = false;
            }
        }
    }
    close_or(&mut parsed, &mut pending_or);
    drop_short_prefix(&mut parsed);
    parsed
}

/// While typing, a last word of one letter next to other words is left out
/// until the next letter arrives. Its prefix matches a large part of any
/// mailbox, and joining that with the other words cost about 30 ms on 100k
/// messages, for results the next keystroke replaces anyway. Alone, it is
/// still searched.
fn drop_short_prefix(parsed: &mut Parsed) {
    let terms: usize = parsed.groups.iter().map(Vec::len).sum();
    if terms < 2 {
        return;
    }
    for group in &mut parsed.groups {
        group.retain(|t| !(t.prefix && t.text.chars().count() == 1));
    }
    parsed.groups.retain(|g| !g.is_empty());
}

fn close_or(parsed: &mut Parsed, pending_or: &mut bool) {
    if *pending_or {
        parsed.issues.push(QueryIssue {
            term: "OR".into(),
            kind: IssueKind::MisplacedOr,
        });
        *pending_or = false;
    }
}

/// Whether the tokenizer would find any word in this text. A phrase
/// without one would make FTS5 match nothing.
fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

fn condition(key: &str, value: &str, neg: bool, tz: &TimeZone) -> Result<Cond, IssueKind> {
    let v = value.to_lowercase();
    let flag = |on: bool| on != neg;
    match key {
        "is" => match v.as_str() {
            "unread" => Ok(Cond::Seen(flag(false))),
            "read" | "seen" => Ok(Cond::Seen(flag(true))),
            "pinned" | "flagged" | "starred" => Ok(Cond::Flagged(flag(true))),
            "answered" | "replied" => Ok(Cond::Answered(flag(true))),
            "draft" => Ok(Cond::Draft(flag(true))),
            _ => Err(IssueKind::UnsupportedValue),
        },
        "has" => match v.as_str() {
            "attachment" | "attachments" | "attach" => Ok(Cond::Attachment(flag(true))),
            _ => Err(IssueKind::UnsupportedValue),
        },
        "in" => Ok(Cond::InMailbox {
            name: value.to_owned(),
            neg,
        }),
        "before" | "after" | "date" => {
            let (start, end) = date_range(&v, tz).ok_or(IssueKind::BadDate)?;
            let (from, until) = match key {
                "before" => (None, Some(start)),
                "after" => (Some(start), None),
                _ => (Some(start), Some(end)),
            };
            Ok(Cond::Received { from, until, neg })
        }
        "larger" | "minsize" => Ok(Cond::Size {
            min: Some(parse_size(&v).ok_or(IssueKind::BadSize)?),
            max: None,
            neg,
        }),
        "smaller" | "maxsize" => Ok(Cond::Size {
            min: None,
            max: Some(parse_size(&v).ok_or(IssueKind::BadSize)?),
            neg,
        }),
        _ => Err(IssueKind::UnsupportedOperator),
    }
}

/// The start of the period a date names and the start of the next one, in
/// milliseconds.
fn date_range(value: &str, tz: &TimeZone) -> Option<(i64, i64)> {
    let parts: Vec<&str> = value.split(['-', '/']).collect();
    let num = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let (start, end) = match parts.as_slice() {
        [y] => {
            let y = i16::try_from(num(y)?).ok()?;
            let start = Date::new(y, 1, 1).ok()?;
            (start, start.checked_add(1.year()).ok()?)
        }
        [y, m] => {
            let y = i16::try_from(num(y)?).ok()?;
            let m = i8::try_from(num(m)?).ok()?;
            let start = Date::new(y, m, 1).ok()?;
            (start, start.checked_add(1.month()).ok()?)
        }
        [y, m, d] => {
            let y = i16::try_from(num(y)?).ok()?;
            let m = i8::try_from(num(m)?).ok()?;
            let d = i8::try_from(num(d)?).ok()?;
            let start = Date::new(y, m, d).ok()?;
            (start, start.checked_add(1.day()).ok()?)
        }
        _ => return None,
    };
    let ms = |d: Date| -> Option<i64> {
        Some(d.to_zoned(tz.clone()).ok()?.timestamp().as_millisecond())
    };
    Some((ms(start)?, ms(end)?))
}

fn parse_size(value: &str) -> Option<i64> {
    let value = value.trim().trim_end_matches('b');
    let (digits, unit) = match value.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((pos, _)) => value.split_at(pos),
        None => (value, ""),
    };
    let n: i64 = digits.parse().ok()?;
    let mult: i64 = match unit {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        _ => return None,
    };
    n.checked_mul(mult)
}

fn phrase(term: &FtsTerm) -> String {
    let mut out = String::new();
    if let Some(column) = term.column {
        out.push_str(column);
        out.push_str(" : ");
    }
    out.push('"');
    out.push_str(&term.text.replace('"', "\"\""));
    out.push('"');
    if term.prefix {
        out.push_str(" *");
    }
    out
}

fn match_expression(groups: &[Vec<FtsTerm>]) -> String {
    groups
        .iter()
        .map(|group| {
            let inner: Vec<String> = group.iter().map(phrase).collect();
            if inner.len() == 1 {
                inner.into_iter().next().unwrap_or_default()
            } else {
                format!("({})", inner.join(" OR "))
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

struct MailboxInfo {
    id: String,
    name: String,
    role: Option<String>,
    parent: Option<String>,
}

fn resolve_mailbox(mailboxes: &[MailboxInfo], wanted: &str) -> Vec<String> {
    let wanted = wanted.trim().trim_matches('/').to_lowercase();
    let role = match wanted.as_str() {
        "inbox" => Some("inbox"),
        "archive" => Some("archive"),
        "sent" => Some("sent"),
        "drafts" | "draft" => Some("drafts"),
        "trash" | "bin" => Some("trash"),
        "spam" | "junk" => Some("junk"),
        _ => None,
    };
    let path_of = |m: &MailboxInfo| -> String {
        let mut segments = vec![m.name.clone()];
        let mut parent = m.parent.clone();
        let mut guard = 0;
        while let Some(p) = parent {
            guard += 1;
            if guard > 64 {
                break;
            }
            match mailboxes.iter().find(|x| x.id == p) {
                Some(x) => {
                    segments.push(x.name.clone());
                    parent = x.parent.clone();
                }
                None => break,
            }
        }
        segments.reverse();
        segments.join("/").to_lowercase()
    };
    mailboxes
        .iter()
        .filter(|m| {
            (role.is_some() && m.role.as_deref() == role)
                || m.name.to_lowercase() == wanted
                || path_of(m) == wanted
        })
        .map(|m| m.id.clone())
        .collect()
}

fn highlighted(text: &str) -> Highlighted {
    let mut out = Highlighted::default();
    let mut start = None;
    for c in text.chars() {
        match c {
            MARK_START => start = Some(out.text.len()),
            MARK_END => {
                if let Some(s) = start.take() {
                    out.matches.push(s..out.text.len());
                }
            }
            other => out.text.push(other),
        }
    }
    out
}

/// How many of the newest matches a search ranks, widening in steps when
/// filters leave fewer hits than asked for, then ranking every match.
/// Email row ids follow received time, so the newest matches are the
/// highest row ids, which the index walks first.
const RANK_WINDOWS: [usize; 2] = [2_000, 20_000];

fn columns_sql(columns: &str, where_sql: &str, bounded: bool) -> String {
    let bound = if bounded {
        "AND email_fts.rowid >= ?"
    } else {
        ""
    };
    format!(
        "SELECT {columns},
             highlight(email_fts, 0, char(2), char(3)),
             snippet(email_fts, -1, char(2), char(3), '…', 12)
         FROM email_fts JOIN email e ON e.rid = email_fts.rowid
         WHERE email_fts MATCH ? {bound} AND {where_sql}
         ORDER BY bm25(email_fts, 10.0, 6.0, 3.0, 1.5, 1.0), e.rid DESC
         LIMIT ?"
    )
}

fn ranked(
    conn: &Connection,
    sql: &str,
    expr: &str,
    cutoff: Option<i64>,
    values: &[SqlValue],
    limit: &SqlValue,
) -> Result<Vec<SearchHit>> {
    let mut all = vec![SqlValue::Text(expr.to_owned())];
    if let Some(cutoff) = cutoff {
        all.push(SqlValue::Integer(cutoff));
    }
    all.extend(values.iter().cloned());
    all.push(limit.clone());
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(all), |r| {
        Ok((
            message_row(r)?,
            r.get::<_, Option<String>>(16)?,
            r.get::<_, Option<String>>(17)?,
        ))
    })?;
    let mut hits = Vec::new();
    for row in rows {
        let (raw, subject, snippet) = row?;
        let row = raw.decode()?;
        hits.push(SearchHit {
            subject: highlighted(subject.as_deref().unwrap_or(&row.subject)),
            snippet: highlighted(snippet.as_deref().unwrap_or(&row.preview)),
            row,
        });
    }
    Ok(hits)
}

pub(crate) fn run(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
) -> Result<SearchResults> {
    let mut parsed = parse(query, options);
    let mut wheres: Vec<String> = vec!["e.is_hidden = 0".into()];
    let mut values: Vec<SqlValue> = Vec::new();

    let mut mailboxes = Vec::new();
    if parsed
        .conds
        .iter()
        .any(|c| matches!(c, Cond::InMailbox { .. }))
        || !options.include_trash_and_spam
    {
        let mut stmt = conn.prepare_cached("SELECT id, name, role, parent_id FROM mailbox")?;
        for row in stmt.query_map([], |r| {
            Ok(MailboxInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                role: r.get(2)?,
                parent: r.get(3)?,
            })
        })? {
            mailboxes.push(row?);
        }
    }

    let mut used_in = false;
    let mut has_filters = false;
    for cond in &parsed.conds {
        let (sql, neg) = match cond {
            Cond::Seen(on) => (format!("e.is_seen = {}", i64::from(*on)), false),
            Cond::Flagged(on) => (format!("e.is_flagged = {}", i64::from(*on)), false),
            Cond::Answered(on) => (format!("e.is_answered = {}", i64::from(*on)), false),
            Cond::Draft(on) => (format!("e.is_draft = {}", i64::from(*on)), false),
            Cond::Attachment(on) => (format!("e.has_attachment = {}", i64::from(*on)), false),
            Cond::InMailbox { name, neg } => {
                let ids = resolve_mailbox(&mailboxes, name);
                if ids.is_empty() {
                    parsed.issues.push(QueryIssue {
                        term: format!("in:{name}"),
                        kind: IssueKind::UnknownMailbox,
                    });
                    continue;
                }
                if !neg {
                    used_in = true;
                }
                let marks = vec!["?"; ids.len()].join(", ");
                values.extend(ids.into_iter().map(SqlValue::Text));
                (
                    format!(
                        "EXISTS (SELECT 1 FROM email_mailbox em WHERE em.email_rid = e.rid
                         AND em.mailbox_id IN ({marks}))"
                    ),
                    *neg,
                )
            }
            Cond::Received { from, until, neg } => {
                let mut parts = Vec::new();
                if let Some(from) = from {
                    parts.push("e.received_at >= ?".to_owned());
                    values.push(SqlValue::Integer(*from));
                }
                if let Some(until) = until {
                    parts.push("e.received_at < ?".to_owned());
                    values.push(SqlValue::Integer(*until));
                }
                (parts.join(" AND "), *neg)
            }
            Cond::Size { min, max, neg } => {
                let mut parts = Vec::new();
                if let Some(min) = min {
                    parts.push("e.size >= ?".to_owned());
                    values.push(SqlValue::Integer(*min));
                }
                if let Some(max) = max {
                    parts.push("e.size < ?".to_owned());
                    values.push(SqlValue::Integer(*max));
                }
                (parts.join(" AND "), *neg)
            }
        };
        has_filters = true;
        wheres.push(if neg { format!("NOT ({sql})") } else { sql });
    }

    if !used_in && !options.include_trash_and_spam {
        let excluded: Vec<String> = mailboxes
            .iter()
            .filter(|m| matches!(m.role.as_deref(), Some("trash" | "junk")))
            .map(|m| m.id.clone())
            .collect();
        if excluded.is_empty() {
            wheres
                .push("EXISTS (SELECT 1 FROM email_mailbox em WHERE em.email_rid = e.rid)".into());
        } else {
            let marks = vec!["?"; excluded.len()].join(", ");
            values.extend(excluded.into_iter().map(SqlValue::Text));
            wheres.push(format!(
                "EXISTS (SELECT 1 FROM email_mailbox em WHERE em.email_rid = e.rid
                 AND em.mailbox_id NOT IN ({marks}))"
            ));
        }
    }

    if !parsed.negated.is_empty() {
        let expr = parsed
            .negated
            .iter()
            .map(phrase)
            .collect::<Vec<_>>()
            .join(" OR ");
        wheres.push("e.rid NOT IN (SELECT rowid FROM email_fts WHERE email_fts MATCH ?)".into());
        values.push(SqlValue::Text(expr));
        has_filters = true;
    }

    let limit = SqlValue::Integer(usize_i64(options.limit));
    let full_text = !parsed.groups.is_empty();
    let columns = row_columns();
    let where_sql = wheres.join(" AND ");
    let mut hits = Vec::new();
    if full_text {
        let expr = match_expression(&parsed.groups);
        let mut done = false;
        for window in RANK_WINDOWS {
            let cutoff: Option<i64> = conn
                .prepare_cached(
                    "SELECT rowid FROM email_fts WHERE email_fts MATCH ?1
                     ORDER BY rowid DESC LIMIT 1 OFFSET ?2",
                )?
                .query_row(
                    rusqlite::params![expr, usize_i64(window.saturating_sub(1))],
                    |r| r.get(0),
                )
                .optional()?;
            // No row at that offset: every match fits in the window, so
            // ranking them all is exact and cheap.
            hits = ranked(
                conn,
                &columns_sql(columns, &where_sql, cutoff.is_some()),
                &expr,
                cutoff,
                &values,
                &limit,
            )?;
            if cutoff.is_none() || hits.len() >= options.limit {
                done = true;
                break;
            }
        }
        if !done {
            hits = ranked(
                conn,
                &columns_sql(columns, &where_sql, false),
                &expr,
                None,
                &values,
                &limit,
            )?;
        }
    } else if has_filters {
        let sql = format!(
            "SELECT {columns} FROM email e WHERE {where_sql}
             ORDER BY e.rid DESC LIMIT ?"
        );
        values.push(limit);
        let mut stmt = conn.prepare_cached(&sql)?;
        let raw: Vec<RawRow> = stmt
            .query_map(rusqlite::params_from_iter(values), message_row)?
            .collect::<Result<_, _>>()?;
        for raw in raw {
            let row = raw.decode()?;
            hits.push(SearchHit {
                subject: Highlighted {
                    text: row.subject.clone(),
                    matches: Vec::new(),
                },
                snippet: Highlighted {
                    text: row.preview.clone(),
                    matches: Vec::new(),
                },
                row,
            });
        }
    }
    Ok(SearchResults {
        hits,
        issues: parsed.issues,
        full_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::ms_ts;

    fn opts() -> SearchOptions {
        SearchOptions::default()
    }

    fn kinds(p: &Parsed) -> Vec<IssueKind> {
        p.issues.iter().map(|i| i.kind).collect()
    }

    #[test]
    fn words_phrases_and_prefix() {
        let p = parse("garden \"plot rota\" jun", &opts());
        assert_eq!(
            match_expression(&p.groups),
            "\"garden\" AND \"plot rota\" AND \"jun\" *"
        );
        let p = parse("garden jun ", &opts());
        assert_eq!(match_expression(&p.groups), "\"garden\" AND \"jun\"");
        let p = parse("\"plot rota", &opts());
        assert_eq!(match_expression(&p.groups), "\"plot rota\"");
    }

    #[test]
    fn one_letter_prefix_waits_when_other_words_exist() {
        let p = parse("garden r", &opts());
        assert_eq!(match_expression(&p.groups), "\"garden\"");
        let p = parse("g", &opts());
        assert_eq!(match_expression(&p.groups), "\"g\" *");
        let p = parse("garden r ", &opts());
        assert_eq!(match_expression(&p.groups), "\"garden\" AND \"r\"");
        let p = parse("garden OR r", &opts());
        assert_eq!(match_expression(&p.groups), "\"garden\"");
    }

    #[test]
    fn column_operators() {
        let p = parse(
            "from:juniper subject:\"plot rota\" to:ada@example.org",
            &opts(),
        );
        assert_eq!(
            match_expression(&p.groups),
            "from_text : \"juniper\" AND subject : \"plot rota\" AND to_text : \"ada@example.org\" *"
        );
    }

    #[test]
    fn quotes_are_escaped() {
        let p = parse(
            "say\"hi",
            &SearchOptions {
                prefix_last: false,
                ..opts()
            },
        );
        assert_eq!(match_expression(&p.groups), "\"say\"\"hi\"");
    }

    #[test]
    fn or_between_terms() {
        let p = parse("garden OR allotment rota", &opts());
        assert_eq!(
            match_expression(&p.groups),
            "(\"garden\" OR \"allotment\") AND \"rota\" *"
        );
        let p = parse("OR garden", &opts());
        assert_eq!(kinds(&p), vec![IssueKind::MisplacedOr]);
        let p = parse("garden OR is:unread", &opts());
        assert_eq!(kinds(&p), vec![IssueKind::MisplacedOr]);
        let p = parse("garden OR", &opts());
        assert_eq!(kinds(&p), vec![IssueKind::MisplacedOr]);
        // Lowercase "or" is a word.
        let p = parse("this or that", &opts());
        assert_eq!(p.groups.len(), 3);
    }

    #[test]
    fn filters_and_negation() {
        let p = parse("is:unread -is:pinned has:attachment -spam", &opts());
        assert_eq!(
            p.conds,
            vec![
                Cond::Seen(false),
                Cond::Flagged(false),
                Cond::Attachment(true)
            ]
        );
        assert_eq!(p.negated.len(), 1);
        assert!(!p.negated[0].prefix);
        assert!(p.groups.is_empty());
    }

    #[test]
    fn unsupported_and_bad_values_are_reported() {
        let p = parse(
            "cc:ada fromin:vips is:muted has:link before:yesterday larger:huge from:",
            &opts(),
        );
        assert_eq!(
            kinds(&p),
            vec![
                IssueKind::UnsupportedOperator,
                IssueKind::UnsupportedOperator,
                IssueKind::UnsupportedValue,
                IssueKind::UnsupportedValue,
                IssueKind::BadDate,
                IssueKind::BadSize,
                IssueKind::EmptyValue,
            ]
        );
        assert!(p.groups.is_empty());
    }

    #[test]
    fn unknown_words_with_colons_are_text() {
        let p = parse(
            "Re: meeting at 10:30",
            &SearchOptions {
                prefix_last: false,
                ..opts()
            },
        );
        assert_eq!(
            match_expression(&p.groups),
            "\"Re:\" AND \"meeting\" AND \"at\" AND \"10:30\""
        );
        assert!(p.issues.is_empty());
    }

    #[test]
    fn grouping_is_reported() {
        let p = parse("(garden rota)", &opts());
        assert_eq!(kinds(&p), vec![IssueKind::Grouping]);
        assert_eq!(p.groups.len(), 2);
    }

    #[test]
    fn punctuation_only_terms_are_skipped() {
        let p = parse("garden -- @@ ...", &opts());
        assert_eq!(p.groups.len(), 1);
        assert!(p.negated.is_empty());
    }

    #[test]
    fn dates() {
        let tz = TimeZone::UTC;
        let (s, e) = date_range("2026-10-23", &tz).unwrap();
        assert_eq!(ms_ts(s).to_string(), "2026-10-23T00:00:00Z");
        assert_eq!(ms_ts(e).to_string(), "2026-10-24T00:00:00Z");
        let (s, e) = date_range("2026-12", &tz).unwrap();
        assert_eq!(ms_ts(s).to_string(), "2026-12-01T00:00:00Z");
        assert_eq!(ms_ts(e).to_string(), "2027-01-01T00:00:00Z");
        let (s, e) = date_range("2026", &tz).unwrap();
        assert_eq!(ms_ts(s).to_string(), "2026-01-01T00:00:00Z");
        assert_eq!(ms_ts(e).to_string(), "2027-01-01T00:00:00Z");
        assert!(date_range("2026-02-30", &tz).is_none());
        assert!(date_range("tomorrow", &tz).is_none());
        assert!(date_range("2026-1x", &tz).is_none());
        let plus_two = TimeZone::fixed(jiff::tz::offset(2));
        let (s, _) = date_range("2026-10-23", &plus_two).unwrap();
        assert_eq!(ms_ts(s).to_string(), "2026-10-22T22:00:00Z");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("500"), Some(500));
        assert_eq!(parse_size("10k"), Some(10 * 1024));
        assert_eq!(parse_size("2mb"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("1.5m"), None);
        assert_eq!(parse_size("k"), None);
    }

    #[test]
    fn highlight_markers() {
        let h = highlighted("a \u{2}garden\u{3} and \u{2}rota\u{3}");
        assert_eq!(h.text, "a garden and rota");
        assert_eq!(&h.text[h.matches[0].clone()], "garden");
        assert_eq!(&h.text[h.matches[1].clone()], "rota");
        let h = highlighted("ünï \u{2}cødé\u{3}");
        assert_eq!(&h.text[h.matches[0].clone()], "cødé");
    }

    #[test]
    fn mailbox_resolution() {
        let boxes = vec![
            MailboxInfo {
                id: "i".into(),
                name: "Inbox".into(),
                role: Some("inbox".into()),
                parent: None,
            },
            MailboxInfo {
                id: "j".into(),
                name: "Spam".into(),
                role: Some("junk".into()),
                parent: None,
            },
            MailboxInfo {
                id: "p".into(),
                name: "Projects".into(),
                role: None,
                parent: None,
            },
            MailboxInfo {
                id: "g".into(),
                name: "Garden club".into(),
                role: None,
                parent: Some("p".into()),
            },
        ];
        assert_eq!(resolve_mailbox(&boxes, "inbox"), vec!["i"]);
        assert_eq!(resolve_mailbox(&boxes, "spam"), vec!["j"]);
        assert_eq!(resolve_mailbox(&boxes, "junk"), vec!["j"]);
        assert_eq!(resolve_mailbox(&boxes, "Projects/Garden Club"), vec!["g"]);
        assert_eq!(resolve_mailbox(&boxes, "garden club"), vec!["g"]);
        assert!(resolve_mailbox(&boxes, "nowhere").is_empty());
    }
}
