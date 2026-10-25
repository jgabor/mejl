//! `Email/query`, `Email/queryChanges` and `SearchSnippet/get`
//! (RFC 8621 sections 4.4, 4.5 and 5, RFC 8620 sections 5.5 and 5.6).

use std::cmp::Ordering;
use std::collections::BTreeSet;

use jiff::Timestamp;
use serde_json::{Map, Value, json};

use super::{Args, Ctx, MethodError};
use crate::ids::fnv1a;
use crate::model::{Address, Email};
use crate::store::Store;
use crate::text;

/// Sort properties the fake implements. The session advertises exactly
/// these.
pub(crate) const SORT_OPTIONS: &[&str] = &[
    "receivedAt",
    "sentAt",
    "size",
    "from",
    "to",
    "subject",
    "hasKeyword",
    "allInThreadHaveKeyword",
    "someInThreadHaveKeyword",
];

pub(crate) const COLLATIONS: &[&str] = &["i;ascii-casemap", "i;octet"];

#[derive(Debug, Clone)]
enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Vec<Filter>),
    Cond(Box<Cond>),
}

#[derive(Debug, Clone, Default)]
struct Cond {
    in_mailbox: Option<Option<String>>,
    in_mailbox_other_than: Option<BTreeSet<String>>,
    before: Option<Timestamp>,
    after: Option<Timestamp>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    all_in_thread_have_keyword: Option<String>,
    some_in_thread_have_keyword: Option<String>,
    none_in_thread_have_keyword: Option<String>,
    has_keyword: Option<String>,
    not_keyword: Option<String>,
    has_attachment: Option<bool>,
    text: Option<String>,
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    bcc: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    header: Option<(String, Option<String>)>,
}

fn unsupported(why: impl Into<String>) -> MethodError {
    MethodError::with("unsupportedFilter", why)
}

fn parse_filter(ctx: &Ctx<'_>, v: &Value, depth: usize) -> Result<Filter, MethodError> {
    if depth > 64 {
        return Err(unsupported("filter is nested too deeply"));
    }
    let Some(obj) = v.as_object() else {
        return Err(unsupported("filter must be an object"));
    };
    if let Some(op) = obj.get("operator") {
        if obj.keys().any(|k| k != "operator" && k != "conditions") {
            return Err(unsupported(
                "a FilterOperator has only operator and conditions",
            ));
        }
        let Some(conds) = obj.get("conditions").and_then(Value::as_array) else {
            return Err(unsupported("conditions must be an array"));
        };
        let parsed = conds
            .iter()
            .map(|c| parse_filter(ctx, c, depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        return match op.as_str() {
            Some("AND") => Ok(Filter::And(parsed)),
            Some("OR") => Ok(Filter::Or(parsed)),
            Some("NOT") => Ok(Filter::Not(parsed)),
            _ => Err(unsupported("operator must be AND, OR or NOT")),
        };
    }
    let mut c = Cond::default();
    let string = |k: &str, v: &Value| -> Result<String, MethodError> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| unsupported(format!("{k} must be a string")))
    };
    let keyword = |k: &str, v: &Value| -> Result<String, MethodError> {
        v.as_str()
            .and_then(super::email::normalize_keyword)
            .ok_or_else(|| unsupported(format!("{k} must be a valid keyword")))
    };
    let date = |k: &str, v: &Value| -> Result<Timestamp, MethodError> {
        v.as_str()
            .and_then(text::parse_utc_date)
            .ok_or_else(|| unsupported(format!("{k} must be a UTCDate")))
    };
    let size = |k: &str, v: &Value| -> Result<u64, MethodError> {
        v.as_u64()
            .ok_or_else(|| unsupported(format!("{k} must be an UnsignedInt")))
    };
    for (k, v) in obj {
        match k.as_str() {
            "inMailbox" => c.in_mailbox = Some(ctx.resolve_id(&string(k, v)?)),
            "inMailboxOtherThan" => {
                let Some(items) = v.as_array() else {
                    return Err(unsupported("inMailboxOtherThan must be an array"));
                };
                let mut set = BTreeSet::new();
                for i in items {
                    let id = string(k, i)?;
                    if let Some(r) = ctx.resolve_id(&id) {
                        set.insert(r);
                    }
                }
                c.in_mailbox_other_than = Some(set);
            }
            "before" => c.before = Some(date(k, v)?),
            "after" => c.after = Some(date(k, v)?),
            "minSize" => c.min_size = Some(size(k, v)?),
            "maxSize" => c.max_size = Some(size(k, v)?),
            "allInThreadHaveKeyword" => c.all_in_thread_have_keyword = Some(keyword(k, v)?),
            "someInThreadHaveKeyword" => c.some_in_thread_have_keyword = Some(keyword(k, v)?),
            "noneInThreadHaveKeyword" => c.none_in_thread_have_keyword = Some(keyword(k, v)?),
            "hasKeyword" => c.has_keyword = Some(keyword(k, v)?),
            "notKeyword" => c.not_keyword = Some(keyword(k, v)?),
            "hasAttachment" => {
                c.has_attachment = Some(
                    v.as_bool()
                        .ok_or_else(|| unsupported("hasAttachment must be a boolean"))?,
                );
            }
            "text" => c.text = Some(text::fold(&string(k, v)?)),
            "from" => c.from = Some(text::fold(&string(k, v)?)),
            "to" => c.to = Some(text::fold(&string(k, v)?)),
            "cc" => c.cc = Some(text::fold(&string(k, v)?)),
            "bcc" => c.bcc = Some(text::fold(&string(k, v)?)),
            "subject" => c.subject = Some(text::fold(&string(k, v)?)),
            "body" => c.body = Some(text::fold(&string(k, v)?)),
            "header" => {
                let items: Vec<String> = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let len = v.as_array().map_or(0, Vec::len);
                match (items.as_slice(), len) {
                    ([name], 1) if text::is_header_name(name) => {
                        c.header = Some((name.clone(), None))
                    }
                    ([name, value], 2) if text::is_header_name(name) => {
                        c.header = Some((name.clone(), Some(text::fold(value))));
                    }
                    _ => return Err(unsupported("header must be [name] or [name, value]")),
                }
            }
            other => return Err(unsupported(format!("unknown filter condition {other:?}"))),
        }
    }
    Ok(Filter::Cond(Box::new(c)))
}

fn filter_uses_threads(f: &Filter) -> bool {
    match f {
        Filter::And(fs) | Filter::Or(fs) | Filter::Not(fs) => fs.iter().any(filter_uses_threads),
        Filter::Cond(c) => {
            c.all_in_thread_have_keyword.is_some()
                || c.some_in_thread_have_keyword.is_some()
                || c.none_in_thread_have_keyword.is_some()
        }
    }
}

fn addr_matches(list: &Option<Vec<Address>>, needle: &str) -> bool {
    list.iter().flatten().any(|a| {
        text::contains_folded(&a.email, needle)
            || a.name
                .as_deref()
                .is_some_and(|n| text::contains_folded(n, needle))
    })
}

fn thread_keyword(store: &Store, e: &Email, kw: &str) -> (bool, bool) {
    let Some(t) = store.threads.get(&e.thread_id) else {
        return (false, false);
    };
    let mut all = true;
    let mut some = false;
    for id in &t.email_ids {
        if let Some(other) = store.emails.get(id) {
            let has = other.keywords.contains(kw);
            all &= has;
            some |= has;
        }
    }
    (all, some)
}

fn cond_matches(store: &Store, e: &Email, c: &Cond) -> bool {
    if let Some(mb) = &c.in_mailbox {
        match mb {
            Some(id) if e.mailbox_ids.contains(id) => {}
            _ => return false,
        }
    }
    if let Some(excluded) = &c.in_mailbox_other_than
        && !e.mailbox_ids.iter().any(|m| !excluded.contains(m))
    {
        return false;
    }
    if c.before.is_some_and(|b| e.received_at >= b) {
        return false;
    }
    if c.after.is_some_and(|a| e.received_at < a) {
        return false;
    }
    if c.min_size.is_some_and(|m| e.size < m) {
        return false;
    }
    if c.max_size.is_some_and(|m| e.size >= m) {
        return false;
    }
    if let Some(kw) = &c.has_keyword
        && !e.keywords.contains(kw)
    {
        return false;
    }
    if let Some(kw) = &c.not_keyword
        && e.keywords.contains(kw)
    {
        return false;
    }
    for (kw, want) in [
        (&c.all_in_thread_have_keyword, 0),
        (&c.some_in_thread_have_keyword, 1),
        (&c.none_in_thread_have_keyword, 2),
    ] {
        if let Some(kw) = kw {
            let (all, some) = thread_keyword(store, e, kw);
            let ok = match want {
                0 => all,
                1 => some,
                _ => !some,
            };
            if !ok {
                return false;
            }
        }
    }
    if c.has_attachment.is_some_and(|h| e.has_attachment() != h) {
        return false;
    }
    if let Some(n) = &c.from
        && !addr_matches(&e.from, n)
    {
        return false;
    }
    if let Some(n) = &c.to
        && !addr_matches(&e.to, n)
    {
        return false;
    }
    if let Some(n) = &c.cc
        && !addr_matches(&e.cc, n)
    {
        return false;
    }
    if let Some(n) = &c.bcc
        && !addr_matches(&e.bcc, n)
    {
        return false;
    }
    if let Some(n) = &c.subject
        && !e
            .subject
            .as_deref()
            .is_some_and(|s| text::contains_folded(s, n))
    {
        return false;
    }
    if let Some(n) = &c.body
        && !text::contains_folded(&e.plain_text(), n)
    {
        return false;
    }
    if let Some(n) = &c.text {
        let hit = addr_matches(&e.from, n)
            || addr_matches(&e.to, n)
            || addr_matches(&e.cc, n)
            || addr_matches(&e.bcc, n)
            || e.subject
                .as_deref()
                .is_some_and(|s| text::contains_folded(s, n))
            || e.attachments
                .iter()
                .any(|a| text::contains_folded(&a.name, n))
            || text::contains_folded(&e.plain_text(), n);
        if !hit {
            return false;
        }
    }
    if let Some((name, value)) = &c.header {
        let headers = e.headers();
        let mut matching = headers.iter().filter(|(h, _)| h.eq_ignore_ascii_case(name));
        let hit = match value {
            None => matching.next().is_some(),
            Some(v) => matching.any(|(_, hv)| text::contains_folded(hv, v)),
        };
        if !hit {
            return false;
        }
    }
    true
}

fn matches(store: &Store, e: &Email, f: &Filter) -> bool {
    match f {
        Filter::And(fs) => fs.iter().all(|f| matches(store, e, f)),
        Filter::Or(fs) => fs.iter().any(|f| matches(store, e, f)),
        Filter::Not(fs) => !fs.iter().any(|f| matches(store, e, f)),
        Filter::Cond(c) => cond_matches(store, e, c),
    }
}

#[derive(Debug, Clone)]
struct Comparator {
    property: String,
    ascending: bool,
    octet: bool,
    keyword: Option<String>,
}

fn parse_sort(v: Option<&Value>) -> Result<Vec<Comparator>, MethodError> {
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(MethodError::invalid("sort must be an array")),
    };
    let bad = |why: String| MethodError::with("unsupportedSort", why);
    items
        .iter()
        .map(|item| {
            let Some(obj) = item.as_object() else {
                return Err(bad("each comparator must be an object".into()));
            };
            if let Some(k) = obj.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "property" | "isAscending" | "collation" | "keyword"
                )
            }) {
                return Err(bad(format!("unknown comparator property {k:?}")));
            }
            let property = obj
                .get("property")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("comparator needs a property".into()))?;
            if !SORT_OPTIONS.contains(&property) {
                return Err(bad(format!("cannot sort by {property:?}")));
            }
            let ascending = match obj.get("isAscending") {
                None => true,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(MethodError::invalid("isAscending must be a boolean")),
            };
            let octet = match obj.get("collation").and_then(Value::as_str) {
                None | Some("i;ascii-casemap") => false,
                Some("i;octet") => true,
                Some(other) => return Err(bad(format!("unsupported collation {other:?}"))),
            };
            let keyword = match property {
                "hasKeyword" | "allInThreadHaveKeyword" | "someInThreadHaveKeyword" => Some(
                    obj.get("keyword")
                        .and_then(Value::as_str)
                        .and_then(super::email::normalize_keyword)
                        .ok_or_else(|| bad(format!("{property} needs a valid keyword")))?,
                ),
                _ => None,
            };
            Ok(Comparator {
                property: property.to_owned(),
                ascending,
                octet,
                keyword,
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Num(i64),
    Str(String),
}

fn first_addr_key(list: &Option<Vec<Address>>) -> String {
    list.as_ref()
        .and_then(|l| l.first())
        .map(|a| match &a.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => a.email.clone(),
        })
        .unwrap_or_default()
}

fn sort_key(store: &Store, e: &Email, c: &Comparator) -> Key {
    let s = |v: String| {
        if c.octet {
            Key::Str(v)
        } else {
            Key::Str(v.to_ascii_lowercase())
        }
    };
    match c.property.as_str() {
        "receivedAt" => Key::Num(e.received_at.as_second()),
        "sentAt" => Key::Num(e.sent_at.unwrap_or(e.received_at).as_second()),
        "size" => Key::Num(i64::try_from(e.size).unwrap_or(i64::MAX)),
        "from" => s(first_addr_key(&e.from)),
        "to" => s(first_addr_key(&e.to)),
        "subject" => {
            let base = text::base_subject(e.subject.as_deref().unwrap_or(""));
            Key::Str(base)
        }
        "hasKeyword" => Key::Num(i64::from(
            c.keyword.as_ref().is_some_and(|k| e.keywords.contains(k)),
        )),
        "allInThreadHaveKeyword" | "someInThreadHaveKeyword" => {
            let (all, some) = c
                .keyword
                .as_ref()
                .map(|k| thread_keyword(store, e, k))
                .unwrap_or((false, false));
            let v = if c.property == "allInThreadHaveKeyword" {
                all
            } else {
                some
            };
            Key::Num(i64::from(v))
        }
        _ => Key::Num(0),
    }
}

/// A parsed query: what both `/query` and `/queryChanges` need.
struct Query {
    filter: Option<Filter>,
    sort: Vec<Comparator>,
    collapse: bool,
    fingerprint: u64,
}

/// Recursively sorts object keys so equal queries hash equally no matter
/// how the JSON map orders them.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        json!(k),
                        m.get(k).map(canonical).unwrap_or_default()
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

fn parse_query(ctx: &Ctx<'_>, args: &Args<'_>) -> Result<Query, MethodError> {
    let filter = match args.raw("filter") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_filter(ctx, v, 0)?),
    };
    let sort = parse_sort(args.raw("sort"))?;
    let collapse = args.bool_opt("collapseThreads")?.unwrap_or(false);
    let fp_input = format!(
        "{}|{}|{}",
        args.raw("filter").map(canonical).unwrap_or_default(),
        args.raw("sort").map(canonical).unwrap_or_default(),
        collapse
    );
    Ok(Query {
        filter,
        sort,
        collapse,
        fingerprint: fnv1a(fp_input.as_bytes()),
    })
}

/// Runs a query and returns every matching id in order.
fn run(store: &Store, q: &Query) -> Vec<String> {
    let mut rows: Vec<(Vec<Key>, &Email)> = store
        .emails
        .values()
        .filter(|e| q.filter.as_ref().is_none_or(|f| matches(store, e, f)))
        .map(|e| {
            let keys = if q.sort.is_empty() {
                vec![Key::Num(e.received_at.as_second())]
            } else {
                q.sort.iter().map(|c| sort_key(store, e, c)).collect()
            };
            (keys, e)
        })
        .collect();
    let default_sort = [Comparator {
        property: "receivedAt".into(),
        ascending: false,
        octet: false,
        keyword: None,
    }];
    let comparators: &[Comparator] = if q.sort.is_empty() {
        &default_sort
    } else {
        &q.sort
    };
    rows.sort_by(|(ka, ea), (kb, eb)| {
        for (i, c) in comparators.iter().enumerate() {
            let ord = ka.get(i).cmp(&kb.get(i));
            let ord = if c.ascending { ord } else { ord.reverse() };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        ea.id.cmp(&eb.id)
    });
    let mut seen_threads = BTreeSet::new();
    rows.into_iter()
        .filter(|(_, e)| !q.collapse || seen_threads.insert(e.thread_id.clone()))
        .map(|(_, e)| e.id.clone())
        .collect()
}

fn query_state(store: &Store, q: &Query) -> String {
    format!("{}-{:x}", store.email_state(), q.fingerprint)
}

const QUERY_ARGS: &[&str] = &[
    "accountId",
    "filter",
    "sort",
    "position",
    "anchor",
    "anchorOffset",
    "limit",
    "calculateTotal",
    "collapseThreads",
];

pub(super) fn query(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(args, QUERY_ARGS)?;
    args.account(ctx.store)?;
    let q = parse_query(ctx, &args)?;
    let position = args.int_opt("position")?.unwrap_or(0);
    let anchor = args.str_opt("anchor")?;
    let anchor_offset = args.int_opt("anchorOffset")?.unwrap_or(0);
    let limit = args.uint_opt("limit")?;
    let calculate_total = args.bool_opt("calculateTotal")?.unwrap_or(false);
    let ids = run(ctx.store, &q);
    let total = ids.len() as i64;
    let start = match anchor {
        Some(anchor) => {
            let resolved = ctx.resolve_id(anchor);
            let Some(idx) = resolved.and_then(|a| ids.iter().position(|i| *i == a)) else {
                return Err(MethodError::with(
                    "anchorNotFound",
                    format!("{anchor} is not in the results"),
                ));
            };
            (idx as i64 + anchor_offset).max(0)
        }
        None if position < 0 => (total + position).max(0),
        None => position,
    };
    let start_idx = usize::try_from(start).unwrap_or(usize::MAX).min(ids.len());
    let end_idx = match limit {
        Some(l) => start_idx
            .saturating_add(usize::try_from(l).unwrap_or(usize::MAX))
            .min(ids.len()),
        None => ids.len(),
    };
    let page: &[String] = ids.get(start_idx..end_idx).unwrap_or(&[]);
    let mut out = json!({
        "accountId": ctx.store.account_id,
        "queryState": query_state(ctx.store, &q),
        "canCalculateChanges": true,
        "position": start_idx,
        "ids": page,
    });
    if calculate_total && let Some(m) = out.as_object_mut() {
        m.insert("total".to_owned(), json!(total));
    }
    Ok(out)
}

pub(super) fn query_changes(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    let args = Args::new(
        args,
        &[
            "accountId",
            "filter",
            "sort",
            "sinceQueryState",
            "maxChanges",
            "upToId",
            "calculateTotal",
            "collapseThreads",
        ],
    )?;
    args.account(ctx.store)?;
    let q = parse_query(ctx, &args)?;
    let since = args.str_req("sinceQueryState")?;
    let max_changes = args.uint_opt("maxChanges")?;
    if max_changes == Some(0) {
        return Err(MethodError::invalid("maxChanges must be greater than 0"));
    }
    // `upToId` is an optional optimisation (RFC 8620 section 5.6) and does
    // not apply when the query uses mutable properties, as mejl's mailbox
    // and keyword queries do. It is checked and then ignored, so clients see
    // `added` indexes beyond their cached window, as a real server may send.
    args.str_opt("upToId")?;
    let calculate_total = args.bool_opt("calculateTotal")?.unwrap_or(false);
    let cannot = || {
        MethodError::with(
            "cannotCalculateChanges",
            format!("query state {since:?} is unknown, too old or for another query"),
        )
    };
    let (state_part, fp_part) = since.split_once('-').ok_or_else(cannot)?;
    if fp_part != format!("{:x}", q.fingerprint) {
        return Err(cannot());
    }
    let since_n = ctx
        .store
        .logs
        .email
        .parse_since(state_part)
        .ok_or_else(cannot)?;
    let touched = ctx
        .store
        .logs
        .email
        .touched_since(since_n)
        .ok_or_else(cannot)?;

    // Only touched emails can change membership or position: the
    // properties that can change (keywords, mailboxes) belong to them. When threads matter, every email in a touched thread can
    // move too, because the thread's representative or keywords changed.
    let mut affected: BTreeSet<String> = touched.ids.clone();
    if q.collapse
        || q.filter.as_ref().is_some_and(filter_uses_threads)
        || q.sort.iter().any(|c| c.property.contains("InThread"))
    {
        for t in &touched.threads {
            if let Some(thread) = ctx.store.threads.get(t) {
                affected.extend(thread.email_ids.iter().cloned());
            }
        }
    }
    let ids = run(ctx.store, &q);
    // Ids created after the old state were never in the old results.
    let removed: Vec<String> = affected
        .iter()
        .filter(|id| !touched.created.contains(*id))
        .cloned()
        .collect();
    let added: Vec<Value> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| affected.contains(*id))
        .map(|(i, id)| json!({"id": id, "index": i}))
        .collect();
    if let Some(max) = max_changes
        && (removed.len() + added.len()) as u64 > max
    {
        return Err(MethodError::with(
            "tooManyChanges",
            format!(
                "{} changes, maxChanges is {max}",
                removed.len() + added.len()
            ),
        ));
    }
    let mut out = json!({
        "accountId": ctx.store.account_id,
        "oldQueryState": since,
        "newQueryState": query_state(ctx.store, &q),
        "removed": removed,
        "added": added,
    });
    if calculate_total && let Some(m) = out.as_object_mut() {
        m.insert("total".to_owned(), json!(ids.len()));
    }
    Ok(out)
}

/// Search terms from `text`, `subject` and `body` conditions, outside NOT.
fn terms(f: &Filter, subject: &mut Vec<String>, body: &mut Vec<String>) {
    match f {
        Filter::And(fs) | Filter::Or(fs) => fs.iter().for_each(|f| terms(f, subject, body)),
        Filter::Not(_) => {}
        Filter::Cond(c) => {
            for t in [&c.text, &c.subject].into_iter().flatten() {
                subject.extend(t.split_whitespace().map(str::to_owned));
            }
            for t in [&c.text, &c.body].into_iter().flatten() {
                body.extend(t.split_whitespace().map(str::to_owned));
            }
        }
    }
}

pub(super) fn snippets(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(args, &["accountId", "filter", "emailIds"])?;
    args.account(ctx.store)?;
    let filter = match args.raw("filter") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_filter(ctx, v, 0)?),
    };
    let Some(ids) = args.ids_opt("emailIds")? else {
        return Err(MethodError::invalid("emailIds is required"));
    };
    let limit = ctx.store.limits.max_objects_in_get;
    if ids.len() as u64 > limit {
        return Err(MethodError::with(
            "requestTooLarge",
            format!("{} ids, maxObjectsInGet is {limit}", ids.len()),
        ));
    }
    let mut subject_terms = Vec::new();
    let mut body_terms = Vec::new();
    if let Some(f) = &filter {
        terms(f, &mut subject_terms, &mut body_terms);
    }
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for given in ids {
        let Some(e) = ctx
            .resolve_id(&given)
            .and_then(|id| ctx.store.emails.get(&id))
        else {
            not_found.push(given);
            continue;
        };
        let subject = e
            .subject
            .as_deref()
            .and_then(|s| text::highlight(s, &subject_terms));
        let preview = text::highlight_window(&e.plain_text(), &body_terms);
        list.push(json!({"emailId": e.id, "subject": subject, "preview": preview}));
    }
    Ok(json!({
        "accountId": ctx.store.account_id,
        "list": list,
        "notFound": if not_found.is_empty() { Value::Null } else { json!(not_found) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_ignores_key_order() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"y":[1,{"q":2,"p":3}],"x":null}}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"a":{"x":null,"y":[1,{"p":3,"q":2}]},"b":1}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        let c: Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
        assert_ne!(canonical(&a), canonical(&c));
    }

    #[test]
    fn sort_parsing() {
        assert!(
            parse_sort(Some(
                &json!([{"property": "receivedAt", "isAscending": false}])
            ))
            .is_ok()
        );
        assert!(
            parse_sort(Some(
                &json!([{"property": "hasKeyword", "keyword": "$flagged"}])
            ))
            .is_ok()
        );
        for bad in [
            json!([{"property": "nope"}]),
            json!([{"property": "hasKeyword"}]),
            json!([{"property": "from", "collation": "i;klingon"}]),
            json!([{"property": "from", "extra": 1}]),
            json!(["receivedAt"]),
        ] {
            let e = parse_sort(Some(&bad)).unwrap_err();
            assert_eq!(e.kind, "unsupportedSort", "{bad}");
        }
        assert_eq!(
            parse_sort(Some(&json!([{"property": "from", "isAscending": "yes"}])))
                .unwrap_err()
                .kind,
            "invalidArguments"
        );
    }
}
