//! Building JMAP requests and decoding their responses (RFC 8620 sections 3.3
//! to 3.7).
//!
//! ```
//! use mejl_jmap::RequestBuilder;
//! use mejl_jmap::methods::{EmailGet, EmailQuery};
//! use mejl_jmap::method::{GetArgs, QueryArgs};
//!
//! let mut req = RequestBuilder::new();
//! let query = req.call::<EmailQuery>(QueryArgs::new("A1").into());
//! let _get = req.call_with_refs::<EmailGet>(
//!     GetArgs::by_reference("A1").properties(["subject"]).into(),
//!     [("ids", query.ids())],
//! );
//! let request = req.build().unwrap();
//! assert_eq!(request.using, ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"]);
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, DeserializeOwned};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::{Error, MethodError, MethodErrorType};
use crate::method::Method;
use crate::methods::{EmailGet, EmailSet, EmailSubmissionSet, ThreadGet};
use crate::session::capability;
use crate::types::core::State;

/// One method call or response: `[name, arguments, callId]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub name: String,
    pub arguments: Value,
    pub call_id: String,
}

impl Invocation {
    /// The method name used for error responses.
    pub const ERROR: &'static str = "error";

    /// A method error response, for servers and fakes.
    pub fn error(call_id: impl Into<String>, error: &MethodError) -> Self {
        Self {
            name: Self::ERROR.to_owned(),
            arguments: serde_json::to_value(error)
                .unwrap_or_else(|_| serde_json::json!({ "type": error.kind.as_str() })),
            call_id: call_id.into(),
        }
    }
}

impl Serialize for Invocation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tuple = serializer.serialize_tuple(3)?;
        tuple.serialize_element(&self.name)?;
        tuple.serialize_element(&self.arguments)?;
        tuple.serialize_element(&self.call_id)?;
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for Invocation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, arguments, call_id) = <(String, Value, String)>::deserialize(deserializer)?;
        if !arguments.is_object() {
            return Err(de::Error::custom("invocation arguments must be an object"));
        }
        Ok(Self {
            name,
            arguments,
            call_id,
        })
    }
}

/// A JMAP Request object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub using: Vec<String>,
    pub method_calls: Vec<Invocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<Map<String, Value>>,
}

/// A reference to part of an earlier call's result (RFC 8620 section 3.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultReference {
    pub result_of: String,
    pub name: String,
    /// A JSON pointer where `*` maps over an array.
    pub path: String,
}

/// A typed handle to a call in a request, used to read its response and
/// to reference its results from later calls.
pub struct CallHandle<M> {
    call_id: String,
    _method: PhantomData<fn() -> M>,
}

impl<M> fmt::Debug for CallHandle<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallHandle")
            .field("call_id", &self.call_id)
            .finish()
    }
}

impl<M> Clone for CallHandle<M> {
    fn clone(&self) -> Self {
        Self {
            call_id: self.call_id.clone(),
            _method: PhantomData,
        }
    }
}

impl<M: Method> CallHandle<M> {
    /// The call id in the request.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// A reference to `path` in this call's result.
    pub fn reference(&self, path: impl Into<String>) -> ResultReference {
        ResultReference {
            result_of: self.call_id.clone(),
            name: M::NAME.to_owned(),
            path: path.into(),
        }
    }

    /// The ids of every object in a `/get` list (`/list/*/id`).
    pub fn list_ids(&self) -> ResultReference {
        self.reference("/list/*/id")
    }

    /// `/ids` of a `/query` result.
    pub fn ids(&self) -> ResultReference {
        self.reference("/ids")
    }

    /// `/created` of a `/changes` result.
    pub fn created(&self) -> ResultReference {
        self.reference("/created")
    }

    /// `/updated` of a `/changes` result.
    pub fn updated(&self) -> ResultReference {
        self.reference("/updated")
    }

    /// `/destroyed` of a `/changes` result.
    pub fn destroyed(&self) -> ResultReference {
        self.reference("/destroyed")
    }
}

impl CallHandle<EmailGet> {
    /// Thread ids of the fetched emails (`/list/*/threadId`).
    pub fn thread_ids(&self) -> ResultReference {
        self.reference("/list/*/threadId")
    }
}

impl CallHandle<ThreadGet> {
    /// Email ids of the fetched threads (`/list/*/emailIds`).
    pub fn email_ids(&self) -> ResultReference {
        self.reference("/list/*/emailIds")
    }
}

/// Builds a [`Request`], computing `using` from the calls added.
#[derive(Debug, Default)]
pub struct RequestBuilder {
    using: BTreeSet<String>,
    calls: Vec<Invocation>,
    // The first failure, reported by `build` so `call` stays chainable.
    error: Option<Error>,
}

impl RequestBuilder {
    /// An empty request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a call and get a handle to its result.
    pub fn call<M: Method>(&mut self, args: M::Args) -> CallHandle<M> {
        self.call_with_refs::<M>(args, [])
    }

    /// Add a call whose arguments are partly filled by result references,
    /// given as `(argument name, reference)` pairs. The plain argument of
    /// the same name is removed, because sending both is invalid.
    pub fn call_with_refs<'k, M: Method>(
        &mut self,
        args: M::Args,
        refs: impl IntoIterator<Item = (&'k str, ResultReference)>,
    ) -> CallHandle<M> {
        let call_id = format!("c{}", self.calls.len());
        let arguments = match Self::encode::<M>(args, refs) {
            Ok(arguments) => arguments,
            Err(err) => {
                self.error.get_or_insert(err);
                Value::Object(Map::new())
            }
        };
        self.using
            .extend(M::CAPABILITIES.iter().map(|c| (*c).to_owned()));
        self.calls.push(Invocation {
            name: M::NAME.to_owned(),
            arguments,
            call_id: call_id.clone(),
        });
        CallHandle {
            call_id,
            _method: PhantomData,
        }
    }

    fn encode<'k, M: Method>(
        args: M::Args,
        refs: impl IntoIterator<Item = (&'k str, ResultReference)>,
    ) -> Result<Value, Error> {
        let value = serde_json::to_value(&args).map_err(|source| Error::Encode {
            context: format!("{} arguments", M::NAME),
            source,
        })?;
        let Value::Object(mut map) = value else {
            return Err(Error::Encode {
                context: format!("{} arguments", M::NAME),
                source: serde::ser::Error::custom("arguments are not a JSON object"),
            });
        };
        for (key, reference) in refs {
            let key = key.strip_prefix('#').unwrap_or(key);
            map.remove(key);
            let reference = serde_json::to_value(&reference).map_err(|source| Error::Encode {
                context: "result reference".into(),
                source,
            })?;
            map.insert(format!("#{key}"), reference);
        }
        Ok(Value::Object(map))
    }

    /// Add a capability to `using` that no call implies.
    pub fn using(&mut self, capability: impl Into<String>) -> &mut Self {
        self.using.insert(capability.into());
        self
    }

    /// The number of calls added so far.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    /// Whether no calls were added.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Finish the request. `using` always starts with core, then the other
    /// capabilities in sorted order.
    pub fn build(self) -> Result<Request, Error> {
        if let Some(err) = self.error {
            return Err(err);
        }
        let mut using = vec![capability::CORE.to_owned()];
        using.extend(self.using.into_iter().filter(|c| c != capability::CORE));
        Ok(Request {
            using,
            method_calls: self.calls,
            created_ids: None,
        })
    }
}

/// A JMAP Response object with typed access per call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub method_responses: Vec<Invocation>,
    #[serde(default)]
    pub session_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<Map<String, Value>>,
    /// Set by the client when `session_state` differs from the loaded
    /// session, meaning the session should be fetched again.
    #[serde(skip)]
    pub session_changed: bool,
}

impl Response {
    /// The result of a call.
    ///
    /// A method error for that call comes back as [`Error::Method`].
    pub fn get<M: Method>(&self, handle: &CallHandle<M>) -> Result<M::Response, Error> {
        self.get_as::<M::Response>(&handle.call_id, M::NAME)
    }

    /// The implicit `Email/set` result that `onSuccessUpdateEmail` or
    /// `onSuccessDestroyEmail` adds under the submission's call id. `None`
    /// when the server sent none, which happens when no submission
    /// succeeded.
    pub fn implicit_email_set(
        &self,
        handle: &CallHandle<EmailSubmissionSet>,
    ) -> Option<Result<<EmailSet as Method>::Response, Error>> {
        self.find(&handle.call_id, EmailSet::NAME)?;
        Some(self.get_as(&handle.call_id, EmailSet::NAME))
    }

    /// Decode the response named `name` for `call_id` into `T`.
    pub fn get_as<T: DeserializeOwned>(&self, call_id: &str, name: &str) -> Result<T, Error> {
        if let Some(inv) = self.find(call_id, name) {
            return T::deserialize(&inv.arguments).map_err(|source| Error::Decode {
                context: format!("{name} response"),
                source,
            });
        }
        if let Some(inv) = self.find(call_id, Invocation::ERROR) {
            let error = MethodError::deserialize(&inv.arguments).unwrap_or_else(|_| {
                MethodError::new(MethodErrorType::Other("malformed error".into()))
            });
            return Err(Error::Method {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                error: Box::new(error),
            });
        }
        Err(Error::MissingResponse {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
        })
    }

    fn find(&self, call_id: &str, name: &str) -> Option<&Invocation> {
        self.method_responses
            .iter()
            .find(|inv| inv.call_id == call_id && inv.name == name)
    }
}

impl ResultReference {
    /// Evaluate the reference against earlier responses, as a server does.
    ///
    /// Fails with `invalidResultReference` if the call is missing, its name
    /// differs, or the path does not resolve. Arrays produced by `*` that
    /// themselves hold arrays are flattened one level, as RFC 8620 section
    /// 3.7 requires.
    pub fn resolve(&self, responses: &[Invocation]) -> Result<Value, MethodError> {
        let invalid = |why: &str| {
            MethodError::new(MethodErrorType::InvalidResultReference).with_description(why)
        };
        let inv = responses
            .iter()
            .find(|inv| inv.call_id == self.result_of)
            .ok_or_else(|| invalid("no response for resultOf"))?;
        if inv.name != self.name {
            return Err(invalid("name does not match the referenced response"));
        }
        evaluate_pointer(&inv.arguments, &self.path).ok_or_else(|| invalid("path does not resolve"))
    }
}

/// Evaluate a JMAP extended JSON pointer: RFC 6901 plus `*` over arrays.
pub fn evaluate_pointer(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }
    let rest = path.strip_prefix('/')?;
    let segments: Vec<&str> = rest.split('/').collect();
    walk(value, &segments)
}

fn walk(value: &Value, segments: &[&str]) -> Option<Value> {
    let Some((first, rest)) = segments.split_first() else {
        return Some(value.clone());
    };
    match value {
        Value::Array(items) if *first == "*" => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match walk(item, rest)? {
                    Value::Array(inner) => out.extend(inner),
                    other => out.push(other),
                }
            }
            Some(Value::Array(out))
        }
        Value::Array(items) => {
            let index = parse_index(first)?;
            walk(items.get(index)?, rest)
        }
        Value::Object(map) => {
            let key = crate::patch::unescape_segment(first)?;
            walk(map.get(key.as_ref())?, rest)
        }
        _ => None,
    }
}

fn parse_index(segment: &str) -> Option<usize> {
    // RFC 6901: no leading zeros, no signs.
    if segment.is_empty()
        || (segment.len() > 1 && segment.starts_with('0'))
        || !segment.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    segment.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Comparator, EmailFilterCondition};
    use crate::method::{ChangesArgs, GetArgs, QueryArgs, SetArgs};
    use crate::methods::*;
    use crate::patch::Patch;
    use crate::session::capability::{MASKED_EMAIL, SUBMISSION};
    use crate::types::{Email, MaskedEmail, Thread};
    use serde_json::json;

    #[test]
    fn using_always_has_core_first_and_is_sorted() {
        let mut req = RequestBuilder::new();
        req.call::<MaskedEmailGet>(GetArgs::all("A1"));
        req.call::<MailboxGet>(GetArgs::all("A1"));
        req.call::<IdentityGet>(GetArgs::all("A1"));
        let request = req.build().unwrap();
        assert_eq!(
            request.using,
            vec![
                capability::CORE.to_string(),
                MASKED_EMAIL.to_string(),
                capability::MAIL.to_string(),
                SUBMISSION.to_string(),
            ]
        );
    }

    #[test]
    fn empty_request_still_uses_core() {
        let request = RequestBuilder::new().build().unwrap();
        assert_eq!(request.using, vec![capability::CORE.to_string()]);
        assert!(request.method_calls.is_empty());
    }

    #[test]
    fn explicit_using_is_deduplicated() {
        let mut req = RequestBuilder::new();
        req.using(capability::CORE).using(capability::MAIL);
        req.call::<EmailSet>(SetArgs::new("A1"));
        assert_eq!(
            req.build().unwrap().using,
            vec![capability::CORE.to_string(), capability::MAIL.to_string()]
        );
    }

    // The chain from RFC 8620 section 3.7, with mejl's call ids.
    #[test]
    fn rfc_back_reference_chain() {
        let mut req = RequestBuilder::new();
        let mut query = QueryArgs::<EmailFilterCondition>::new("A1");
        query.filter = Some(
            EmailFilterCondition {
                in_mailbox: Some("id_of_inbox".into()),
                ..Default::default()
            }
            .into(),
        );
        query.sort = Some(vec![Comparator::desc("receivedAt")]);
        query.position = Some(0);
        query.limit = Some(10);
        query.calculate_total = Some(true);
        let mut query = EmailQueryArgs::from(query);
        query.collapse_threads = Some(true);
        let q = req.call::<EmailQuery>(query);
        let g = req.call_with_refs::<EmailGet>(
            GetArgs::by_reference("A1").properties(["threadId"]).into(),
            [("ids", q.ids())],
        );
        let t =
            req.call_with_refs::<ThreadGet>(GetArgs::by_reference("A1"), [("ids", g.thread_ids())]);
        req.call_with_refs::<EmailGet>(
            GetArgs::by_reference("A1")
                .properties(["from", "receivedAt", "subject"])
                .into(),
            [("ids", t.email_ids())],
        );
        let request = req.build().unwrap();
        let expected = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "methodCalls": [
                ["Email/query", {
                    "accountId": "A1",
                    "filter": {"inMailbox": "id_of_inbox"},
                    "sort": [{"property": "receivedAt", "isAscending": false}],
                    "collapseThreads": true,
                    "position": 0,
                    "limit": 10,
                    "calculateTotal": true
                }, "c0"],
                ["Email/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c0", "name": "Email/query", "path": "/ids"},
                    "properties": ["threadId"]
                }, "c1"],
                ["Thread/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c1", "name": "Email/get", "path": "/list/*/threadId"}
                }, "c2"],
                ["Email/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c2", "name": "Thread/get", "path": "/list/*/emailIds"},
                    "properties": ["from", "receivedAt", "subject"]
                }, "c3"]
            ]
        });
        assert_eq!(serde_json::to_value(&request).unwrap(), expected);
    }

    #[test]
    fn reference_replaces_plain_argument() {
        let mut req = RequestBuilder::new();
        let c = req.call::<EmailChanges>(ChangesArgs::new("A1", "s1"));
        // Even if ids were set, the reference wins and ids is dropped.
        req.call_with_refs::<EmailGet>(
            GetArgs::new("A1", ["stale".into()]).into(),
            [("#ids", c.created())],
        );
        let request = req.build().unwrap();
        let args = request.method_calls[1].arguments.as_object().unwrap();
        assert!(!args.contains_key("ids"));
        assert_eq!(args["#ids"]["path"], "/created");
        assert_eq!(args["#ids"]["name"], "Email/changes");
    }

    #[test]
    fn search_snippet_reference() {
        let mut req = RequestBuilder::new();
        let q = req.call::<EmailQuery>(QueryArgs::new("A1").into());
        req.call_with_refs::<SearchSnippetGet>(
            SearchSnippetGetArgs {
                account_id: "A1".into(),
                filter: None,
                email_ids: None,
            },
            [("emailIds", q.ids())],
        );
        let request = req.build().unwrap();
        let args = request.method_calls[1].arguments.as_object().unwrap();
        assert!(!args.contains_key("emailIds"));
        assert!(args.contains_key("#emailIds"));
    }

    struct NotAnObject;
    impl Method for NotAnObject {
        const NAME: &'static str = "Test/bad";
        const CAPABILITIES: &'static [&'static str] = &[];
        type Args = Vec<u8>;
        type Response = Value;
    }

    #[test]
    fn non_object_args_fail_at_build() {
        let mut req = RequestBuilder::new();
        req.call::<NotAnObject>(vec![1, 2]);
        req.call::<CoreEcho>(Map::new());
        assert!(matches!(req.build(), Err(Error::Encode { .. })));
    }

    fn response(json: Value) -> Response {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn typed_results_and_errors() {
        let mut req = RequestBuilder::new();
        let m = req.call::<MaskedEmailGet>(GetArgs::all("u1"));
        let e = req.call::<EmailChanges>(ChangesArgs::new("u1", "old"));
        let x = req.call::<ThreadGet>(GetArgs::all("u1"));
        let resp = response(json!({
            "methodResponses": [
                ["MaskedEmail/get", {"accountId": "u1", "state": "", "list": [
                    {"id": "masked-1", "email": "a.b1@example.com", "state": "enabled"}
                ], "notFound": []}, "c0"],
                ["error", {"type": "cannotCalculateChanges"}, "c1"]
            ],
            "sessionState": "s1"
        }));
        let masks = resp.get(&m).unwrap();
        assert_eq!(masks.list.len(), 1);
        let err = resp.get(&e).unwrap_err();
        assert!(err.is_method_error(&MethodErrorType::CannotCalculateChanges));
        assert!(matches!(resp.get(&x), Err(Error::MissingResponse { .. })));
    }

    #[test]
    fn wrong_shape_is_a_decode_error() {
        let mut req = RequestBuilder::new();
        let t = req.call::<ThreadGet>(GetArgs::all("u1"));
        let resp = response(json!({
            "methodResponses": [["Thread/get", {"accountId": 5}, "c0"]],
            "sessionState": "s1"
        }));
        assert!(matches!(resp.get(&t), Err(Error::Decode { .. })));
    }

    #[test]
    fn malformed_error_object_still_reports_error() {
        let mut req = RequestBuilder::new();
        let t = req.call::<ThreadGet>(GetArgs::all("u1"));
        let resp = response(json!({
            "methodResponses": [["error", {"description": "no type"}, "c0"]],
            "sessionState": "s1"
        }));
        assert!(matches!(resp.get(&t), Err(Error::Method { .. })));
    }

    #[test]
    fn implicit_email_set_after_submission() {
        let mut req = RequestBuilder::new();
        req.call::<EmailSet>(SetArgs::<Email>::new("u1"));
        let s = req.call::<EmailSubmissionSet>(
            EmailSubmissionSetArgs::from(SetArgs::new("u1"))
                .on_success_update("s1", Patch::new().keyword("$draft", false)),
        );
        let resp = response(json!({
            "methodResponses": [
                ["Email/set", {"accountId": "u1", "created": {"d1": {"id": "E9"}}}, "c0"],
                ["EmailSubmission/set", {"accountId": "u1", "created": {"s1": {"id": "S1"}}}, "c1"],
                ["Email/set", {"accountId": "u1", "updated": {"E9": null}}, "c1"]
            ],
            "sessionState": "s1"
        }));
        let sub = resp.get(&s).unwrap();
        assert_eq!(sub.created["s1"].id, "S1");
        let implicit = resp.implicit_email_set(&s).unwrap().unwrap();
        assert!(implicit.updated.contains_key("E9"));

        let resp = response(json!({
            "methodResponses": [["EmailSubmission/set", {"accountId": "u1"}, "c1"]],
            "sessionState": "s1"
        }));
        assert!(resp.implicit_email_set(&s).is_none());
    }

    #[test]
    fn invocation_rejects_bad_shapes() {
        assert!(serde_json::from_value::<Invocation>(json!(["a", {}, "c"])).is_ok());
        assert!(serde_json::from_value::<Invocation>(json!(["a", [], "c"])).is_err());
        assert!(serde_json::from_value::<Invocation>(json!(["a", {}])).is_err());
        assert!(serde_json::from_value::<Invocation>(json!(["a", {}, "c", 1])).is_err());
        assert!(serde_json::from_value::<Invocation>(json!({"name": "a"})).is_err());
        assert!(serde_json::from_value::<Invocation>(json!([1, {}, "c"])).is_err());
    }

    #[test]
    fn invocation_error_constructor() {
        let inv = Invocation::error("c3", &MethodError::new(MethodErrorType::UnknownMethod));
        assert_eq!(
            serde_json::to_value(&inv).unwrap(),
            json!(["error", {"type": "unknownMethod"}, "c3"])
        );
    }

    #[test]
    fn resolve_references_like_a_server() {
        let responses: Vec<Invocation> = serde_json::from_value(json!([
            ["Email/get", {"list": [
                {"id": "E1", "threadId": "T1"},
                {"id": "E2", "threadId": "T2"}
            ]}, "t1"],
            ["Thread/get", {"list": [
                {"id": "T1", "emailIds": ["E1", "E3"]},
                {"id": "T2", "emailIds": ["E2"]}
            ]}, "t2"],
            ["Odd/get", {"a/b": {"~": [10, 20]}}, "t3"]
        ]))
        .unwrap();
        let reference = |of: &str, name: &str, path: &str| ResultReference {
            result_of: of.into(),
            name: name.into(),
            path: path.into(),
        };
        assert_eq!(
            reference("t1", "Email/get", "/list/*/threadId")
                .resolve(&responses)
                .unwrap(),
            json!(["T1", "T2"])
        );
        // Arrays from `*` are flattened.
        assert_eq!(
            reference("t2", "Thread/get", "/list/*/emailIds")
                .resolve(&responses)
                .unwrap(),
            json!(["E1", "E3", "E2"])
        );
        assert_eq!(
            reference("t3", "Odd/get", "/a~1b/~0/1")
                .resolve(&responses)
                .unwrap(),
            json!(20)
        );
        for (of, name, path) in [
            ("t9", "Email/get", "/list"),
            ("t1", "Thread/get", "/list"),
            ("t1", "Email/get", "/nope"),
            ("t1", "Email/get", "list"),
            ("t1", "Email/get", "/list/01"),
            ("t1", "Email/get", "/list/-1"),
            ("t1", "Email/get", "/list/*/missing"),
            ("t3", "Odd/get", "/a~2b"),
        ] {
            let err = reference(of, name, path).resolve(&responses).unwrap_err();
            assert_eq!(
                err.kind,
                MethodErrorType::InvalidResultReference,
                "{of} {path}"
            );
        }
    }

    #[test]
    fn request_round_trips() {
        let mut req = RequestBuilder::new();
        req.call::<MaskedEmailSet>(
            SetArgs::new("u1").create("k1", MaskedEmail::new_enabled("Shop")),
        );
        req.call::<ThreadGet>(GetArgs::<Thread>::new("u1", ["T1".into()]));
        let request = req.build().unwrap();
        let text = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&text).unwrap();
        assert_eq!(back, request);
    }
}
