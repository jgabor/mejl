//! Search: the search bar with live local results, server results merged
//! in, and results opened as a list on the mail screen.
//!
//! Every keystroke runs [`MailData::search_local`](crate::MailData), which
//! the store answers from its index in a few milliseconds and which
//! reports operators it cannot run, and
//! [`MailData::search_filter_preview`](crate::MailData), which shows the
//! JMAP filter the server search would send. Enter (or `Ctrl-r`) also asks the
//! server with [`Cmd::ServerSearch`](crate::Cmd::ServerSearch); its rows
//! are added below the local ones. Enter on a result opens all results as
//! a list on the mail screen, with the reader on that result; `Esc` from
//! there puts the previous mailbox and cursor back.

use std::sync::Arc;

use crate::data::{ConversationRow, Highlight, MailboxId, SearchIssueKind, SearchResults};
use crate::input::LineInput;
use crate::state::{Focus, ListState};

/// Local results asked for per keystroke.
pub(crate) const LOCAL_LIMIT: usize = 50;
/// Server results asked for.
pub(crate) const SERVER_LIMIT: usize = 100;

/// Where a result came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The local index.
    Local,
    /// The server search.
    Server,
}

/// One merged result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRow {
    /// Where it came from.
    pub source: Source,
    /// The conversation.
    pub row: ConversationRow,
    /// The mailbox it was found in, for archive and move.
    pub mailbox: Option<MailboxId>,
    /// Subject with matches marked.
    pub subject: Highlight,
    /// Snippet with matches marked.
    pub snippet: Highlight,
}

/// The server search for the current query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerSearch {
    /// Asked; no answer yet.
    Searching {
        /// The query sent.
        query: String,
    },
    /// Answered.
    Done {
        /// The query sent.
        query: String,
        /// What it found.
        rows: Vec<ConversationRow>,
    },
    /// Failed.
    Failed {
        /// The query sent.
        query: String,
        /// What went wrong.
        error: String,
    },
}

impl ServerSearch {
    pub(crate) fn query(&self) -> &str {
        match self {
            ServerSearch::Searching { query }
            | ServerSearch::Done { query, .. }
            | ServerSearch::Failed { query, .. } => query,
        }
    }
}

/// What the mail screen looked like before search, to restore it.
#[derive(Clone, Debug)]
pub(crate) struct Saved {
    pub(crate) mailbox: Option<MailboxId>,
    pub(crate) list: ListState,
    pub(crate) focus: Focus,
    pub(crate) reader_full: bool,
}

/// The search screen.
#[derive(Clone, Debug)]
pub struct SearchState {
    pub(crate) input: LineInput,
    pub(crate) local: SearchResults,
    pub(crate) server: Option<ServerSearch>,
    /// The JMAP filter the server search would send for the query as typed
    /// now, from [`MailData::search_filter_preview`](crate::MailData).
    pub(crate) jmap: Option<String>,
    pub(crate) rows: Vec<ResultRow>,
    pub(crate) cursor: usize,
    pub(crate) scroll: usize,
    pub(crate) results_focused: bool,
    pub(crate) prev: Saved,
}

impl SearchState {
    pub(crate) fn new(query: &str, prev: Saved) -> SearchState {
        SearchState {
            input: LineInput::new(query),
            local: SearchResults::default(),
            server: None,
            jmap: None,
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            results_focused: false,
            prev,
        }
    }

    /// Merges local hits and server rows, local first, one row per
    /// conversation.
    pub(crate) fn merge(&mut self) {
        self.rows = merged(&self.local, self.server.as_ref(), self.input.text());
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// Server rows for the query as typed now, if any.
    pub(crate) fn server_count(&self) -> Option<usize> {
        match &self.server {
            Some(ServerSearch::Done { query, .. }) if query == self.input.text() => Some(
                self.rows
                    .iter()
                    .filter(|r| r.source == Source::Server)
                    .count(),
            ),
            _ => None,
        }
    }
}

pub(crate) fn merged(
    local: &SearchResults,
    server: Option<&ServerSearch>,
    query: &str,
) -> Vec<ResultRow> {
    let mut rows: Vec<ResultRow> = local
        .hits
        .iter()
        .map(|h| ResultRow {
            source: Source::Local,
            row: h.row.clone(),
            mailbox: h.mailbox.clone(),
            subject: h.subject.clone(),
            snippet: h.snippet.clone(),
        })
        .collect();
    if let Some(ServerSearch::Done {
        query: q,
        rows: found,
    }) = server
        && q == query
    {
        for r in found {
            if rows.iter().any(|x| x.row.thread_id == r.thread_id) {
                continue;
            }
            rows.push(ResultRow {
                source: Source::Server,
                subject: Highlight {
                    text: r.subject.clone(),
                    matches: Vec::new(),
                },
                snippet: Highlight {
                    text: r.preview.clone(),
                    matches: Vec::new(),
                },
                row: r.clone(),
                mailbox: None,
            });
        }
    }
    rows
}

/// Search results shown as a list on the mail screen.
#[derive(Clone, Debug)]
pub struct ResultsList {
    pub(crate) query: String,
    pub(crate) rows: Arc<Vec<ResultRow>>,
    pub(crate) server: Option<ServerSearch>,
    pub(crate) prev: Saved,
}

/// One operator chip parsed from the query, for the filter line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    pub(crate) text: String,
    pub(crate) skipped: bool,
}

/// Splits a query into chips: `key:value` terms, quoted phrases and words.
pub(crate) fn chips(query: &str, skipped: &[String]) -> Vec<Chip> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let push = |t: &mut String, out: &mut Vec<Chip>| {
        let s = t.trim();
        if !s.is_empty() {
            let text = match s.split_once(':') {
                Some((k, v)) if !k.is_empty() && !k.contains('"') => format!("{k}: {v}"),
                _ => s.to_owned(),
            };
            out.push(Chip {
                skipped: skipped.iter().any(|x| x == s),
                text,
            });
        }
        t.clear();
    };
    for c in query.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ' ' if !quoted => push(&mut cur, &mut out),
            _ => cur.push(c),
        }
    }
    push(&mut cur, &mut out);
    out
}

/// Plain words for a skipped term.
pub(crate) fn issue_text(kind: SearchIssueKind) -> &'static str {
    match kind {
        SearchIssueKind::UnsupportedOperator => "not in the local index",
        SearchIssueKind::UnsupportedValue => "value not known locally",
        SearchIssueKind::UnknownMailbox => "no such mailbox",
        SearchIssueKind::BadDate => "dates are YYYY-MM-DD, YYYY-MM or YYYY",
        SearchIssueKind::BadSize => "sizes are a number with k, m or g",
        SearchIssueKind::EmptyValue => "nothing after the colon",
        SearchIssueKind::MisplacedOr => "OR works between words only",
        SearchIssueKind::Grouping => "parentheses are not supported locally",
    }
}

/// The syntax help shown under the results: operator, meaning, twice
/// per row.
pub(crate) const SYNTAX: [(&str, &str, &str, &str); 6] = [
    ("from:name", "sender", "is:unread", "unread"),
    ("to:name", "To, Cc or Bcc", "is:pinned", "pinned"),
    ("subject:word", "subject", "in:Receipts", "in a mailbox"),
    (
        "has:attachment",
        "has an attachment",
        "before: after:",
        "YYYY-MM-DD dates",
    ),
    ("\"exact phrase\"", "phrase", "-word", "not"),
    ("word OR word", "either word", "larger:2m", "size over 2 MB"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Participant, SearchHit, ThreadId};

    fn row(t: &str) -> ConversationRow {
        ConversationRow {
            thread_id: ThreadId::from(t),
            latest_email_id: format!("e{t}").into(),
            from: Participant::new("Ada", "ada@example.com"),
            participants: Vec::new(),
            subject: format!("Subject {t}"),
            preview: "preview".into(),
            date: "2026-10-23T10:00:00Z".parse().unwrap(),
            message_count: 1,
            unread: false,
            flagged: false,
            has_attachment: false,
            masked: false,
        }
    }

    #[test]
    fn merge_dedups_and_ignores_stale_server_answers() {
        let local = SearchResults {
            hits: vec![SearchHit {
                row: row("a"),
                mailbox: None,
                subject: Highlight::default(),
                snippet: Highlight::default(),
            }],
            ..SearchResults::default()
        };
        let server = ServerSearch::Done {
            query: "q".into(),
            rows: vec![row("a"), row("b")],
        };
        let rows = merged(&local, Some(&server), "q");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].source, Source::Server);
        assert_eq!(merged(&local, Some(&server), "other").len(), 1);
        assert_eq!(server.query(), "q");
    }

    #[test]
    fn chips_split_terms() {
        let c = chips(
            "from:larkspur \"exact phrase\" has:attachment deliveredto:x",
            &["deliveredto:x".into()],
        );
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "from: larkspur",
                "\"exact phrase\"",
                "has: attachment",
                "deliveredto: x"
            ]
        );
        assert!(c[3].skipped && !c[0].skipped);
        assert!(chips("  ", &[]).is_empty());
    }
}
