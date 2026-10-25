//! Round trips and builder checks against JSON modelled on the examples in
//! RFC 8620, RFC 8621 and RFC 9610 and on the request and response examples
//! in the Fastmail research docs. The RFC fixtures were transcribed, not
//! copied byte for byte, and a few objects (a full Email, contact cards, an
//! upload response) were written to cover every property. Hosts and
//! addresses in the fixtures are on example domains only.

use std::path::Path;

use mejl_jmap::capability::{CORE, MAIL, MASKED_EMAIL};
use mejl_jmap::filter::{Filter, Operator};
use mejl_jmap::method::{
    ChangesArgs, ChangesResponse, GetArgs, GetResponse, QueryChangesArgs, QueryChangesResponse,
    QueryResponse, SetArgs, SetResponse,
};
use mejl_jmap::methods::*;
use mejl_jmap::push::parse_event;
use mejl_jmap::sse::SseEvent;
use mejl_jmap::types::*;
use mejl_jmap::{
    Comparator, EmailFilterCondition, Invocation, Patch, PushEvent, Request, RequestBuilder,
    RequestError, RequestErrorType, Response, ResultReference, Session, SetErrorType,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn load(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn arguments(invocation: &Value) -> Value {
    invocation[1].clone()
}

/// JSON equality where a missing member equals `null` or a default
/// (`false`, `""`, `[]`, `{}`), because the encoder leaves optional and
/// default values out. Present, non-default values must match exactly.
fn equivalent(a: &Value, b: &Value) -> bool {
    fn absent_like(v: &Value) -> bool {
        match v {
            Value::Null => true,
            Value::Bool(b) => !b,
            Value::String(s) => s.is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            Value::Number(_) => false,
        }
    }
    if absent_like(a) && absent_like(b) {
        return true;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.keys().chain(y.keys()).all(|k| {
            equivalent(
                x.get(k).unwrap_or(&Value::Null),
                y.get(k).unwrap_or(&Value::Null),
            )
        }),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equivalent(a, b))
        }
        _ => a == b,
    }
}

/// Decode into `T`, encode again, and require an equivalent document.
fn round_trip<T: Serialize + DeserializeOwned>(value: &Value) -> T {
    let typed: T = serde_json::from_value(value.clone()).expect("decodes");
    let back = serde_json::to_value(&typed).expect("encodes");
    assert!(
        equivalent(value, &back),
        "round trip changed the document\noriginal: {value:#}\nafter:    {back:#}"
    );
    // Encoding is stable: a second pass gives the same document.
    let again: T = serde_json::from_value(back.clone()).expect("decodes again");
    assert_eq!(serde_json::to_value(&again).expect("encodes again"), back);
    typed
}

#[test]
fn rfc8620_session() {
    let session: Session = round_trip(&load("rfc8620_session.json"));
    assert_eq!(session.username, "john@example.com");
    assert_eq!(session.state, "75128aab4b1b");
    assert_eq!(
        session.primary_account(MAIL).map(Id::as_str),
        Some("A13824")
    );
    assert!(session.has_capability("https://example.com/apis/foobar"));
    assert!(session.account("A97813").unwrap().is_read_only);
    assert!(session.account_has_capability("A13824", MAIL));
    assert!(!session.account_has_capability("A97813", "urn:ietf:params:jmap:contacts"));

    let core = session.core_capability().unwrap();
    // The fixture spells this limit "maxConcurrentRequest", as the RFC
    // example is recalled to. The odd key is kept and the real limit
    // falls back.
    assert!(core.extra.contains_key("maxConcurrentRequest"));
    let limits = session.core_limits();
    assert_eq!(limits.max_calls_in_request, 32);
    assert_eq!(limits.max_objects_in_get, 256);
    assert_eq!(limits.max_objects_in_set, 128);
    assert_eq!(limits.max_concurrent_requests, 4);

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MailCaps {
        max_mailbox_depth: Option<u32>,
    }
    let caps: MailCaps = session.account_capability_as("A13824", MAIL).unwrap();
    assert_eq!(caps.max_mailbox_depth, Some(10));
}

#[test]
fn glob_import_of_types_does_not_shadow_core() {
    // `types::*` is imported above; the real `core` crate must still resolve.
    assert_eq!(core::mem::size_of::<u8>(), 1);
}

#[test]
fn rfc8620_request_and_response_objects() {
    let request = load("rfc8620_request.json");
    let typed: Request = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(typed.method_calls.len(), 3);
    assert_eq!(serde_json::to_value(&typed).unwrap(), request);

    let response = load("rfc8620_response.json");
    let typed: Response = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), response);
    // A second response under the same call id is reachable by name.
    let extra: Value = typed.get_as("c2", "anotherResponseFromMethod2").unwrap();
    assert_eq!(extra["data"], 10);
    let err = typed.get_as::<Value>("c3", "method3").unwrap_err();
    assert!(err.is_method_error(&mejl_jmap::MethodErrorType::UnknownMethod));
}

#[test]
fn rfc8620_back_reference() {
    let request: Request = serde_json::from_value(load("rfc8620_back_reference.json")).unwrap();
    let args = request.method_calls[1].arguments.as_object().unwrap();
    let reference: ResultReference = serde_json::from_value(args["#ids"].clone()).unwrap();
    assert_eq!(reference.result_of, "t0");
    assert_eq!(reference.name, "Foo/changes");
    assert_eq!(reference.path, "/created");
}

#[test]
fn rfc8620_problem_details() {
    let body = serde_json::to_vec(&load("rfc8620_problem_unknown_capability.json")).unwrap();
    let problem = RequestError::from_body(&body).unwrap();
    assert_eq!(problem.kind, RequestErrorType::UnknownCapability);
    assert_eq!(problem.status, Some(400));

    let body = serde_json::to_vec(&load("rfc8620_problem_limit.json")).unwrap();
    let problem = RequestError::from_body(&body).unwrap();
    assert_eq!(problem.kind, RequestErrorType::Limit);
    assert_eq!(problem.limit.as_deref(), Some("maxSizeRequest"));
}

#[test]
fn rfc8620_state_change_event() {
    let event = SseEvent {
        event: "state".into(),
        data: load("rfc8620_state_change.json").to_string(),
        id: None,
    };
    let Some(PushEvent::StateChange { changed }) = parse_event(&event) else {
        panic!("expected a state change");
    };
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[&Id::from("a3123")]["EmailDelivery"], "428d565f2440");
    assert_eq!(changed[&Id::from("a43461d")]["Mailbox"], "0af7a512ce70");
}

#[test]
fn rfc8620_upload_response() {
    let up: mejl_jmap::UploadResponse = round_trip(&load("rfc8620_upload_response.json"));
    assert_eq!(up.size, 85724);
    assert_eq!(up.content_type, "image/jpeg");
}

#[test]
fn rfc8621_mailbox_get() {
    let inv = load("rfc8621_mailbox_get_response.json");
    let resp: GetResponse<Mailbox> = round_trip(&arguments(&inv));
    assert_eq!(resp.state, "78540");
    let inbox = &resp.list[0];
    assert_eq!(inbox.role, Some(Role::Inbox));
    assert_eq!(inbox.unread_emails, Some(13905));
    assert!(inbox.parent_id.is_none());
    let rights = inbox.my_rights.as_ref().unwrap();
    assert!(rights.may_submit && !rights.may_rename);
    assert_eq!(resp.list[1].role, None);
}

#[test]
fn rfc8621_email_get_request() {
    let inv = load("rfc8621_email_get_request.json");
    let args: EmailGetArgs = round_trip(&arguments(&inv));
    assert_eq!(args.fetch_html_body_values, Some(true));
    assert_eq!(args.max_body_value_bytes, Some(256));
    assert_eq!(args.get.ids.as_ref().map(Vec::len), Some(2));
    let encoded = serde_json::to_value(&args).unwrap();
    assert!(encoded.get("fetchHTMLBodyValues").is_some());
}

#[test]
fn rfc8621_email_get_response() {
    let inv = load("rfc8621_email_get_response.json");
    let resp: GetResponse<Email> = round_trip(&arguments(&inv));
    assert_eq!(resp.not_found, vec![Id::from("f123u456")]);
    let email = &resp.list[0];
    assert_eq!(email.subject.as_deref(), Some("Dinner on Thursday?"));
    assert!(email.in_mailbox("f123"));
    let urls = email
        .header_property("List-Post", HeaderForm::Urls, false)
        .unwrap();
    assert_eq!(urls[0], "mailto:partytime@lists.example.com");
    let html = email.html_body.as_ref().unwrap();
    assert_eq!(html[0].content_type.as_deref(), Some("text/html"));
    let values = email.body_values.as_ref().unwrap();
    assert!(values["1"].is_truncated);
    assert!(!values["2"].is_truncated);
    assert_eq!(
        email.received_at.map(|t| t.to_string()).as_deref(),
        Some("2013-10-13T14:12:00Z")
    );
}

#[test]
fn rfc8621_email_all_properties() {
    let email: Email = round_trip(&load("rfc8621_email_full.json"));
    assert_eq!(
        email.header_text("x-fm-masked-email"),
        Some("masked-123456")
    );
    assert_eq!(email.header_text("Fastmail-MaskedEmail"), None);
    assert!(email.has_keyword(keyword::FLAGGED));
    assert_eq!(email.sent_at.as_deref(), Some("2014-10-30T14:12:00+08:00"));
    assert_eq!(email.sent_at_timestamp(), email.received_at);
    let structure = email.body_structure.as_ref().unwrap();
    assert_eq!(structure.sub_parts.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        email.attachments.as_ref().unwrap()[0].name.as_deref(),
        Some("beach.jpg")
    );
    assert_eq!(email.to.as_ref().unwrap()[0].name, None);
    assert!(email.extra.contains_key("x-future-property"));
}

#[test]
fn rfc8621_thread_get() {
    let inv = load("rfc8621_thread_get_response.json");
    let resp: GetResponse<Thread> = round_trip(&arguments(&inv));
    assert_eq!(
        resp.list[0].email_ids,
        vec![Id::from("eaa623"), Id::from("f782cbb")]
    );
}

#[test]
fn rfc8621_email_query_request() {
    let inv = load("rfc8621_email_query_request.json");
    let args: EmailQueryArgs = round_trip(&arguments(&inv));
    assert_eq!(args.collapse_threads, Some(true));
    let Some(Filter::Operator(and)) = &args.query.filter else {
        panic!("expected AND");
    };
    assert_eq!(and.operator, Operator::And);
    assert_eq!(and.conditions.len(), 4);
    let Filter::Operator(not) = &and.conditions[2] else {
        panic!("expected NOT");
    };
    assert_eq!(not.operator, Operator::Not);
    let Filter::Condition(header) = &not.conditions[1] else {
        panic!("expected a condition");
    };
    assert_eq!(header.header.as_deref(), Some(&["List-Id".to_string()][..]));
    let sort = args.query.sort.as_ref().unwrap();
    assert_eq!(sort[1].keyword.as_deref(), Some("$flagged"));
}

#[test]
fn rfc8621_changes_query_and_query_changes_responses() {
    let changes: ChangesResponse<Email> =
        round_trip(&arguments(&load("rfc8621_email_changes_response.json")));
    assert!(changes.has_more_changes);
    assert_eq!(changes.created.len(), 2);

    let query: QueryResponse = round_trip(&arguments(&load("rfc8621_email_query_response.json")));
    assert!(query.can_calculate_changes);
    assert_eq!(query.total, Some(115));

    let qc: QueryChangesResponse = round_trip(&arguments(&load(
        "rfc8621_email_query_changes_response.json",
    )));
    assert_eq!(qc.removed.len(), 2);
    assert_eq!(qc.added[1].index, 2);
}

#[test]
fn rfc8621_submission_request() {
    let request: Request = serde_json::from_value(load("rfc8621_submission_request.json")).unwrap();
    let create: SetArgs<Email> = round_trip(&request.method_calls[0].arguments);
    let draft = &create.create.as_ref().unwrap()["k1490"];
    assert!(draft.has_keyword(keyword::DRAFT));
    let submit: EmailSubmissionSetArgs = round_trip(&request.method_calls[1].arguments);
    let sub = &submit.set.create.as_ref().unwrap()["k1491"];
    assert_eq!(sub.email_id.as_ref().map(Id::as_str), Some("#k1490"));
    let envelope = sub.envelope.as_ref().unwrap();
    assert_eq!(envelope.rcpt_to.len(), 2);
    let params = envelope.rcpt_to[1].parameters.as_ref().unwrap();
    assert_eq!(params["NOTIFY"].as_deref(), Some("SUCCESS"));
    assert_eq!(params["SMTPUTF8"], None);
    // Patch nulls must survive exactly, not just equivalently.
    let patch = &submit.on_success_update_email.as_ref().unwrap()["#k1491"];
    assert_eq!(patch.as_map()["keywords/$draft"], Value::Null);
    assert_eq!(patch.as_map().len(), 3);
    let encoded = serde_json::to_value(&submit).unwrap();
    assert_eq!(
        encoded["onSuccessUpdateEmail"],
        request.method_calls[1].arguments["onSuccessUpdateEmail"]
    );
}

#[test]
fn rfc8621_search_snippets() {
    let inv = load("rfc8621_search_snippet_response.json");
    let resp: SearchSnippetGetResponse = serde_json::from_value(arguments(&inv)).unwrap();
    assert!(resp.not_found.is_empty(), "null notFound becomes empty");
    assert_eq!(resp.list.len(), 3);
    assert_eq!(resp.list[0].subject, None);
    assert!(
        resp.list[1]
            .subject
            .as_deref()
            .unwrap()
            .contains("<mark>Foo</mark>")
    );
    let back = serde_json::to_value(&resp.list).unwrap();
    assert!(equivalent(&back, &arguments(&inv)["list"]));
}

#[test]
fn rfc8621_identity_get() {
    let inv = load("rfc8621_identity_get_response.json");
    let resp: GetResponse<Identity> = round_trip(&arguments(&inv));
    let joe = &resp.list[0];
    assert_eq!(
        joe.bcc.as_ref().unwrap()[0].email,
        "joe+archive@example.com"
    );
    assert_eq!(joe.may_delete, Some(false));
    assert!(resp.list[1].allows("anyone@example.com"));
    assert!(!joe.allows("other@example.com"));
}

#[test]
fn rfc9610_contact_cards() {
    let inv = load("rfc9610_contact_card_get_response.json");
    let resp: GetResponse<ContactCard> = round_trip(&arguments(&inv));
    let robin = &resp.list[0];
    assert_eq!(robin.display_name().as_deref(), Some("Robin Example"));
    assert_eq!(robin.email_addresses()[0], "robin@example.org");
    assert_eq!(robin.extra["@type"], "Card");
    assert!(robin.extra.contains_key("phones"));
    let group = &resp.list[1];
    assert!(group.is_group());
    assert_eq!(group.display_name().as_deref(), Some("Book club"));
    assert_eq!(group.members.as_ref().map(|m| m.len()), Some(1));
}

#[test]
fn masked_email_session() {
    let session: Session = round_trip(&load("masked_email_session.json"));
    assert_eq!(
        session.primary_account(MASKED_EMAIL).map(Id::as_str),
        Some("u1234abcd")
    );
    assert_eq!(session.core_limits().max_objects_in_get, 4096);
    assert_eq!(session.core_limits().max_calls_in_request, 50);
}

#[test]
fn masked_email_get() {
    let expected = load("masked_email_get_request.json");
    let mut req = RequestBuilder::new();
    req.call::<MaskedEmailGet>(GetArgs::all("u1234abcd"));
    let built = serde_json::to_value(req.build().unwrap()).unwrap();
    assert_eq!(built["using"], expected["using"]);
    assert_eq!(built["methodCalls"][0][0], expected["methodCalls"][0][0]);
    assert_eq!(built["methodCalls"][0][1], expected["methodCalls"][0][1]);

    let resp: Response = serde_json::from_value(load("masked_email_get_response.json")).unwrap();
    let list: GetResponse<MaskedEmail> = resp.get_as("g0", "MaskedEmail/get").unwrap();
    assert!(list.state.is_empty());
    assert_eq!(list.list[0].state, Some(MaskedEmailState::Enabled));
    assert_eq!(list.list[1].state, Some(MaskedEmailState::Deleted));
    assert_eq!(list.list[1].for_domain.as_deref(), Some(""));
    assert!(list.list[0].last_message_at.is_some());
    round_trip::<GetResponse<MaskedEmail>>(&resp.method_responses[0].arguments);
}

#[test]
fn masked_email_create() {
    let expected = load("masked_email_create_request.json");
    let mut mask = MaskedEmail::new_enabled("Shop example newsletter");
    mask.for_domain = Some("https://shop.example".into());
    mask.email_prefix = Some("examplenews".into());
    let mut req = RequestBuilder::new();
    req.call::<MaskedEmailSet>(SetArgs::new("u1234abcd").create("k1", mask));
    let built = serde_json::to_value(req.build().unwrap()).unwrap();
    assert_eq!(built["using"], expected["using"]);
    assert!(equivalent(
        &built["methodCalls"][0][1],
        &expected["methodCalls"][0][1]
    ));

    let resp: Response = serde_json::from_value(load("masked_email_create_response.json")).unwrap();
    let set: SetResponse<MaskedEmail> = resp.get_as("s0", "MaskedEmail/set").unwrap();
    assert!(set.is_complete_success());
    assert!(set.new_state.is_none());
    let created = &set.created["k1"];
    assert_eq!(created.id, "masked-777777");
    assert_eq!(
        created.email.as_deref(),
        Some("examplenews.qz4ty@example.com")
    );
    assert!(created.state.is_none());

    let limited: SetResponse<MaskedEmail> =
        serde_json::from_value(arguments(&load("masked_email_rate_limited.json"))).unwrap();
    assert!(limited.created.is_empty());
    assert_eq!(limited.not_created["k1"].kind, SetErrorType::RateLimit);
}

#[test]
fn masked_email_update_and_destroy() {
    let expected = load("masked_email_update_request.json");
    let mut req = RequestBuilder::new();
    req.call::<MaskedEmailSet>(
        SetArgs::new("u1234abcd")
            .update("masked-123456", Patch::new().property("state", "disabled"))
            .update(
                "masked-777777",
                Patch::new().property("description", "Shop example (orders only)"),
            ),
    );
    let built = serde_json::to_value(req.build().unwrap()).unwrap();
    assert_eq!(built["methodCalls"][0][1], expected["methodCalls"][0][1]);

    let updated: SetResponse<MaskedEmail> =
        round_trip(&arguments(&load("masked_email_update_response.json")));
    assert_eq!(updated.updated.len(), 2);
    assert!(updated.updated["masked-123456"].is_none());

    let failed: SetResponse<MaskedEmail> =
        serde_json::from_value(arguments(&load("masked_email_destroy_failure.json"))).unwrap();
    let err = &failed.not_destroyed["masked-123456"];
    assert_eq!(err.kind, SetErrorType::Forbidden);
    assert_eq!(err.sub_type(), Some("addressInUse"));
}

/// Rename the builder's call ids so a built request can be compared with a
/// fixture that uses its own ids.
fn rename_call_ids(request: &mut Value, ids: &[&str]) {
    let rename = |id: &str| -> String {
        id.strip_prefix('c')
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| ids.get(n))
            .map(|s| (*s).to_owned())
            .unwrap_or_else(|| id.to_owned())
    };
    for call in request["methodCalls"].as_array_mut().unwrap() {
        let id = rename(call[2].as_str().unwrap());
        call[2] = Value::String(id);
        for (key, value) in call[1].as_object_mut().unwrap() {
            if key.starts_with('#') {
                let of = rename(value["resultOf"].as_str().unwrap());
                value["resultOf"] = Value::String(of);
            }
        }
    }
}

#[test]
fn fastmail_sync_request_from_builder() {
    let mut req = RequestBuilder::new();
    req.call::<MailboxChanges>(ChangesArgs::new("u1", "M1"));
    let e = req.call::<EmailChanges>(ChangesArgs::new("u1", "E1").max_changes(500));
    req.call_with_refs::<EmailGet>(
        GetArgs::by_reference("u1")
            .properties([
                "threadId",
                "mailboxIds",
                "keywords",
                "from",
                "subject",
                "receivedAt",
                "preview",
            ])
            .into(),
        [("ids", e.created())],
    );
    req.call_with_refs::<EmailGet>(
        GetArgs::by_reference("u1")
            .properties(["mailboxIds", "keywords"])
            .into(),
        [("ids", e.updated())],
    );
    req.call::<ThreadChanges>(ChangesArgs::new("u1", "T1"));
    let mut qc = QueryChangesArgs::<EmailFilterCondition>::new("u1", "Q1");
    qc.filter = Some(
        EmailFilterCondition {
            in_mailbox: Some("inbox-id".into()),
            ..Default::default()
        }
        .into(),
    );
    qc.sort = Some(vec![Comparator::desc("receivedAt")]);
    let mut qc = EmailQueryChangesArgs::from(qc);
    qc.collapse_threads = Some(true);
    req.call::<EmailQueryChanges>(qc);

    let mut built = serde_json::to_value(req.build().unwrap()).unwrap();
    rename_call_ids(&mut built, &["m", "e", "ec", "eu", "t", "q"]);
    assert_eq!(built, load("fastmail_sync_request.json"));
}

#[test]
fn fastmail_send_request_from_builder() {
    let draft = Email {
        mailbox_ids: Some([(Id::from("drafts-id"), true)].into()),
        keywords: Some(
            [
                (keyword::DRAFT.to_owned(), true),
                (keyword::SEEN.to_owned(), true),
            ]
            .into(),
        ),
        from: Some(vec![EmailAddress::with_name("Me", "me@example.com")]),
        to: Some(vec![EmailAddress::new("you@example.org")]),
        subject: Some("Hello".into()),
        body_values: Some([("t".to_owned(), EmailBodyValue::new("Hi there"))].into()),
        text_body: Some(vec![EmailBodyPart {
            part_id: Some("t".into()),
            content_type: Some("text/plain".into()),
            ..Default::default()
        }]),
        ..Default::default()
    };
    let submission = EmailSubmission {
        identity_id: Some("identity-id".into()),
        email_id: Some("#d1".into()),
        ..Default::default()
    };
    let mut req = RequestBuilder::new();
    req.call::<EmailSet>(SetArgs::new("u1").create("d1", draft));
    req.call::<EmailSubmissionSet>(
        EmailSubmissionSetArgs::from(SetArgs::new("u1").create("s1", submission))
            .on_success_update(
                "s1",
                Patch::new()
                    .mailbox("drafts-id", false)
                    .mailbox("sent-id", true)
                    .keyword(keyword::DRAFT, false),
            ),
    );
    let request = req.build().unwrap();
    assert_eq!(
        request.using,
        vec![CORE, MAIL, "urn:ietf:params:jmap:submission"]
    );
    let mut built = serde_json::to_value(&request).unwrap();
    rename_call_ids(&mut built, &["0", "1"]);
    let expected = load("fastmail_send_request.json");
    assert!(
        equivalent(&built, &expected),
        "built: {built:#}\nexpected: {expected:#}"
    );
    // The patch nulls are real nulls, not omissions.
    assert_eq!(
        built["methodCalls"][1][1]["onSuccessUpdateEmail"],
        expected["methodCalls"][1][1]["onSuccessUpdateEmail"]
    );
}

#[test]
fn fastmail_connect_event() {
    let event = SseEvent {
        event: "message".into(),
        data: load("fastmail_connect_event.json").to_string(),
        id: None,
    };
    let Some(PushEvent::StateChange { changed }) = parse_event(&event) else {
        panic!("expected a state change");
    };
    assert_eq!(changed[&Id::from("u1234abcd")]["EmailDelivery"], "J7891");
}

#[test]
fn invocations_in_fixtures_parse() {
    for name in [
        "rfc8621_mailbox_get_response.json",
        "rfc8621_email_get_response.json",
        "masked_email_rate_limited.json",
    ] {
        let inv: Invocation = serde_json::from_value(load(name)).unwrap();
        assert!(!inv.call_id.is_empty(), "{name}");
    }
}

#[test]
fn set_args_from_fixture_deserialize() {
    // `mejl-fake` decodes requests with these same types.
    let request: Request =
        serde_json::from_value(load("masked_email_update_request.json")).unwrap();
    let args: SetArgs<MaskedEmail> = round_trip(&request.method_calls[0].arguments);
    assert_eq!(args.update.as_ref().map(|u| u.len()), Some(2));
    let get: Request = serde_json::from_value(load("masked_email_get_request.json")).unwrap();
    let args: GetArgs<MaskedEmail> =
        serde_json::from_value(get.method_calls[0].arguments.clone()).unwrap();
    assert!(args.ids.is_none());
    let resp = ChangesResponse::<Mailbox>::new("u1", "a", "b");
    assert_eq!(serde_json::to_value(&resp).unwrap()["newState"], json!("b"));
}

/// Fixtures must never hold a real address: only example domains.
#[test]
fn fixture_addresses_use_example_domains() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let allowed = ["example.com", "example.org", "example.net"];
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let bytes = text.as_bytes();
        for (at, _) in text.match_indices('@') {
            let local_ok = at > 0 && {
                let b = bytes[at - 1];
                b.is_ascii_alphanumeric() || b"._%+-".contains(&b)
            };
            if !local_ok {
                continue; // "@type" and similar keys
            }
            let domain: String = text[at + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
                .collect();
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            assert!(
                allowed
                    .iter()
                    .any(|a| domain == *a || domain.ends_with(&format!(".{a}"))),
                "{}: address at {domain:?} is not on an example domain",
                path.display()
            );
        }
        for host in ["fastmail.com", "fastmailusercontent.com"] {
            for (i, _) in text.match_indices(host) {
                // Capability URIs are identifiers, not hosts to contact.
                let before = &text[..i];
                assert!(
                    before.ends_with("https://www.") && text[i..].starts_with("fastmail.com/dev/"),
                    "{}: unexpected {host} reference",
                    path.display()
                );
            }
        }
    }
}
