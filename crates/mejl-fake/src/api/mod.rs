//! The JMAP API endpoint: request envelope, result references, dispatch and
//! the argument helpers shared by every method (RFC 8620 sections 3 and 5).

mod email;
mod mailbox;
mod masked;
mod misc;
pub(crate) mod pointer;
mod query;
mod submission;

pub(crate) use email::normalize_keyword;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::changes::{ChangeLog, ChangesPage};
use crate::config::Quirks;
use crate::store::Store;

pub(crate) const CORE: &str = "urn:ietf:params:jmap:core";
pub(crate) const MAIL: &str = "urn:ietf:params:jmap:mail";
pub(crate) const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
pub(crate) const CONTACTS: &str = "urn:ietf:params:jmap:contacts";
pub(crate) const MASKED: &str = "https://www.fastmail.com/dev/maskedemail";

/// Every capability the fake supports, in session order.
pub(crate) const CAPABILITIES: &[&str] = &[CORE, MAIL, SUBMISSION, CONTACTS, MASKED];

/// Sort properties `Email/query` supports, for the session.
pub(crate) fn sort_options() -> &'static [&'static str] {
    query::SORT_OPTIONS
}

/// Collations `Email/query` supports, for the session.
pub(crate) fn collations() -> &'static [&'static str] {
    query::COLLATIONS
}

/// Methods and the capability each needs besides core.
const METHODS: &[(&str, &str)] = &[
    ("Core/echo", CORE),
    ("Mailbox/get", MAIL),
    ("Mailbox/changes", MAIL),
    ("Mailbox/set", MAIL),
    ("Email/get", MAIL),
    ("Email/changes", MAIL),
    ("Email/query", MAIL),
    ("Email/queryChanges", MAIL),
    ("Email/set", MAIL),
    ("Thread/get", MAIL),
    ("Thread/changes", MAIL),
    ("SearchSnippet/get", MAIL),
    ("Identity/get", SUBMISSION),
    ("Identity/changes", SUBMISSION),
    ("EmailSubmission/get", SUBMISSION),
    ("EmailSubmission/changes", SUBMISSION),
    ("EmailSubmission/set", SUBMISSION),
    ("MaskedEmail/get", MASKED),
    ("MaskedEmail/changes", MASKED),
    ("MaskedEmail/set", MASKED),
    ("AddressBook/get", CONTACTS),
    ("ContactCard/get", CONTACTS),
    ("ContactCard/changes", CONTACTS),
];

/// A method-level error (RFC 8620 section 3.6.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MethodError {
    pub kind: &'static str,
    pub description: Option<String>,
    pub extra: Map<String, Value>,
}

impl MethodError {
    pub(crate) fn new(kind: &'static str) -> Self {
        MethodError {
            kind,
            description: None,
            extra: Map::new(),
        }
    }

    pub(crate) fn with(kind: &'static str, description: impl Into<String>) -> Self {
        MethodError {
            kind,
            description: Some(description.into()),
            extra: Map::new(),
        }
    }

    pub(crate) fn invalid(description: impl Into<String>) -> Self {
        MethodError::with("invalidArguments", description)
    }

    fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("type".to_owned(), json!(self.kind));
        if let Some(d) = &self.description {
            obj.insert("description".to_owned(), json!(d));
        }
        Value::Object(obj)
    }
}

/// A per-object error in `/set` responses (RFC 8620 section 5.3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetError {
    pub kind: &'static str,
    pub description: Option<String>,
    pub properties: Option<Vec<String>>,
    pub extra: Map<String, Value>,
}

impl SetError {
    pub(crate) fn new(kind: &'static str, description: impl Into<String>) -> Self {
        SetError {
            kind,
            description: Some(description.into()),
            properties: None,
            extra: Map::new(),
        }
    }

    pub(crate) fn props(description: impl Into<String>, props: &[&str]) -> Self {
        SetError {
            kind: "invalidProperties",
            description: Some(description.into()),
            properties: Some(props.iter().map(|p| (*p).to_owned()).collect()),
            extra: Map::new(),
        }
    }

    pub(crate) fn not_found() -> Self {
        SetError::new("notFound", "no such object")
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("type".to_owned(), json!(self.kind));
        if let Some(d) = &self.description {
            obj.insert("description".to_owned(), json!(d));
        }
        if let Some(p) = &self.properties {
            obj.insert("properties".to_owned(), json!(p));
        }
        Value::Object(obj)
    }
}

/// A request-level error, sent as RFC 7807 problem details with HTTP 400.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestError {
    NotJson(String),
    NotRequest(String),
    UnknownCapability(String),
    Limit { limit: &'static str, detail: String },
}

impl RequestError {
    pub(crate) fn to_problem(&self) -> Value {
        let (kind, detail, limit) = match self {
            RequestError::NotJson(d) => ("notJSON", d, None),
            RequestError::NotRequest(d) => ("notRequest", d, None),
            RequestError::UnknownCapability(d) => ("unknownCapability", d, None),
            RequestError::Limit { limit, detail } => ("limit", detail, Some(*limit)),
        };
        let mut obj = json!({
            "type": format!("urn:ietf:params:jmap:error:{kind}"),
            "status": 400,
            "detail": detail,
        });
        if let (Some(limit), Some(map)) = (limit, obj.as_object_mut()) {
            map.insert("limit".to_owned(), json!(limit));
        }
        obj
    }
}

/// Per-request context handed to every method.
pub(crate) struct Ctx<'a> {
    pub store: &'a mut Store,
    pub quirks: &'a Mutex<Quirks>,
    pub using: BTreeSet<String>,
    pub created_ids: BTreeMap<String, String>,
    pub masked_without_state: bool,
}

impl Ctx<'_> {
    /// Resolves `#creationId` references made earlier in this request.
    pub(crate) fn resolve_id(&self, id: &str) -> Option<String> {
        match id.strip_prefix('#') {
            Some(cid) => self.created_ids.get(cid).cloned(),
            None => Some(id.to_owned()),
        }
    }

    pub(crate) fn quirks(&self) -> std::sync::MutexGuard<'_, Quirks> {
        self.quirks.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One method call: name, arguments, call id.
type Call = (String, Map<String, Value>, String);

/// One method response: name, arguments, call id.
pub(crate) type Response = (String, Value, String);

/// Handles a POST to the API endpoint. Returns the response object, or a
/// request-level error.
pub(crate) fn process(
    store: &mut Store,
    quirks: &Mutex<Quirks>,
    body: &[u8],
) -> Result<Value, RequestError> {
    if body.len() as u64 > store.limits.max_size_request {
        return Err(RequestError::Limit {
            limit: "maxSizeRequest",
            detail: format!(
                "request is {} bytes, the limit is {}",
                body.len(),
                store.limits.max_size_request
            ),
        });
    }
    let request: Value = serde_json::from_slice(body)
        .map_err(|e| RequestError::NotJson(format!("request body is not JSON: {e}")))?;
    let Some(obj) = request.as_object() else {
        return Err(RequestError::NotRequest(
            "request must be a JSON object".into(),
        ));
    };
    if let Some(key) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "using" | "methodCalls" | "createdIds"))
    {
        return Err(RequestError::NotRequest(format!(
            "unknown request property {key:?}"
        )));
    }
    let using = parse_using(obj.get("using"))?;
    let calls = parse_calls(obj.get("methodCalls"))?;
    let unknown: Vec<&String> = using
        .iter()
        .filter(|c| !CAPABILITIES.contains(&c.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(RequestError::UnknownCapability(format!(
            "unsupported capabilities in using: {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if calls.len() as u64 > store.limits.max_calls_in_request {
        return Err(RequestError::Limit {
            limit: "maxCallsInRequest",
            detail: format!(
                "{} method calls, the limit is {}",
                calls.len(),
                store.limits.max_calls_in_request
            ),
        });
    }
    let (created_ids, echo_created) = match obj.get("createdIds") {
        None => (BTreeMap::new(), false),
        Some(Value::Object(m)) => {
            let mut out = BTreeMap::new();
            for (k, v) in m {
                let Some(s) = v.as_str() else {
                    return Err(RequestError::NotRequest(
                        "createdIds values must be strings".into(),
                    ));
                };
                out.insert(k.clone(), s.to_owned());
            }
            (out, true)
        }
        Some(_) => {
            return Err(RequestError::NotRequest(
                "createdIds must be an object".into(),
            ));
        }
    };
    let masked_without_state = quirks
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .masked_email_without_state;
    let mut ctx = Ctx {
        store,
        quirks,
        using,
        created_ids,
        masked_without_state,
    };
    let mut responses: Vec<Response> = Vec::new();
    for (name, args, call_id) in calls {
        let out = run_call(&mut ctx, &name, args, &call_id, &responses);
        ctx.store.flush_counts();
        responses.extend(out);
    }
    let mut response = json!({
        "methodResponses": responses
            .into_iter()
            .map(|(n, a, id)| json!([n, a, id]))
            .collect::<Vec<_>>(),
        "sessionState": ctx.store.session_state,
    });
    if echo_created && let Some(map) = response.as_object_mut() {
        map.insert("createdIds".to_owned(), json!(ctx.created_ids));
    }
    Ok(response)
}

fn parse_using(v: Option<&Value>) -> Result<BTreeSet<String>, RequestError> {
    let Some(Value::Array(items)) = v else {
        return Err(RequestError::NotRequest(
            "using must be an array of strings".into(),
        ));
    };
    items
        .iter()
        .map(|i| {
            i.as_str()
                .map(str::to_owned)
                .ok_or_else(|| RequestError::NotRequest("using must be an array of strings".into()))
        })
        .collect()
}

fn parse_calls(v: Option<&Value>) -> Result<Vec<Call>, RequestError> {
    let Some(Value::Array(items)) = v else {
        return Err(RequestError::NotRequest(
            "methodCalls must be an array".into(),
        ));
    };
    let bad =
        || RequestError::NotRequest("each method call must be [name, arguments, callId]".into());
    items
        .iter()
        .map(|call| {
            let parts = call.as_array().ok_or_else(bad)?;
            match parts.as_slice() {
                [Value::String(name), Value::Object(args), Value::String(id)] => {
                    Ok((name.clone(), args.clone(), id.clone()))
                }
                _ => Err(bad()),
            }
        })
        .collect()
}

fn run_call(
    ctx: &mut Ctx<'_>,
    name: &str,
    args: Map<String, Value>,
    call_id: &str,
    previous: &[Response],
) -> Vec<Response> {
    let error = |e: MethodError| vec![("error".to_owned(), e.to_json(), call_id.to_owned())];
    let Some((_, capability)) = METHODS.iter().find(|(n, _)| *n == name) else {
        return error(MethodError::with(
            "unknownMethod",
            format!("unknown method {name}"),
        ));
    };
    // Fastmail is reported to require core in `using` for every call.
    if !ctx.using.contains(CORE) || !ctx.using.contains(*capability) {
        return error(MethodError::with(
            "unknownMethod",
            format!("{name} needs {capability} and {CORE} in using"),
        ));
    }
    let args = match resolve_references(args, previous) {
        Ok(a) => a,
        Err(e) => return error(e),
    };
    match dispatch(ctx, name, &args) {
        Ok(responses) => responses
            .into_iter()
            .map(|(n, a)| (n, a, call_id.to_owned()))
            .collect(),
        Err(e) => error(e),
    }
}

fn dispatch(
    ctx: &mut Ctx<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Vec<(String, Value)>, MethodError> {
    let one = |v: Value| Ok(vec![(name.to_owned(), v)]);
    match name {
        "Core/echo" => one(Value::Object(args.clone())),
        "Mailbox/get" => one(mailbox::get(ctx, args)?),
        "Mailbox/changes" => one(mailbox::changes(ctx, args)?),
        "Mailbox/set" => one(mailbox::set(ctx, args)?),
        "Email/get" => one(email::get(ctx, args)?),
        "Email/changes" => one(email::changes(ctx, args)?),
        "Email/set" => one(email::set(ctx, args)?),
        "Email/query" => one(query::query(ctx, args)?),
        "Email/queryChanges" => one(query::query_changes(ctx, args)?),
        "SearchSnippet/get" => one(query::snippets(ctx, args)?),
        "Thread/get" => one(misc::thread_get(ctx, args)?),
        "Thread/changes" => one(misc::thread_changes(ctx, args)?),
        "Identity/get" => one(misc::identity_get(ctx, args)?),
        "Identity/changes" => one(misc::identity_changes(ctx, args)?),
        "EmailSubmission/get" => one(submission::get(ctx, args)?),
        "EmailSubmission/changes" => one(submission::changes(ctx, args)?),
        "EmailSubmission/set" => submission::set(ctx, args),
        "MaskedEmail/get" => one(masked::get(ctx, args)?),
        "MaskedEmail/changes" => one(masked::changes(ctx, args)?),
        "MaskedEmail/set" => one(masked::set(ctx, args)?),
        "AddressBook/get" => one(misc::address_book_get(ctx, args)?),
        "ContactCard/get" => one(misc::contact_get(ctx, args)?),
        "ContactCard/changes" => one(misc::contact_changes(ctx, args)?),
        _ => Err(MethodError::with(
            "unknownMethod",
            format!("unknown method {name}"),
        )),
    }
}

/// Replaces `#name` arguments with the values their result references
/// point at (RFC 8620 section 3.7).
fn resolve_references(
    args: Map<String, Value>,
    previous: &[Response],
) -> Result<Map<String, Value>, MethodError> {
    let mut out = Map::new();
    for (key, value) in &args {
        let Some(plain) = key.strip_prefix('#') else {
            out.insert(key.clone(), value.clone());
            continue;
        };
        if args.contains_key(plain) {
            return Err(MethodError::invalid(format!(
                "both {plain} and #{plain} were given"
            )));
        }
        let bad =
            |why: &str| MethodError::with("invalidResultReference", format!("#{plain}: {why}"));
        let Some(reference) = value.as_object() else {
            return Err(bad("not a ResultReference object"));
        };
        if reference
            .keys()
            .any(|k| !matches!(k.as_str(), "resultOf" | "name" | "path"))
        {
            return Err(bad("unknown property in ResultReference"));
        }
        let (Some(result_of), Some(name), Some(path)) = (
            reference.get("resultOf").and_then(Value::as_str),
            reference.get("name").and_then(Value::as_str),
            reference.get("path").and_then(Value::as_str),
        ) else {
            return Err(bad("resultOf, name and path must be strings"));
        };
        let Some((_, target, _)) = previous
            .iter()
            .find(|(n, _, id)| id == result_of && n == name)
        else {
            return Err(bad(&format!(
                "no earlier {name} response with call id {result_of:?}"
            )));
        };
        let Some(resolved) = pointer::evaluate(target, path) else {
            return Err(bad(&format!("path {path:?} does not resolve")));
        };
        out.insert(plain.to_owned(), resolved);
    }
    Ok(out)
}

/// Typed access to method arguments that rejects unknown names.
pub(crate) struct Args<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    pub(crate) fn new(map: &'a Map<String, Value>, allowed: &[&str]) -> Result<Self, MethodError> {
        if let Some(k) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(MethodError::invalid(format!("unknown argument {k:?}")));
        }
        Ok(Args { map })
    }

    pub(crate) fn raw(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    /// Checks `accountId`.
    pub(crate) fn account(&self, store: &Store) -> Result<(), MethodError> {
        match self.map.get("accountId") {
            Some(Value::String(id)) if *id == store.account_id => Ok(()),
            Some(Value::String(_)) => Err(MethodError::new("accountNotFound")),
            _ => Err(MethodError::invalid("accountId must be a string")),
        }
    }

    pub(crate) fn str_opt(&self, key: &str) -> Result<Option<&'a str>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(MethodError::invalid(format!("{key} must be a string"))),
        }
    }

    pub(crate) fn str_req(&self, key: &str) -> Result<&'a str, MethodError> {
        self.str_opt(key)?
            .ok_or_else(|| MethodError::invalid(format!("{key} is required")))
    }

    pub(crate) fn bool_opt(&self, key: &str) -> Result<Option<bool>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(MethodError::invalid(format!("{key} must be a boolean"))),
        }
    }

    pub(crate) fn uint_opt(&self, key: &str) -> Result<Option<u64>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n < (1u64 << 53))
                .map(Some)
                .ok_or_else(|| MethodError::invalid(format!("{key} must be an UnsignedInt"))),
        }
    }

    pub(crate) fn int_opt(&self, key: &str) -> Result<Option<i64>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_i64()
                .filter(|n| n.unsigned_abs() < (1u64 << 53))
                .map(Some)
                .ok_or_else(|| MethodError::invalid(format!("{key} must be an Int"))),
        }
    }

    /// An `Id[]|null` argument. Absent and null both give `None`.
    pub(crate) fn ids_opt(&self, key: &str) -> Result<Option<Vec<String>>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| MethodError::invalid(format!("{key} must hold strings")))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(MethodError::invalid(format!(
                "{key} must be an array or null"
            ))),
        }
    }

    pub(crate) fn obj_opt(&self, key: &str) -> Result<Option<&'a Map<String, Value>>, MethodError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(m)) => Ok(Some(m)),
            Some(_) => Err(MethodError::invalid(format!(
                "{key} must be an object or null"
            ))),
        }
    }
}

/// Standard `/get` arguments.
pub(crate) const GET_ARGS: &[&str] = &["accountId", "ids", "properties"];
/// Standard `/changes` arguments.
pub(crate) const CHANGES_ARGS: &[&str] = &["accountId", "sinceState", "maxChanges"];

/// Resolves `ids` for a `/get`, enforcing `maxObjectsInGet`. Each entry is
/// the id as the client wrote it and the id after creation-id lookup.
pub(crate) fn get_ids(
    ctx: &Ctx<'_>,
    args: &Args<'_>,
    all: impl FnOnce() -> Vec<String>,
) -> Result<Vec<(String, Option<String>)>, MethodError> {
    let limit = ctx.store.limits.max_objects_in_get;
    match args.ids_opt("ids")? {
        Some(ids) => {
            if ids.len() as u64 > limit {
                return Err(MethodError::with(
                    "requestTooLarge",
                    format!("{} ids, maxObjectsInGet is {limit}", ids.len()),
                ));
            }
            let mut seen = BTreeSet::new();
            Ok(ids
                .into_iter()
                .filter(|id| seen.insert(id.clone()))
                .map(|id| {
                    let resolved = ctx.resolve_id(&id);
                    (id, resolved)
                })
                .collect())
        }
        None => {
            let all = all();
            if all.len() as u64 > limit {
                return Err(MethodError::with(
                    "requestTooLarge",
                    format!("{} objects, maxObjectsInGet is {limit}", all.len()),
                ));
            }
            Ok(all.into_iter().map(|id| (id.clone(), Some(id))).collect())
        }
    }
}

/// Parses `properties`. `None` means the type's default set.
pub(crate) fn get_properties(
    args: &Args<'_>,
    allowed: &[&str],
) -> Result<Option<BTreeSet<String>>, MethodError> {
    let Some(props) = args.ids_opt("properties")? else {
        return Ok(None);
    };
    if let Some(bad) = props.iter().find(|p| !allowed.contains(&p.as_str())) {
        return Err(MethodError::invalid(format!("unknown property {bad:?}")));
    }
    let mut set: BTreeSet<String> = props.into_iter().collect();
    set.insert("id".to_owned());
    Ok(Some(set))
}

/// Keeps only the selected properties of an object.
pub(crate) fn select(obj: Value, props: &Option<BTreeSet<String>>) -> Value {
    match (obj, props) {
        (Value::Object(map), Some(props)) => {
            Value::Object(map.into_iter().filter(|(k, _)| props.contains(k)).collect())
        }
        (obj, _) => obj,
    }
}

/// A complete standard `/get` for types whose objects are cheap to build.
pub(crate) fn simple_get(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
    allowed: &[&str],
    state: Value,
    all_ids: Vec<String>,
    lookup: impl Fn(&str) -> Option<Value>,
) -> Result<Value, MethodError> {
    let args = Args::new(args, GET_ARGS)?;
    args.account(ctx.store)?;
    let props = get_properties(&args, allowed)?;
    let ids = get_ids(ctx, &args, || all_ids)?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for (given, resolved) in ids {
        match resolved.as_deref().and_then(&lookup) {
            Some(obj) => list.push(select(obj, &props)),
            None => not_found.push(given),
        }
    }
    Ok(json!({
        "accountId": ctx.store.account_id,
        "state": state,
        "list": list,
        "notFound": not_found,
    }))
}

/// A complete standard `/changes`.
pub(crate) fn changes_from_log(
    ctx: &Ctx<'_>,
    args: &Map<String, Value>,
    log: &ChangeLog,
    extra: impl FnOnce(&ChangesPage, &mut Map<String, Value>),
) -> Result<Value, MethodError> {
    let args = Args::new(args, CHANGES_ARGS)?;
    args.account(ctx.store)?;
    let since = args.str_req("sinceState")?;
    let max = args.uint_opt("maxChanges")?;
    if max == Some(0) {
        return Err(MethodError::invalid("maxChanges must be greater than 0"));
    }
    let max = max.map(|m| usize::try_from(m).unwrap_or(usize::MAX));
    let Some(page) = log.changes(since, max) else {
        return Err(MethodError::with(
            "cannotCalculateChanges",
            format!("state {since:?} is unknown or too old"),
        ));
    };
    let mut out = Map::new();
    out.insert("accountId".to_owned(), json!(ctx.store.account_id));
    out.insert("oldState".to_owned(), json!(page.old_state));
    out.insert("newState".to_owned(), json!(page.new_state));
    out.insert("hasMoreChanges".to_owned(), json!(page.has_more_changes));
    out.insert("created".to_owned(), json!(page.created));
    out.insert("updated".to_owned(), json!(page.updated));
    out.insert("destroyed".to_owned(), json!(page.destroyed));
    extra(&page, &mut out);
    Ok(Value::Object(out))
}

/// Checks `ifInState` and `maxObjectsInSet` for a `/set`.
pub(crate) fn check_set(
    ctx: &Ctx<'_>,
    args: &Args<'_>,
    current_state: &str,
) -> Result<(), MethodError> {
    if let Some(expected) = args.str_opt("ifInState")?
        && expected != current_state
    {
        return Err(MethodError::with(
            "stateMismatch",
            format!("ifInState {expected:?} does not match {current_state:?}"),
        ));
    }
    let count = |key: &str| -> usize {
        match args.raw(key) {
            Some(Value::Object(m)) => m.len(),
            Some(Value::Array(a)) => a.len(),
            _ => 0,
        }
    };
    let total = count("create") + count("update") + count("destroy");
    let limit = ctx.store.limits.max_objects_in_set;
    if total as u64 > limit {
        return Err(MethodError::with(
            "requestTooLarge",
            format!("{total} objects, maxObjectsInSet is {limit}"),
        ));
    }
    for key in ["create", "update"] {
        match args.raw(key) {
            None | Some(Value::Null) | Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(MethodError::invalid(format!(
                    "{key} must be an object or null"
                )));
            }
        }
    }
    match args.raw("destroy") {
        None | Some(Value::Null) | Some(Value::Array(_)) => Ok(()),
        Some(_) => Err(MethodError::invalid("destroy must be an array or null")),
    }
}

/// Collects `/set` results into the standard response shape.
#[derive(Default)]
pub(crate) struct SetResult {
    pub created: Map<String, Value>,
    pub not_created: Map<String, Value>,
    pub updated: Map<String, Value>,
    pub not_updated: Map<String, Value>,
    pub destroyed: Vec<String>,
    pub not_destroyed: Map<String, Value>,
}

impl SetResult {
    pub(crate) fn into_json(self, account_id: &str, old_state: Value, new_state: Value) -> Value {
        let opt_map = |m: Map<String, Value>| {
            if m.is_empty() {
                Value::Null
            } else {
                Value::Object(m)
            }
        };
        json!({
            "accountId": account_id,
            "oldState": old_state,
            "newState": new_state,
            "created": opt_map(self.created),
            "notCreated": opt_map(self.not_created),
            "updated": opt_map(self.updated),
            "notUpdated": opt_map(self.not_updated),
            "destroyed": if self.destroyed.is_empty() { Value::Null } else { json!(self.destroyed) },
            "notDestroyed": opt_map(self.not_destroyed),
        })
    }
}

/// The `create`, `update` and `destroy` arguments of a `/set`.
pub(crate) struct SetArgs<'a> {
    pub create: Vec<(String, &'a Value)>,
    pub update: Vec<(String, &'a Value)>,
    pub destroy: Vec<String>,
}

pub(crate) fn set_args<'a>(args: &Args<'a>) -> Result<SetArgs<'a>, MethodError> {
    let create = args
        .obj_opt("create")?
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();
    let update = args
        .obj_opt("update")?
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();
    let destroy = args.ids_opt("destroy")?.unwrap_or_default();
    Ok(SetArgs {
        create,
        update,
        destroy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn responses() -> Vec<Response> {
        vec![
            (
                "Email/query".to_owned(),
                json!({"ids": ["e1", "e2"]}),
                "q".to_owned(),
            ),
            (
                "error".to_owned(),
                json!({"type": "serverFail"}),
                "bad".to_owned(),
            ),
        ]
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn references_resolve() {
        let a = args(
            json!({"accountId": "u", "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"}}),
        );
        let out = resolve_references(a, &responses()).unwrap();
        assert_eq!(out.get("ids"), Some(&json!(["e1", "e2"])));
        assert!(!out.contains_key("#ids"));
    }

    #[test]
    fn reference_errors() {
        let cases = [
            json!({"#ids": {"resultOf": "q", "name": "Email/get", "path": "/ids"}}),
            json!({"#ids": {"resultOf": "nope", "name": "Email/query", "path": "/ids"}}),
            json!({"#ids": {"resultOf": "q", "name": "Email/query", "path": "/missing"}}),
            json!({"#ids": {"resultOf": "bad", "name": "Email/query", "path": "/ids"}}),
            json!({"#ids": {"resultOf": "q", "name": "Email/query"}}),
            json!({"#ids": "q"}),
            json!({"#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids", "x": 1}}),
        ];
        for case in cases {
            let err = resolve_references(args(case.clone()), &responses()).unwrap_err();
            assert_eq!(err.kind, "invalidResultReference", "{case}");
        }
        let both =
            json!({"ids": [], "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"}});
        let err = resolve_references(args(both), &responses()).unwrap_err();
        assert_eq!(err.kind, "invalidArguments");
    }

    #[test]
    fn problem_details() {
        let p = RequestError::Limit {
            limit: "maxCallsInRequest",
            detail: "x".into(),
        }
        .to_problem();
        assert_eq!(p["type"], "urn:ietf:params:jmap:error:limit");
        assert_eq!(p["limit"], "maxCallsInRequest");
        assert_eq!(p["status"], 400);
    }

    #[test]
    fn args_reject_unknown_and_mistyped() {
        let m = args(json!({"accountId": "u", "fetchTextBodyValue": true}));
        assert!(Args::new(&m, &["accountId", "fetchTextBodyValues"]).is_err());
        let m = args(json!({"n": -1, "f": 1.5, "s": 3, "big": 9007199254740993u64}));
        let a = Args::new(&m, &["n", "f", "s", "big"]).unwrap();
        assert!(a.uint_opt("n").is_err());
        assert!(a.uint_opt("f").is_err());
        assert!(a.str_opt("s").is_err());
        assert!(a.uint_opt("big").is_err());
        assert_eq!(a.int_opt("n").unwrap(), Some(-1));
        assert_eq!(a.uint_opt("missing").unwrap(), None);
    }
}
