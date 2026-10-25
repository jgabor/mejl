//! `Email/get`, `Email/changes` and `Email/set` (RFC 8621 section 4).

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::pointer::unescape;
use super::{
    Args, Ctx, MethodError, SetError, SetResult, changes_from_log, check_set, get_ids, set_args,
};
use crate::model::{Address, Attachment, Body, Email, Node, Part, PartKind, addresses_json};
use crate::store::NewEmail;
use crate::text;

const PROPS: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "sentAt",
    "hasAttachment",
    "preview",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
    "bodyStructure",
    "headers",
];

/// RFC 8621's default `properties` for `Email/get`.
const DEFAULT_PROPS: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "sentAt",
    "hasAttachment",
    "preview",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

const BODY_PROPS: &[&str] = &[
    "partId",
    "blobId",
    "size",
    "headers",
    "name",
    "type",
    "charset",
    "disposition",
    "cid",
    "language",
    "location",
    "subParts",
];

const DEFAULT_BODY_PROPS: &[&str] = &[
    "partId",
    "blobId",
    "size",
    "name",
    "type",
    "charset",
    "disposition",
    "cid",
    "language",
    "location",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Raw,
    Text,
    Addresses,
    GroupedAddresses,
    MessageIds,
    Date,
    Urls,
}

#[derive(Debug, Clone)]
struct HeaderProp {
    property: String,
    name: String,
    form: Form,
    all: bool,
}

/// Headers with a fixed parsed form in RFC 8621 section 4.1.2. Any other
/// header may be fetched in every form.
fn fixed_form(name: &str) -> Option<Form> {
    let lower = name.to_ascii_lowercase();
    let list = |names: &[&str]| names.contains(&lower.as_str());
    if list(&["subject", "comments", "keywords", "list-id"]) {
        Some(Form::Text)
    } else if list(&[
        "from",
        "sender",
        "reply-to",
        "to",
        "cc",
        "bcc",
        "resent-from",
        "resent-sender",
        "resent-reply-to",
        "resent-to",
        "resent-cc",
        "resent-bcc",
    ]) {
        Some(Form::Addresses)
    } else if list(&[
        "message-id",
        "in-reply-to",
        "references",
        "resent-message-id",
    ]) {
        Some(Form::MessageIds)
    } else if list(&["date", "resent-date"]) {
        Some(Form::Date)
    } else if list(&[
        "list-help",
        "list-unsubscribe",
        "list-subscribe",
        "list-post",
        "list-owner",
        "list-archive",
    ]) {
        Some(Form::Urls)
    } else {
        None
    }
}

/// Parses `header:{name}[:as{Form}][:all]`.
fn parse_header_prop(property: &str) -> Result<HeaderProp, MethodError> {
    let bad = || MethodError::invalid(format!("bad header property {property:?}"));
    let rest = property.strip_prefix("header:").ok_or_else(bad)?;
    let mut parts = rest.split(':');
    let name = parts
        .next()
        .filter(|n| text::is_header_name(n))
        .ok_or_else(bad)?;
    let mut form = Form::Raw;
    let mut all = false;
    let mut seen_form = false;
    for part in parts {
        match part {
            "all" if !all => all = true,
            _ if !seen_form && !all => {
                form = match part {
                    "asRaw" => Form::Raw,
                    "asText" => Form::Text,
                    "asAddresses" => Form::Addresses,
                    "asGroupedAddresses" => Form::GroupedAddresses,
                    "asMessageIds" => Form::MessageIds,
                    "asDate" => Form::Date,
                    "asURLs" => Form::Urls,
                    _ => return Err(bad()),
                };
                seen_form = true;
            }
            _ => return Err(bad()),
        }
    }
    if let Some(fixed) = fixed_form(name) {
        let ok = form == Form::Raw
            || form == fixed
            || (fixed == Form::Addresses && form == Form::GroupedAddresses);
        if !ok {
            return Err(MethodError::invalid(format!(
                "{property:?}: this form is not allowed for {name}"
            )));
        }
    }
    Ok(HeaderProp {
        property: property.to_owned(),
        name: name.to_owned(),
        form,
        all,
    })
}

fn header_value(headers: &[(String, String)], hp: &HeaderProp) -> Value {
    let convert = |raw: &str| -> Value {
        match hp.form {
            Form::Raw => json!(format!(" {raw}")),
            Form::Text => json!(raw.trim()),
            Form::Addresses => Value::Array(
                text::parse_address_list(raw)
                    .iter()
                    .map(Address::to_json)
                    .collect(),
            ),
            Form::GroupedAddresses => json!([{
                "name": null,
                "addresses": text::parse_address_list(raw)
                    .iter()
                    .map(Address::to_json)
                    .collect::<Vec<_>>(),
            }]),
            Form::MessageIds => json!(text::parse_message_ids(raw)),
            Form::Date => json!(text::parse_rfc5322_date(raw).map(text::utc_date)),
            Form::Urls => json!(text::parse_urls(raw)),
        }
    };
    let matching: Vec<Value> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(&hp.name))
        .map(|(_, v)| convert(v))
        .collect();
    if hp.all {
        Value::Array(matching)
    } else {
        matching.into_iter().last().unwrap_or(Value::Null)
    }
}

struct BodyOpts {
    props: Vec<String>,
    fetch_text: bool,
    fetch_html: bool,
    fetch_all: bool,
    max_bytes: Option<usize>,
}

fn part_json(p: &Part, props: &[String]) -> Value {
    let mut out = Map::new();
    for prop in props {
        let v = match prop.as_str() {
            "partId" => json!(p.part_id),
            "blobId" => json!(p.blob_id),
            "size" => json!(p.size),
            "name" => json!(p.name),
            "type" => json!(p.mime),
            "charset" => json!(p.charset),
            "disposition" => json!(p.disposition),
            "cid" => json!(p.cid),
            "language" | "location" => Value::Null,
            "subParts" => continue,
            "headers" => {
                let mut ct = match &p.charset {
                    Some(cs) => format!(" {}; charset={cs}", p.mime),
                    None => format!(" {}", p.mime),
                };
                if let Some(name) = &p.name {
                    ct.push_str(&format!("; name=\"{name}\""));
                }
                let mut hs = vec![json!({"name": "Content-Type", "value": ct})];
                if let Some(d) = &p.disposition {
                    let name = p.name.as_deref().unwrap_or("file");
                    hs.push(json!({
                        "name": "Content-Disposition",
                        "value": format!(" {d}; filename=\"{name}\""),
                    }));
                }
                if matches!(p.kind, PartKind::Attachment(_)) {
                    hs.push(json!({"name": "Content-Transfer-Encoding", "value": " base64"}));
                }
                Value::Array(hs)
            }
            _ => continue,
        };
        out.insert(prop.clone(), v);
    }
    Value::Object(out)
}

fn node_json(node: &Node, props: &[String]) -> Value {
    match node {
        Node::Leaf(p) => part_json(p, props),
        Node::Multi { mime, children } => {
            let mut out = Map::new();
            for prop in props {
                let v = match prop.as_str() {
                    "type" => json!(mime),
                    "size" => json!(0),
                    "headers" => json!([{"name": "Content-Type", "value": format!(" {mime}")}]),
                    "subParts" => continue,
                    _ => Value::Null,
                };
                out.insert(prop.clone(), v);
            }
            out.insert(
                "subParts".to_owned(),
                Value::Array(children.iter().map(|c| node_json(c, props)).collect()),
            );
            Value::Object(out)
        }
    }
}

fn body_value(email: &Email, part: &Part, max: Option<usize>) -> Option<Value> {
    let content = email.part_text(part.kind)?;
    let (value, truncated) = match max {
        Some(max) => text::truncate_utf8(&content, max, part.kind == PartKind::Html),
        None => (content.as_ref(), false),
    };
    Some(json!({
        "value": value,
        "isEncodingProblem": false,
        "isTruncated": truncated,
    }))
}

fn email_json(
    email: &Email,
    props: &Option<BTreeSet<String>>,
    headers: &[HeaderProp],
    body: &BodyOpts,
) -> Value {
    let wants = |p: &str| match props {
        Some(set) => set.contains(p),
        None => DEFAULT_PROPS.contains(&p),
    };
    let mut out = Map::new();
    let mut put = |k: &str, v: Value| {
        out.insert(k.to_owned(), v);
    };
    put("id", json!(email.id));
    if wants("blobId") {
        put("blobId", json!(email.blob_id()));
    }
    if wants("threadId") {
        put("threadId", json!(email.thread_id));
    }
    if wants("mailboxIds") {
        let m: Map<String, Value> = email
            .mailbox_ids
            .iter()
            .map(|id| (id.clone(), json!(true)))
            .collect();
        put("mailboxIds", Value::Object(m));
    }
    if wants("keywords") {
        let m: Map<String, Value> = email
            .keywords
            .iter()
            .map(|k| (k.clone(), json!(true)))
            .collect();
        put("keywords", Value::Object(m));
    }
    if wants("size") {
        put("size", json!(email.size));
    }
    if wants("receivedAt") {
        put("receivedAt", json!(text::utc_date(email.received_at)));
    }
    for (key, ids) in [
        ("messageId", &email.message_id),
        ("inReplyTo", &email.in_reply_to),
        ("references", &email.references),
    ] {
        if wants(key) {
            put(key, json!(ids));
        }
    }
    for (key, list) in [
        ("sender", &email.sender),
        ("from", &email.from),
        ("to", &email.to),
        ("cc", &email.cc),
        ("bcc", &email.bcc),
        ("replyTo", &email.reply_to),
    ] {
        if wants(key) {
            put(key, addresses_json(list));
        }
    }
    if wants("subject") {
        put("subject", json!(email.subject));
    }
    if wants("sentAt") {
        put("sentAt", json!(email.sent_at.map(text::utc_date)));
    }
    if wants("hasAttachment") {
        put("hasAttachment", json!(email.has_attachment()));
    }
    if wants("preview") {
        put("preview", json!(email.preview()));
    }
    let needs_parts = [
        "bodyValues",
        "textBody",
        "htmlBody",
        "attachments",
        "bodyStructure",
    ]
    .iter()
    .any(|p| wants(p));
    if needs_parts {
        let parts = email.parts();
        let list =
            |ps: &[Part]| Value::Array(ps.iter().map(|p| part_json(p, &body.props)).collect());
        if wants("textBody") {
            put("textBody", list(&parts.text_body));
        }
        if wants("htmlBody") {
            put("htmlBody", list(&parts.html_body));
        }
        if wants("attachments") {
            put("attachments", list(&parts.attachments));
        }
        if wants("bodyStructure") {
            put("bodyStructure", node_json(&parts.structure, &body.props));
        }
        if wants("bodyValues") {
            let mut values = Map::new();
            let mut add = |p: &Part| {
                if let Some(v) = body_value(email, p, body.max_bytes) {
                    values.insert(p.part_id.clone(), v);
                }
            };
            if body.fetch_text || body.fetch_all {
                parts.text_body.iter().for_each(&mut add);
            }
            if body.fetch_html || body.fetch_all {
                parts.html_body.iter().for_each(&mut add);
            }
            put("bodyValues", Value::Object(values));
        }
    }
    if wants("headers") || !headers.is_empty() {
        let all = email.headers();
        if wants("headers") {
            put(
                "headers",
                Value::Array(
                    all.iter()
                        .map(|(n, v)| json!({"name": n, "value": format!(" {v}")}))
                        .collect(),
                ),
            );
        }
        for hp in headers {
            put(&hp.property, header_value(&all, hp));
        }
    }
    Value::Object(out)
}

pub(super) fn get(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(
        args,
        &[
            "accountId",
            "ids",
            "properties",
            "bodyProperties",
            "fetchTextBodyValues",
            "fetchHTMLBodyValues",
            "fetchAllBodyValues",
            "maxBodyValueBytes",
        ],
    )?;
    args.account(ctx.store)?;
    let mut props: Option<BTreeSet<String>> = None;
    let mut header_props = Vec::new();
    if let Some(list) = args.ids_opt("properties")? {
        let mut set = BTreeSet::new();
        for p in list {
            if p.starts_with("header:") {
                header_props.push(parse_header_prop(&p)?);
            } else if PROPS.contains(&p.as_str()) {
                set.insert(p);
            } else {
                return Err(MethodError::invalid(format!("unknown property {p:?}")));
            }
        }
        set.insert("id".to_owned());
        props = Some(set);
    }
    let body_props = match args.ids_opt("bodyProperties")? {
        Some(list) => {
            if let Some(bad) = list.iter().find(|p| !BODY_PROPS.contains(&p.as_str())) {
                return Err(MethodError::invalid(format!(
                    "unknown body property {bad:?}"
                )));
            }
            list
        }
        None => DEFAULT_BODY_PROPS.iter().map(|s| (*s).to_owned()).collect(),
    };
    let max_bytes = match args.uint_opt("maxBodyValueBytes")? {
        Some(0) => {
            return Err(MethodError::invalid(
                "maxBodyValueBytes must be greater than 0",
            ));
        }
        Some(n) => Some(usize::try_from(n).unwrap_or(usize::MAX)),
        None => None,
    };
    let body = BodyOpts {
        props: body_props,
        fetch_text: args.bool_opt("fetchTextBodyValues")?.unwrap_or(false),
        fetch_html: args.bool_opt("fetchHTMLBodyValues")?.unwrap_or(false),
        fetch_all: args.bool_opt("fetchAllBodyValues")?.unwrap_or(false),
        max_bytes,
    };
    let ids = get_ids(ctx, &args, || ctx.store.emails.keys().cloned().collect())?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for (given, resolved) in ids {
        match resolved.and_then(|id| ctx.store.emails.get(&id)) {
            Some(e) => list.push(email_json(e, &props, &header_props, &body)),
            None => not_found.push(given),
        }
    }
    Ok(json!({
        "accountId": ctx.store.account_id,
        "state": ctx.store.logs.email.state(),
        "list": list,
        "notFound": not_found,
    }))
}

pub(super) fn changes(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.email, |_, _| {})
}

/// Validates and lowercases a keyword (RFC 8621 section 4.1.1).
pub(crate) fn normalize_keyword(k: &str) -> Option<String> {
    let ok = !k.is_empty()
        && k.len() <= 255
        && k.bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"(){]%*\"\\".contains(&b));
    ok.then(|| k.to_ascii_lowercase())
}

fn bool_map(
    ctx: &Ctx<'_>,
    v: &Value,
    prop: &str,
    key_fn: impl Fn(&Ctx<'_>, &str) -> Option<String>,
) -> Result<BTreeSet<String>, SetError> {
    let Some(obj) = v.as_object() else {
        return Err(SetError::props(
            format!("{prop} must be an object"),
            &[prop],
        ));
    };
    let mut out = BTreeSet::new();
    for (k, val) in obj {
        if val != &Value::Bool(true) {
            return Err(SetError::props(
                format!("{prop} values must be true"),
                &[prop],
            ));
        }
        match key_fn(ctx, k) {
            Some(key) => {
                out.insert(key);
            }
            None => {
                return Err(SetError::props(
                    format!("{prop} has an invalid key {k:?}"),
                    &[prop],
                ));
            }
        }
    }
    Ok(out)
}

fn mailbox_key(ctx: &Ctx<'_>, k: &str) -> Option<String> {
    ctx.resolve_id(k)
        .filter(|id| ctx.store.mailboxes.contains_key(id))
}

fn keyword_key(_: &Ctx<'_>, k: &str) -> Option<String> {
    normalize_keyword(k)
}

fn address_list(v: &Value, prop: &str) -> Result<Option<Vec<Address>>, SetError> {
    match v {
        Value::Null => Ok(None),
        Value::Array(items) => items
            .iter()
            .map(|i| {
                Address::from_json(i)
                    .ok_or_else(|| SetError::props(format!("{prop} has a bad address"), &[prop]))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Err(SetError::props(
            format!("{prop} must be an array or null"),
            &[prop],
        )),
    }
}

fn string_list(v: &Value, prop: &str) -> Result<Option<Vec<String>>, SetError> {
    match v {
        Value::Null => Ok(None),
        Value::Array(items) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .filter(|s| !s.is_empty() && !s.contains(['<', '>', ' ']))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        SetError::props(
                            format!("{prop} must hold message ids without angle brackets"),
                            &[prop],
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Err(SetError::props(
            format!("{prop} must be an array or null"),
            &[prop],
        )),
    }
}

/// A body part reference in a create: either a value in `bodyValues` or an
/// uploaded blob.
struct PartSpec {
    part_id: Option<String>,
    blob_id: Option<String>,
    mime: Option<String>,
    name: Option<String>,
    disposition: Option<String>,
    cid: Option<String>,
}

fn part_spec(v: &Value, prop: &str) -> Result<PartSpec, SetError> {
    let Some(obj) = v.as_object() else {
        return Err(SetError::props(
            format!("{prop} parts must be objects"),
            &[prop],
        ));
    };
    let s = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
    for k in obj.keys() {
        let known = [
            "partId",
            "blobId",
            "type",
            "name",
            "disposition",
            "cid",
            "charset",
            "language",
            "location",
            "size",
            "subParts",
            "headers",
        ];
        if !known.contains(&k.as_str()) && !k.starts_with("header:") {
            return Err(SetError::props(
                format!("unknown body part property {k:?}"),
                &[prop],
            ));
        }
    }
    Ok(PartSpec {
        part_id: s("partId"),
        blob_id: s("blobId"),
        mime: s("type").map(|t| t.to_ascii_lowercase()),
        name: s("name"),
        disposition: s("disposition"),
        cid: s("cid"),
    })
}

struct Bodies {
    text: Option<String>,
    html: Option<String>,
    attachments: Vec<Attachment>,
}

fn resolve_attachment(ctx: &Ctx<'_>, spec: &PartSpec) -> Result<Attachment, SetError> {
    let Some(blob) = &spec.blob_id else {
        return Err(SetError::props(
            "attachments need a blobId",
            &["attachments"],
        ));
    };
    let (size, mime) = match ctx.store.uploads.get(blob) {
        Some(up) => (up.data.len() as u64, up.mime.clone()),
        None => match crate::http::blob_size(ctx.store, blob) {
            Some((size, mime)) => (size, mime),
            None => {
                let mut e = SetError::new("blobNotFound", format!("blob {blob} does not exist"));
                e.extra.insert("notFound".to_owned(), json!([blob]));
                return Err(e);
            }
        },
    };
    Ok(Attachment {
        name: spec.name.clone().unwrap_or_else(|| "attachment".to_owned()),
        mime: spec.mime.clone().unwrap_or(mime),
        size,
        inline: spec.disposition.as_deref() == Some("inline"),
        cid: spec.cid.clone(),
        upload: Some(blob.clone()),
    })
}

fn parse_bodies(ctx: &Ctx<'_>, obj: &Map<String, Value>) -> Result<Bodies, SetError> {
    let mut values: Map<String, Value> = Map::new();
    if let Some(v) = obj.get("bodyValues") {
        let Some(m) = v.as_object() else {
            return Err(SetError::props(
                "bodyValues must be an object",
                &["bodyValues"],
            ));
        };
        for (pid, bv) in m {
            let Some(bv) = bv.as_object() else {
                return Err(SetError::props(
                    "bodyValues entries must be objects",
                    &["bodyValues"],
                ));
            };
            if bv.get("isTruncated") == Some(&Value::Bool(true))
                || bv.get("isEncodingProblem") == Some(&Value::Bool(true))
            {
                return Err(SetError::props(
                    "isTruncated and isEncodingProblem must be false",
                    &["bodyValues"],
                ));
            }
            let Some(value) = bv.get("value").and_then(Value::as_str) else {
                return Err(SetError::props(
                    "bodyValues entries need a string value",
                    &["bodyValues"],
                ));
            };
            values.insert(pid.clone(), json!(value));
        }
    }
    let take_value = |spec: &PartSpec, prop: &str| -> Result<String, SetError> {
        let Some(pid) = &spec.part_id else {
            return Err(SetError::props(format!("{prop} needs a partId"), &[prop]));
        };
        values
            .get(pid)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                SetError::props(
                    format!("{prop} partId {pid:?} is not in bodyValues"),
                    &[prop],
                )
            })
    };
    let mut bodies = Bodies {
        text: None,
        html: None,
        attachments: Vec::new(),
    };
    if let Some(structure) = obj.get("bodyStructure") {
        if obj.contains_key("textBody")
            || obj.contains_key("htmlBody")
            || obj.contains_key("attachments")
        {
            return Err(SetError::props(
                "bodyStructure cannot be combined with textBody, htmlBody or attachments",
                &["bodyStructure"],
            ));
        }
        let mut stack = vec![structure];
        let mut depth = 0;
        while let Some(node) = stack.pop() {
            depth += 1;
            if depth > 256 {
                return Err(SetError::props(
                    "bodyStructure is too deep",
                    &["bodyStructure"],
                ));
            }
            if let Some(subs) = node.get("subParts") {
                let Some(subs) = subs.as_array() else {
                    return Err(SetError::props(
                        "subParts must be an array",
                        &["bodyStructure"],
                    ));
                };
                stack.extend(subs.iter().rev());
                continue;
            }
            let spec = part_spec(node, "bodyStructure")?;
            match (spec.mime.as_deref(), &spec.part_id) {
                (Some("text/plain"), Some(_)) if bodies.text.is_none() => {
                    bodies.text = Some(take_value(&spec, "bodyStructure")?);
                }
                (Some("text/html"), Some(_)) if bodies.html.is_none() => {
                    bodies.html = Some(take_value(&spec, "bodyStructure")?);
                }
                _ => bodies.attachments.push(resolve_attachment(ctx, &spec)?),
            }
        }
        return Ok(bodies);
    }
    for (prop, mime) in [("textBody", "text/plain"), ("htmlBody", "text/html")] {
        let Some(v) = obj.get(prop) else { continue };
        let Some(items) = v.as_array() else {
            return Err(SetError::props(format!("{prop} must be an array"), &[prop]));
        };
        if items.len() > 1 {
            return Err(SetError::props(
                format!("{prop} may hold at most one part"),
                &[prop],
            ));
        }
        if let Some(item) = items.first() {
            let spec = part_spec(item, prop)?;
            if spec.mime.as_deref().is_some_and(|m| m != mime) {
                return Err(SetError::props(
                    format!("{prop} part must be {mime}"),
                    &[prop],
                ));
            }
            let value = take_value(&spec, prop)?;
            if mime == "text/plain" {
                bodies.text = Some(value);
            } else {
                bodies.html = Some(value);
            }
        }
    }
    if let Some(v) = obj.get("attachments") {
        let Some(items) = v.as_array() else {
            return Err(SetError::props(
                "attachments must be an array",
                &["attachments"],
            ));
        };
        for item in items {
            let spec = part_spec(item, "attachments")?;
            bodies.attachments.push(resolve_attachment(ctx, &spec)?);
        }
    }
    Ok(bodies)
}

fn parse_create(ctx: &Ctx<'_>, value: &Value) -> Result<NewEmail, SetError> {
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidProperties",
            "create value must be an object",
        ));
    };
    let mut new = NewEmail::default();
    for (key, v) in obj {
        match key.as_str() {
            "mailboxIds" => new.mailbox_ids = bool_map(ctx, v, "mailboxIds", mailbox_key)?,
            "keywords" => new.keywords = bool_map(ctx, v, "keywords", keyword_key)?,
            "receivedAt" => {
                new.received_at =
                    Some(v.as_str().and_then(text::parse_utc_date).ok_or_else(|| {
                        SetError::props("receivedAt must be a UTCDate", &["receivedAt"])
                    })?);
            }
            "sentAt" => {
                new.sent_at = match v {
                    Value::Null => None,
                    Value::String(s) => Some(
                        s.parse::<jiff::Timestamp>()
                            .map_err(|_| SetError::props("sentAt must be a Date", &["sentAt"]))?,
                    ),
                    _ => return Err(SetError::props("sentAt must be a Date", &["sentAt"])),
                };
            }
            "messageId" => {
                let ids = string_list(v, "messageId")?;
                if ids.as_ref().is_some_and(|i| i.len() > 1) {
                    return Err(SetError::props("messageId may hold one id", &["messageId"]));
                }
                new.message_id = ids.and_then(|i| i.into_iter().next());
            }
            "inReplyTo" => new.in_reply_to = string_list(v, "inReplyTo")?,
            "references" => new.references = string_list(v, "references")?,
            "sender" => new.sender = address_list(v, "sender")?,
            "from" => new.from = address_list(v, "from")?,
            "to" => new.to = address_list(v, "to")?,
            "cc" => new.cc = address_list(v, "cc")?,
            "bcc" => new.bcc = address_list(v, "bcc")?,
            "replyTo" => new.reply_to = address_list(v, "replyTo")?,
            "subject" => {
                new.subject = match v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    _ => return Err(SetError::props("subject must be a string", &["subject"])),
                };
            }
            "bodyValues" | "textBody" | "htmlBody" | "attachments" | "bodyStructure" => {}
            "headers" => {
                let Some(items) = v.as_array() else {
                    return Err(SetError::props("headers must be an array", &["headers"]));
                };
                for h in items {
                    let (Some(n), Some(val)) = (
                        h.get("name").and_then(Value::as_str),
                        h.get("value").and_then(Value::as_str),
                    ) else {
                        return Err(SetError::props(
                            "headers entries need name and value",
                            &["headers"],
                        ));
                    };
                    if !text::is_header_name(n) {
                        return Err(SetError::props("bad header name", &["headers"]));
                    }
                    new.extra_headers
                        .push((n.to_owned(), val.trim_start().to_owned()));
                }
            }
            other if other.starts_with("header:") => {
                let mut parts = other.trim_start_matches("header:").split(':');
                let name = parts.next().unwrap_or("");
                let form = parts.next();
                if !text::is_header_name(name) || parts.next().is_some() {
                    return Err(SetError::props("bad header property", &[other]));
                }
                let raw = match (form, v) {
                    (None | Some("asRaw"), Value::String(s)) => s.trim_start().to_owned(),
                    (Some("asText"), Value::String(s)) => s.clone(),
                    _ => {
                        return Err(SetError::props(
                            "only raw and text header forms can be set here",
                            &[other],
                        ));
                    }
                };
                if raw.contains(['\r', '\n']) {
                    return Err(SetError::props(
                        "header values may not contain line breaks",
                        &[other],
                    ));
                }
                new.extra_headers.push((name.to_owned(), raw));
            }
            "id" | "blobId" | "threadId" | "size" | "hasAttachment" | "preview" => {
                return Err(SetError::props(
                    format!("{key} is set by the server"),
                    &[key],
                ));
            }
            other => {
                return Err(SetError::props(
                    format!("unknown property {other}"),
                    &[other],
                ));
            }
        }
    }
    if new.mailbox_ids.is_empty() {
        return Err(SetError::props(
            "mailboxIds must name at least one mailbox",
            &["mailboxIds"],
        ));
    }
    let bodies = parse_bodies(ctx, obj)?;
    new.text = bodies.text.map(Body::Text);
    new.html = bodies.html;
    new.attachments = bodies.attachments;
    if new.text.is_none() && new.html.is_none() {
        new.text = Some(Body::Text(String::new()));
    }
    Ok(new)
}

/// Applies an `Email/set` patch to mailboxes and keywords. Returns the new
/// values. Shared with `onSuccessUpdateEmail`.
pub(crate) fn apply_patch(
    ctx: &Ctx<'_>,
    email: &Email,
    patch: &Value,
) -> Result<(BTreeSet<String>, BTreeSet<String>), SetError> {
    let Some(obj) = patch.as_object() else {
        return Err(SetError::new("invalidPatch", "patch must be an object"));
    };
    let mut mailboxes = email.mailbox_ids.clone();
    let mut keywords = email.keywords.clone();
    let full_keys: BTreeSet<&str> = obj
        .keys()
        .filter(|k| !k.contains('/'))
        .map(String::as_str)
        .collect();
    for (path, value) in obj {
        let mut segments = path.splitn(2, '/');
        let head = segments.next().unwrap_or("");
        let tail = segments.next();
        if tail.is_some() && full_keys.contains(head) {
            return Err(SetError::new(
                "invalidPatch",
                format!("{path} overlaps with a full {head} value"),
            ));
        }
        match (head, tail) {
            ("mailboxIds", None) => mailboxes = bool_map(ctx, value, "mailboxIds", mailbox_key)?,
            ("keywords", None) => keywords = bool_map(ctx, value, "keywords", keyword_key)?,
            ("mailboxIds", Some(raw)) => {
                if raw.contains('/') {
                    return Err(SetError::new("invalidPatch", format!("{path} is too deep")));
                }
                let key = unescape(raw).ok_or_else(|| {
                    SetError::new("invalidPatch", format!("bad escape in {path}"))
                })?;
                let id = ctx.resolve_id(&key);
                match value {
                    Value::Bool(true) => match id.filter(|i| ctx.store.mailboxes.contains_key(i)) {
                        Some(id) => {
                            mailboxes.insert(id);
                        }
                        None => {
                            return Err(SetError::props(
                                format!("no mailbox {key:?}"),
                                &["mailboxIds"],
                            ));
                        }
                    },
                    Value::Null => {
                        if let Some(id) = id {
                            mailboxes.remove(&id);
                        }
                    }
                    _ => {
                        return Err(SetError::new(
                            "invalidPatch",
                            format!("{path} must be true or null"),
                        ));
                    }
                }
            }
            ("keywords", Some(raw)) => {
                if raw.contains('/') {
                    return Err(SetError::new("invalidPatch", format!("{path} is too deep")));
                }
                let key = unescape(raw).ok_or_else(|| {
                    SetError::new("invalidPatch", format!("bad escape in {path}"))
                })?;
                let Some(kw) = normalize_keyword(&key) else {
                    return Err(SetError::props(
                        format!("invalid keyword {key:?}"),
                        &["keywords"],
                    ));
                };
                match value {
                    Value::Bool(true) => {
                        keywords.insert(kw);
                    }
                    Value::Null => {
                        keywords.remove(&kw);
                    }
                    _ => {
                        return Err(SetError::new(
                            "invalidPatch",
                            format!("{path} must be true or null"),
                        ));
                    }
                }
            }
            _ => {
                return Err(SetError::props(
                    format!("{path} cannot be changed after creation"),
                    &[head],
                ));
            }
        }
    }
    if mailboxes.is_empty() {
        return Err(SetError::props(
            "an email must stay in at least one mailbox",
            &["mailboxIds"],
        ));
    }
    Ok((mailboxes, keywords))
}

/// Runs the update part of an `Email/set`. Shared with submissions.
pub(crate) fn update_emails(
    ctx: &mut Ctx<'_>,
    updates: Vec<(String, Value)>,
    result: &mut SetResult,
) {
    for (given, patch) in updates {
        let Some(id) = ctx
            .resolve_id(&given)
            .filter(|i| ctx.store.emails.contains_key(i))
        else {
            result
                .not_updated
                .insert(given, SetError::not_found().to_json());
            continue;
        };
        let applied = match ctx.store.emails.get(&id) {
            Some(email) => apply_patch(ctx, email, &patch),
            None => Err(SetError::not_found()),
        };
        match applied {
            Ok((mailboxes, keywords)) => {
                ctx.store.set_email_flags(&id, mailboxes, keywords);
                result.updated.insert(id, Value::Null);
            }
            Err(e) => {
                result.not_updated.insert(given, e.to_json());
            }
        }
    }
}

pub(crate) fn destroy_emails(ctx: &mut Ctx<'_>, ids: Vec<String>, result: &mut SetResult) {
    for given in ids {
        match ctx.resolve_id(&given) {
            Some(id) if ctx.store.destroy_email(&id) => result.destroyed.push(id),
            _ => {
                result
                    .not_destroyed
                    .insert(given, SetError::not_found().to_json());
            }
        }
    }
}

pub(super) fn set(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(
        args,
        &["accountId", "ifInState", "create", "update", "destroy"],
    )?;
    args.account(ctx.store)?;
    let old_state = ctx.store.logs.email.state();
    check_set(ctx, &args, &old_state)?;
    let ops = set_args(&args)?;
    let mut result = SetResult::default();
    for (cid, value) in ops.create {
        match parse_create(ctx, value) {
            Ok(new) => {
                let id = ctx.store.insert_email(new);
                ctx.created_ids.insert(cid.clone(), id.clone());
                let created = ctx.store.emails.get(&id).map(|e| {
                    json!({
                        "id": e.id,
                        "blobId": e.blob_id(),
                        "threadId": e.thread_id,
                        "size": e.size,
                    })
                });
                result.created.insert(cid, created.unwrap_or(Value::Null));
            }
            Err(e) => {
                result.not_created.insert(cid, e.to_json());
            }
        }
    }
    let updates = ops
        .update
        .into_iter()
        .map(|(k, v)| (k, v.clone()))
        .collect();
    update_emails(ctx, updates, &mut result);
    destroy_emails(ctx, ops.destroy, &mut result);
    let new_state = ctx.store.logs.email.state();
    Ok(result.into_json(&ctx.store.account_id, json!(old_state), json!(new_state)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_props_parse() {
        let hp = parse_header_prop("header:X-FM-Masked-Email:asText").unwrap();
        assert_eq!(hp.name, "X-FM-Masked-Email");
        assert_eq!(hp.form, Form::Text);
        assert!(!hp.all);
        let hp = parse_header_prop("header:List-Id:all").unwrap();
        assert!(hp.all);
        assert_eq!(hp.form, Form::Raw);
        assert!(parse_header_prop("header:From:asAddresses:all").is_ok());
        assert!(parse_header_prop("header:From:asGroupedAddresses").is_ok());
        for bad in [
            "header:",
            "header:From:asText",
            "header:Subject:asAddresses",
            "header:X:asNope",
            "header:X:all:asText",
            "header:X:asText:asRaw",
            "header:X:all:all",
            "header:Bad Name",
        ] {
            assert!(parse_header_prop(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn header_values() {
        let headers = vec![
            (
                "To".to_owned(),
                "A <a@example.com>, b@example.com".to_owned(),
            ),
            ("X-Tag".to_owned(), "one".to_owned()),
            ("x-tag".to_owned(), "  two  ".to_owned()),
        ];
        let hp = parse_header_prop("header:To:asAddresses").unwrap();
        assert_eq!(
            header_value(&headers, &hp),
            json!([{"name": "A", "email": "a@example.com"}, {"name": null, "email": "b@example.com"}])
        );
        let hp = parse_header_prop("header:X-Tag:asText").unwrap();
        assert_eq!(header_value(&headers, &hp), json!("two"));
        let hp = parse_header_prop("header:X-Tag:all").unwrap();
        assert_eq!(header_value(&headers, &hp), json!([" one", "   two  "]));
        let hp = parse_header_prop("header:Missing").unwrap();
        assert_eq!(header_value(&headers, &hp), Value::Null);
        let hp = parse_header_prop("header:Missing:all").unwrap();
        assert_eq!(header_value(&headers, &hp), json!([]));
    }

    #[test]
    fn keywords_normalize() {
        assert_eq!(normalize_keyword("$Seen").as_deref(), Some("$seen"));
        assert_eq!(normalize_keyword("$MDNSent").as_deref(), Some("$mdnsent"));
        for bad in ["", "a b", "a(b", "a]b", "a%", "a*", "a\"", "a\\", "é", "a{"] {
            assert!(normalize_keyword(bad).is_none(), "{bad}");
        }
        assert!(normalize_keyword(&"k".repeat(256)).is_none());
    }
}
