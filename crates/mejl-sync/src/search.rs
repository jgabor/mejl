//! Server search: Fastmail-style query text to a JMAP filter, then
//! `Email/query` with snippets.

use std::collections::HashMap;
use std::sync::Arc;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use mejl_jmap::method::{GetArgs, QueryArgs};
use mejl_jmap::methods::{
    EmailGet, EmailGetArgs, EmailQuery, EmailQueryArgs, SearchSnippetGet, SearchSnippetGetArgs,
    ThreadGet,
};
use mejl_jmap::types::keyword;
use mejl_jmap::{EmailFilter, EmailFilterCondition, Id, RequestBuilder};
use mejl_store::MailboxRow;

use crate::apply::Apply;
use crate::command::SearchRequest;
use crate::error::SyncError;
use crate::event::{SearchResults, Snippet, SyncEvent};
use crate::shared::Shared;
use crate::sync::{LIST_PROPERTIES, apply_now, newest_first};

/// One search term before mailbox names are resolved.
#[derive(Debug, Clone, PartialEq)]
enum Term {
    Cond(Box<EmailFilterCondition>),
    /// Any of these.
    Any(Vec<Term>),
    Not(Box<Term>),
    /// `in:` with a mailbox name, role or path.
    InMailbox(String),
    /// `in:*`: no mailbox limit.
    Everywhere,
}

/// Query text split into AND-ed groups of OR-ed terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Parsed {
    groups: Vec<Vec<Term>>,
    /// Terms with no JMAP equivalent, as the user typed them.
    pub unsupported: Vec<String>,
}

fn cond(f: impl FnOnce(&mut EmailFilterCondition)) -> Term {
    let mut c = EmailFilterCondition::default();
    f(&mut c);
    Term::Cond(Box::new(c))
}

/// Splits on spaces, keeping quoted runs (`from:"Juniper Tran"`) whole.
fn tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in query.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn unquote(value: &str) -> String {
    value.trim_matches('"').to_owned()
}

fn parse_size(value: &str) -> Option<u64> {
    let lower = value.to_ascii_lowercase();
    let lower = lower.strip_suffix('b').unwrap_or(&lower);
    let (digits, factor) = match lower.chars().last()? {
        'k' => (&lower[..lower.len() - 1], 1024),
        'm' => (&lower[..lower.len() - 1], 1024 * 1024),
        'g' => (&lower[..lower.len() - 1], 1024 * 1024 * 1024),
        _ => (lower, 1),
    };
    let n: u64 = digits.parse().ok()?;
    n.checked_mul(factor)
}

/// The start and end of a day, month or year in `tz`.
fn date_range(value: &str, tz: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    let parts: Vec<&str> = value.split('-').collect();
    let num = |s: &str| s.parse::<i16>().ok();
    let (start, end) = match parts.as_slice() {
        [y] => {
            let y = num(y)?;
            (
                Date::new(y, 1, 1).ok()?,
                Date::new(y.checked_add(1)?, 1, 1).ok()?,
            )
        }
        [y, m] => {
            let (y, m) = (num(y)?, i8::try_from(num(m)?).ok()?);
            let start = Date::new(y, m, 1).ok()?;
            let end = start.checked_add(jiff::Span::new().months(1)).ok()?;
            (start, end)
        }
        [y, m, d] => {
            let start = Date::new(
                num(y)?,
                i8::try_from(num(m)?).ok()?,
                i8::try_from(num(d)?).ok()?,
            )
            .ok()?;
            (start, start.tomorrow().ok()?)
        }
        _ => return None,
    };
    let at = |d: Date| d.to_zoned(tz.clone()).ok().map(|z| z.timestamp());
    Some((at(start)?, at(end)?))
}

fn has_keyword(k: &str) -> Term {
    let k = k.to_owned();
    cond(move |c| c.has_keyword = Some(k))
}

fn not_keyword(k: &str) -> Term {
    let k = k.to_owned();
    cond(move |c| c.not_keyword = Some(k))
}

fn header(name: &str, value: Option<String>) -> Term {
    let mut h = vec![name.to_owned()];
    h.extend(value);
    cond(move |c| c.header = Some(h))
}

/// One token to a term, or `None` when it has no JMAP equivalent.
fn term(token: &str, tz: &TimeZone) -> Option<Term> {
    let Some((op, value)) = token.split_once(':').filter(|(op, _)| !op.starts_with('"')) else {
        let text = unquote(token);
        return Some(cond(move |c| c.text = Some(text)));
    };
    let op = op.to_ascii_lowercase();
    let value = unquote(value);
    if value.is_empty() {
        return None;
    }
    let v = value.clone();
    Some(match op.as_str() {
        "from" => cond(move |c| c.from = Some(v)),
        // Fastmail's to: also matches Cc and Bcc; JMAP's to is To only.
        "to" => Term::Any(vec![
            cond({
                let v = v.clone();
                move |c| c.to = Some(v)
            }),
            cond({
                let v = v.clone();
                move |c| c.cc = Some(v)
            }),
            cond(move |c| c.bcc = Some(v)),
        ]),
        "cc" => cond(move |c| c.cc = Some(v)),
        "bcc" => cond(move |c| c.bcc = Some(v)),
        "with" => Term::Any(vec![
            cond({
                let v = v.clone();
                move |c| c.from = Some(v)
            }),
            cond({
                let v = v.clone();
                move |c| c.to = Some(v)
            }),
            cond({
                let v = v.clone();
                move |c| c.cc = Some(v)
            }),
            cond(move |c| c.bcc = Some(v)),
        ]),
        "subject" => cond(move |c| c.subject = Some(v)),
        "body" => cond(move |c| c.body = Some(v)),
        "in" | "folder" => match value.as_str() {
            "*" | "all" | "anywhere" => Term::Everywhere,
            _ => Term::InMailbox(value),
        },
        "is" => match value.to_ascii_lowercase().as_str() {
            "unread" | "unseen" => not_keyword(keyword::SEEN),
            "read" | "seen" => has_keyword(keyword::SEEN),
            "pinned" | "flagged" | "starred" => has_keyword(keyword::FLAGGED),
            "unpinned" | "unflagged" => not_keyword(keyword::FLAGGED),
            "answered" | "replied" => has_keyword(keyword::ANSWERED),
            "draft" => has_keyword(keyword::DRAFT),
            "muted" => has_keyword(keyword::MUTED),
            _ => return None,
        },
        "has" => match value.to_ascii_lowercase().as_str() {
            "attachment" | "attachments" => cond(|c| c.has_attachment = Some(true)),
            _ => return None,
        },
        "keyword" | "tag" | "flag" => has_keyword(&value),
        "before" => {
            let (start, _) = date_range(&value, tz)?;
            cond(move |c| c.before = Some(start))
        }
        "after" => {
            let (start, _) = date_range(&value, tz)?;
            cond(move |c| c.after = Some(start))
        }
        "date" | "on" => {
            let (start, end) = date_range(&value, tz)?;
            cond(move |c| {
                c.after = Some(start);
                c.before = Some(end);
            })
        }
        "larger" | "minsize" => {
            let n = parse_size(&value)?;
            cond(move |c| c.min_size = Some(n))
        }
        "smaller" | "maxsize" => {
            let n = parse_size(&value)?;
            cond(move |c| c.max_size = Some(n))
        }
        "header" => match value.split_once(':') {
            Some((name, text)) if !name.trim().is_empty() => {
                header(name.trim(), Some(text.trim().to_owned()))
            }
            _ => header(value.trim(), None),
        },
        "list" => header("List-Id", Some(value)),
        "msgid" => header(
            "Message-ID",
            Some(value.trim_matches(['<', '>']).to_owned()),
        ),
        // Fastmail adds X-Delivered-To before filing; matching it is
        // inferred, not checked against a live account.
        "deliveredto" => header("X-Delivered-To", Some(value)),
        _ => return None,
    })
}

/// Parses Fastmail-style search text.
pub(crate) fn parse(query: &str, tz: &TimeZone) -> Parsed {
    let mut parsed = Parsed::default();
    let mut join_next = false;
    for token in tokens(query) {
        if token == "OR" || token == "|" {
            join_next = !parsed.groups.is_empty();
            continue;
        }
        let (negated, body) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, token.as_str()),
        };
        let Some(t) = term(body, tz) else {
            parsed.unsupported.push(token.clone());
            join_next = false;
            continue;
        };
        let t = if negated { Term::Not(Box::new(t)) } else { t };
        match parsed.groups.last_mut() {
            Some(group) if join_next => group.push(t),
            _ => parsed.groups.push(vec![t]),
        }
        join_next = false;
    }
    parsed
}

/// Resolves `in:` names against the mailbox list.
struct Mailboxes {
    by_key: HashMap<String, Id>,
}

impl Mailboxes {
    fn new(rows: &[MailboxRow]) -> Self {
        let by_id: HashMap<&Id, &MailboxRow> = rows.iter().map(|r| (&r.id, r)).collect();
        let mut by_key = HashMap::new();
        for row in rows {
            if let Some(role) = &row.role {
                by_key.insert(role.as_str().to_ascii_lowercase(), row.id.clone());
            }
            let mut path = vec![row.name.clone()];
            let mut parent = row.parent_id.as_ref();
            let mut depth = 0;
            while let Some(p) = parent.and_then(|p| by_id.get(p)) {
                path.push(p.name.clone());
                parent = p.parent_id.as_ref();
                depth += 1;
                if depth > 32 {
                    break;
                }
            }
            path.reverse();
            by_key
                .entry(row.name.to_lowercase())
                .or_insert_with(|| row.id.clone());
            by_key.insert(path.join("/").to_lowercase(), row.id.clone());
        }
        // Fastmail and many users call the junk mailbox Spam.
        if let Some(junk) = by_key.get("junk").cloned() {
            by_key.entry("spam".into()).or_insert(junk);
        }
        Mailboxes { by_key }
    }

    fn find(&self, name: &str) -> Option<Id> {
        self.by_key.get(&name.to_lowercase()).cloned()
    }
}

fn resolve(
    term: Term,
    mailboxes: &Mailboxes,
    unsupported: &mut Vec<String>,
) -> Option<EmailFilter> {
    match term {
        Term::Cond(c) => Some(EmailFilter::Condition(*c)),
        Term::Everywhere => None,
        Term::InMailbox(name) => match mailboxes.find(&name) {
            Some(id) => Some(EmailFilter::Condition(EmailFilterCondition {
                in_mailbox: Some(id),
                ..Default::default()
            })),
            None => {
                unsupported.push(format!("in:{name}"));
                None
            }
        },
        Term::Not(inner) => resolve(*inner, mailboxes, unsupported).map(|f| EmailFilter::not([f])),
        Term::Any(terms) => {
            let mut fs: Vec<EmailFilter> = terms
                .into_iter()
                .filter_map(|t| resolve(t, mailboxes, unsupported))
                .collect();
            match fs.len() {
                0 => None,
                1 => fs.pop(),
                _ => Some(EmailFilter::or(fs)),
            }
        }
    }
}

impl Parsed {
    /// The JMAP filter, `None` for "everything". Unknown mailbox names are
    /// added to `unsupported`.
    fn filter(
        self,
        rows: &[MailboxRow],
        extra: Option<EmailFilter>,
    ) -> (Option<EmailFilter>, Vec<String>) {
        let mailboxes = Mailboxes::new(rows);
        let mut unsupported = self.unsupported;
        let mut all: Vec<EmailFilter> = Vec::new();
        for group in self.groups {
            if let Some(f) = resolve(Term::Any(group), &mailboxes, &mut unsupported) {
                all.push(f);
            }
        }
        all.extend(extra);
        let filter = match all.len() {
            0 => None,
            1 => all.pop(),
            _ => Some(EmailFilter::and(all)),
        };
        (filter, unsupported)
    }
}

/// The `Email/query` filter a server search for `query` sends, with
/// unknown mailbox names and other terms it cannot send. `mailbox` is the
/// optional mailbox limit of the request. The search itself and
/// [`filter_preview`] both build the filter here, so the preview cannot
/// drift from what is sent.
fn request_filter(
    query: &str,
    mailbox: Option<&Id>,
    tz: &TimeZone,
    rows: &[MailboxRow],
) -> (Option<EmailFilter>, Vec<String>) {
    let extra = mailbox.map(|m| {
        EmailFilter::Condition(EmailFilterCondition {
            in_mailbox: Some(m.clone()),
            ..Default::default()
        })
    });
    parse(query, tz).filter(rows, extra)
}

/// The JMAP filter a server search for `query` would send, as compact
/// JSON, for the UI to show while the user types. `None` when the filter
/// is empty (every message matches), or in the unlikely case it does not
/// serialise. Also returns the terms that are left out of the filter, as
/// typed. Pure: no network and no store access, so the caller passes the
/// mailbox rows and the time zone the engine uses.
pub fn filter_preview(
    query: &str,
    mailbox: Option<&Id>,
    tz: &TimeZone,
    rows: &[MailboxRow],
) -> (Option<String>, Vec<String>) {
    let (filter, unsupported) = request_filter(query, mailbox, tz, rows);
    let json = filter.and_then(|f| serde_json::to_string(&f).ok());
    (json, unsupported)
}

/// How many times a search is retried when its data turns out stale.
const STALE_RETRIES: usize = 3;

pub(crate) async fn run(sh: Arc<Shared>, token: u64, request: SearchRequest) {
    sh.status.update(|s| s.activity.searches += 1);
    let result = run_inner(&sh, &request).await;
    sh.status
        .update(|s| s.activity.searches = s.activity.searches.saturating_sub(1));
    match result {
        Ok(results) => sh.events.emit(SyncEvent::SearchResults { token, results }),
        Err(e) => {
            sh.note(&e);
            sh.events.emit(SyncEvent::SearchFailed {
                token,
                error: e.to_string(),
            });
        }
    }
}

async fn run_inner(sh: &Shared, request: &SearchRequest) -> Result<SearchResults, SyncError> {
    sh.session().await?;
    let account = sh.account()?;
    let rows = sh
        .store
        .call(|ctx| -> Result<Vec<MailboxRow>, SyncError> { Ok(ctx.reader()?.mailboxes()?) })
        .await??;
    let (filter, unsupported) = request_filter(
        &request.query,
        request.mailbox.as_ref(),
        &sh.cfg.time_zone,
        &rows,
    );
    let limit = request
        .limit
        .clamp(1, sh.limits().max_objects_in_get.max(1));
    for _ in 0..STALE_RETRIES {
        let stamp = sh.store.stamp();
        let mut q = QueryArgs::new(account.clone());
        q.filter = filter.clone();
        q.sort = Some(newest_first());
        q.position = Some(i64::try_from(request.position).unwrap_or(i64::MAX));
        q.limit = Some(limit);
        q.calculate_total = Some(true);
        let mut q = EmailQueryArgs::from(q);
        q.collapse_threads = Some(request.collapse_threads);
        let mut req = RequestBuilder::new();
        let qh = req.call::<EmailQuery>(q);
        let gh = req.call_with_refs::<EmailGet>(
            EmailGetArgs::from(
                GetArgs::by_reference(account.clone()).properties(LIST_PROPERTIES.iter().copied()),
            ),
            [("ids", qh.ids())],
        );
        let th = req.call_with_refs::<ThreadGet>(
            GetArgs::by_reference(account.clone()),
            [("ids", gh.thread_ids())],
        );
        let sh_args = SearchSnippetGetArgs {
            account_id: account.clone(),
            filter: filter.clone(),
            email_ids: None,
        };
        let snh = req.call_with_refs::<SearchSnippetGet>(sh_args, [("emailIds", qh.ids())]);
        let resp = sh.send(req).await?;
        let query = resp.get(&qh)?;
        let emails = resp.get(&gh)?;
        let threads = resp.get(&th)?;
        // Snippets are a nicety; a server that cannot make them still
        // answers the search.
        let snippets = match resp.get(&snh) {
            Ok(s) => s.list,
            Err(e) => {
                tracing::debug!(error = %e, "no search snippets");
                Vec::new()
            }
        };
        let mut a = Apply::new(stamp);
        a.emails = emails.list;
        a.threads = threads.list;
        match apply_now(sh, a).await {
            Ok(_) => {}
            Err(SyncError::Stale) => continue,
            Err(e) => return Err(e),
        }
        return Ok(SearchResults {
            ids: query.ids,
            total: query.total,
            position: query.position,
            snippets: snippets
                .into_iter()
                .map(|s| Snippet {
                    email_id: s.email_id,
                    subject: s.subject,
                    preview: s.preview,
                })
                .collect(),
            unsupported,
        });
    }
    Err(SyncError::Stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn rows() -> Vec<MailboxRow> {
        let row = |id: &str, name: &str, role: Option<&str>, parent: Option<&str>| MailboxRow {
            id: id.into(),
            parent_id: parent.map(Id::from),
            name: name.into(),
            role: role.map(|r| serde_json::from_value(json!(r)).unwrap()),
            sort_order: 0,
            total_emails: 0,
            unread_emails: 0,
            total_threads: 0,
            unread_threads: 0,
            local_messages: 0,
            local_conversations: 0,
            is_subscribed: true,
            my_rights: None,
            pending: false,
        };
        vec![
            row("in", "Inbox", Some("inbox"), None),
            row("ju", "Junk", Some("junk"), None),
            row("pr", "Projects", None, None),
            row("gc", "Garden club", None, Some("pr")),
        ]
    }

    fn filter_json(query: &str) -> (Value, Vec<String>) {
        let (f, unsupported) = parse(query, &utc()).filter(&rows(), None);
        (serde_json::to_value(f).unwrap(), unsupported)
    }

    #[test]
    fn words_and_operators() {
        let (f, u) = filter_json("rota from:juniper is:unread");
        assert!(u.is_empty());
        assert_eq!(
            f,
            json!({"operator": "AND", "conditions": [
                {"text": "rota"},
                {"from": "juniper"},
                {"notKeyword": "$seen"}
            ]})
        );
        let (f, _) = filter_json(r#""compost kettle""#);
        assert_eq!(f, json!({"text": "compost kettle"}));
        let (f, _) = filter_json(r#"from:"Juniper Tran""#);
        assert_eq!(f, json!({"from": "Juniper Tran"}));
        assert_eq!(filter_json("").0, Value::Null);
    }

    #[test]
    fn to_covers_cc_and_bcc() {
        let (f, _) = filter_json("to:mira");
        assert_eq!(
            f,
            json!({"operator": "OR", "conditions": [{"to": "mira"}, {"cc": "mira"}, {"bcc": "mira"}]})
        );
    }

    #[test]
    fn negation_and_or() {
        let (f, _) = filter_json("-is:pinned");
        assert_eq!(
            f,
            json!({"operator": "NOT", "conditions": [{"hasKeyword": "$flagged"}]})
        );
        let (f, _) = filter_json("from:ada OR from:bo kettle");
        assert_eq!(
            f,
            json!({"operator": "AND", "conditions": [
                {"operator": "OR", "conditions": [{"from": "ada"}, {"from": "bo"}]},
                {"text": "kettle"}
            ]})
        );
    }

    #[test]
    fn mailboxes_by_role_name_and_path() {
        assert_eq!(filter_json("in:inbox").0, json!({"inMailbox": "in"}));
        assert_eq!(filter_json("in:spam").0, json!({"inMailbox": "ju"}));
        assert_eq!(
            filter_json(r#"in:"Projects/Garden club""#).0,
            json!({"inMailbox": "gc"})
        );
        assert_eq!(
            filter_json(r#"in:"garden club""#).0,
            json!({"inMailbox": "gc"})
        );
        assert_eq!(filter_json("in:*").0, Value::Null);
        let (f, u) = filter_json("in:nowhere kettle");
        assert_eq!(f, json!({"text": "kettle"}));
        assert_eq!(u, ["in:nowhere"]);
    }

    #[test]
    fn dates_and_sizes() {
        let (f, _) = filter_json("after:2026-10-01 before:2026-10");
        assert_eq!(
            f,
            json!({"operator": "AND", "conditions": [
                {"after": "2026-10-01T00:00:00Z"},
                {"before": "2026-10-01T00:00:00Z"}
            ]})
        );
        let (f, _) = filter_json("date:2026-02");
        assert_eq!(
            f,
            json!({"after": "2026-02-01T00:00:00Z", "before": "2026-03-01T00:00:00Z"})
        );
        let (f, _) = filter_json("date:2025");
        assert_eq!(
            f,
            json!({"after": "2025-01-01T00:00:00Z", "before": "2026-01-01T00:00:00Z"})
        );
        let (f, _) = filter_json("larger:2m smaller:500k");
        assert_eq!(
            f,
            json!({"operator": "AND", "conditions": [
                {"minSize": 2_097_152},
                {"maxSize": 512_000}
            ]})
        );
        let (_, u) = filter_json("before:yesterday larger:huge");
        assert_eq!(u, ["before:yesterday", "larger:huge"]);
        assert_eq!(parse_size("10"), Some(10));
        assert_eq!(parse_size("1kb"), Some(1024));
        assert_eq!(parse_size(""), None);
        assert!(date_range("2026-13-01", &utc()).is_none());
    }

    #[test]
    fn headers_and_the_unsupported() {
        assert_eq!(
            filter_json(r#"header:"X-Mailer:mutt""#).0,
            json!({"header": ["X-Mailer", "mutt"]})
        );
        assert_eq!(
            filter_json("header:X-Spam").0,
            json!({"header": ["X-Spam"]})
        );
        assert_eq!(
            filter_json("list:rota.example.org").0,
            json!({"header": ["List-Id", "rota.example.org"]})
        );
        assert_eq!(
            filter_json("msgid:<abc@example.org>").0,
            json!({"header": ["Message-ID", "abc@example.org"]})
        );
        assert_eq!(
            filter_json("has:attachment").0,
            json!({"hasAttachment": true})
        );
        let (f, u) = filter_json("fromin:contacts memo:todo filename:a.pdf is:weird kettle");
        assert_eq!(f, json!({"text": "kettle"}));
        assert_eq!(
            u,
            ["fromin:contacts", "memo:todo", "filename:a.pdf", "is:weird"]
        );
    }

    #[test]
    fn with_expands_to_every_address_field() {
        let (f, _) = filter_json("with:ada");
        let conds = f["conditions"].as_array().unwrap();
        assert_eq!(f["operator"], "OR");
        assert_eq!(conds.len(), 4);
    }

    #[test]
    fn preview_is_the_filter_the_search_sends() {
        // The search mock-up's query; the UI snapshot shows this string.
        let (f, u) = filter_preview(
            "from:larkspur has:attachment deliveredto:shop",
            None,
            &utc(),
            &rows(),
        );
        assert_eq!(
            f.as_deref(),
            Some(
                r#"{"operator":"AND","conditions":[{"from":"larkspur"},{"hasAttachment":true},{"header":["X-Delivered-To","shop"]}]}"#
            )
        );
        assert!(u.is_empty());
        // Mailbox names resolve to ids, as in the request.
        let (f, _) = filter_preview("in:inbox", None, &utc(), &rows());
        assert_eq!(f.as_deref(), Some(r#"{"inMailbox":"in"}"#));
        let (f, _) = filter_preview("kettle", Some(&Id::from("pr")), &utc(), &rows());
        assert_eq!(
            f.as_deref(),
            Some(r#"{"operator":"AND","conditions":[{"text":"kettle"},{"inMailbox":"pr"}]}"#)
        );
        // Nothing to filter on, and terms that are left out.
        assert_eq!(
            filter_preview("", None, &utc(), &rows()),
            (None, Vec::new())
        );
        let (f, u) = filter_preview("in:nowhere memo:x", None, &utc(), &rows());
        assert_eq!(f, None);
        assert_eq!(u, ["memo:x", "in:nowhere"]);
        // Same filter as the search builds, for a spread of queries.
        for q in [
            "rota from:juniper is:unread",
            "to:mira -is:pinned",
            "from:ada OR from:bo kettle after:2026-10-01",
            r#"in:"Projects/Garden club" larger:2m"#,
        ] {
            let (want, _) = request_filter(q, None, &utc(), &rows());
            let (got, _) = filter_preview(q, None, &utc(), &rows());
            let got: Value = serde_json::from_str(&got.unwrap()).unwrap();
            assert_eq!(got, serde_json::to_value(want).unwrap(), "{q}");
        }
    }

    #[test]
    fn extra_mailbox_limit() {
        let (f, _) = parse("kettle", &utc()).filter(
            &rows(),
            Some(EmailFilter::Condition(EmailFilterCondition {
                in_mailbox: Some("in".into()),
                ..Default::default()
            })),
        );
        assert_eq!(
            serde_json::to_value(f).unwrap(),
            json!({"operator": "AND", "conditions": [{"text": "kettle"}, {"inMailbox": "in"}]})
        );
    }
}
