//! End to end over real HTTP: a small axum server on 127.0.0.1 and the
//! production `HttpTransport`. Nothing here leaves the loopback interface.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use futures::StreamExt;
use mejl_jmap::method::{ChangesArgs, GetArgs};
use mejl_jmap::methods::{EmailChanges, MailboxGet};
use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport, TransportError};
use mejl_jmap::types::Mailbox;
use mejl_jmap::{
    Client, Error, HttpConfig, HttpTransport, Id, MethodErrorType, PushEvent, PushOptions, Request,
    RequestBuilder, Secret,
};

const TOKEN: &str = "test-token-not-real";

struct App {
    base: String,
    api_calls: AtomicUsize,
    seen: Mutex<Vec<HeaderMap>>,
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"test\"")],
    )
        .into_response()
}

fn json(body: serde_json::Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn session(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return unauthorized();
    }
    let base = &app.base;
    json(serde_json::json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {"maxSizeUpload": 1000, "maxCallsInRequest": 16},
            "urn:ietf:params:jmap:mail": {}
        },
        "accounts": {"A1": {"name": "robin@example.com", "isPersonal": true,
            "isReadOnly": false, "accountCapabilities": {"urn:ietf:params:jmap:mail": {}}}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
        "username": "robin@example.com",
        "apiUrl": format!("{base}/jmap/api/"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}/"),
        // Like Fastmail: no template variables.
        "eventSourceUrl": format!("{base}/jmap/event/"),
        "state": "session-1"
    }))
}

async fn api(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers) {
        return unauthorized();
    }
    app.seen.lock().unwrap().push(headers.clone());
    match app.api_calls.fetch_add(1, Ordering::SeqCst) {
        0 => {
            let request: Request = match serde_json::from_slice(&body) {
                Ok(r) => r,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            assert_eq!(request.using[0], "urn:ietf:params:jmap:core");
            json(serde_json::json!({
                "methodResponses": [
                    ["Mailbox/get", {"accountId": "A1", "state": "m7", "list": [
                        {"id": "M1", "name": "Inbox", "role": "inbox", "sortOrder": 1}
                    ], "notFound": []}, request.method_calls[0].call_id],
                    ["error", {"type": "cannotCalculateChanges"}, request.method_calls[1].call_id]
                ],
                "sessionState": "session-2"
            }))
        }
        1 => (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "2")]).into_response(),
        2 => {
            let at = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(120);
            let date = jiff::fmt::rfc2822::DateTimePrinter::new()
                .timestamp_to_rfc9110_string(&at)
                .unwrap();
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, date)],
            )
                .into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/problem+json")],
            r#"{"type":"urn:ietf:params:jmap:error:notRequest","status":400}"#,
        )
            .into_response(),
    }
}

async fn upload(Path(account): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers) {
        return unauthorized();
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({"accountId": account, "blobId": "B42", "type": content_type, "size": body.len()})
            .to_string(),
    )
        .into_response()
}

async fn download(
    Path((account, blob, name)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    if !authorized(&headers) {
        return unauthorized();
    }
    format!("{account}|{blob}|{name}|{}", query.unwrap_or_default()).into_response()
}

/// Chunks are cut mid-line and between CR and LF on purpose.
const EVENT_CHUNKS: &[&str] = &[
    ": welcome\r\n\r\nevent: state\r\ndata: {\"@type\":\"StateChange\",\"changed\":",
    "{\"A1\":{\"Email\":\"e2\",\"Mailbox\":\"m8\"}}}\r",
    "\nid: 1\r\n\r\n",
    "event: unknown\ndata: {\"whatever\":true}\n\n",
    "data: {\"type\":\"connect\",\"changed\":{\"A1\":{\"EmailDelivery\":\"d1\"}}}\n",
    "\nevent: ping\ndata: {\"interval\":30}\n\n",
    "event: state\ndata: not json at all\n\n",
];

async fn events(RawQuery(query): RawQuery, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return unauthorized();
    }
    if query.as_deref() != Some("types=*&closeafter=no&ping=30") {
        return (StatusCode::BAD_REQUEST, format!("bad query {query:?}")).into_response();
    }
    if headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) != Some("text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let stream = futures::stream::iter(EVENT_CHUNKS.iter().copied()).then(|chunk| async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok::<_, std::io::Error>(Bytes::from_static(chunk.as_bytes()))
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn slow() -> Response {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "late".into_response()
}

async fn slow_stream() -> Response {
    let stream = futures::stream::once(async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok::<_, std::io::Error>(Bytes::from_static(b"data: late\n\n"))
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

struct Servers {
    base: String,
    other_hits: Arc<Mutex<Vec<HeaderMap>>>,
    app: Arc<App>,
}

async fn start() -> Servers {
    // A second server on another port stands in for a foreign host.
    let other_hits: Arc<Mutex<Vec<HeaderMap>>> = Arc::default();
    let hits = other_hits.clone();
    let other = Router::new().route(
        "/{*rest}",
        any(move |headers: HeaderMap| {
            let hits = hits.clone();
            async move {
                hits.lock().unwrap().push(headers);
                "stolen"
            }
        }),
    );
    let other_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_base = format!("http://{}", other_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(other_listener, other).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Arc::new(App {
        base: base.clone(),
        api_calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    });
    let away = format!("{other_base}/steal");
    let router = Router::new()
        .route("/jmap/session", get(session))
        .route(
            "/.well-known/jmap",
            get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/jmap/session")]) }),
        )
        .route(
            "/redirect-away",
            get(move || {
                let away = away.clone();
                async move { (StatusCode::FOUND, [(header::LOCATION, away)]) }
            }),
        )
        .route("/jmap/api/", post(api))
        .route("/jmap/upload/{account}/", post(upload))
        .route("/jmap/download/{account}/{blob}/{name}", get(download))
        .route("/jmap/event/", get(events))
        .route("/slow", get(slow))
        .route("/slow-stream", get(slow_stream))
        .with_state(app.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Servers {
        base,
        other_hits,
        app,
    }
}

fn client(base: &str, path: &str, token: &str) -> Client {
    let transport = HttpTransport::new().unwrap();
    Client::new(
        Arc::new(transport),
        format!("{base}{path}"),
        Secret::new(token),
    )
}

#[tokio::test]
async fn session_api_call_and_backoff() {
    let servers = start().await;
    let client = client(&servers.base, "/jmap/session", TOKEN);
    let session = client.fetch_session().await.unwrap();
    assert_eq!(session.state, "session-1");
    assert_eq!(session.core_limits().max_calls_in_request, 16);

    let mut req = RequestBuilder::new();
    let mailboxes = req.call::<MailboxGet>(GetArgs::<Mailbox>::all("A1"));
    let changes = req.call::<EmailChanges>(ChangesArgs::new("A1", "old"));
    let request = req.build().unwrap();

    let resp = client.call(&request).await.unwrap();
    assert!(resp.session_changed, "server moved to session-2");
    let list = resp.get(&mailboxes).unwrap();
    assert_eq!(list.list[0].name.as_deref(), Some("Inbox"));
    let err = resp.get(&changes).unwrap_err();
    assert!(err.is_method_error(&MethodErrorType::CannotCalculateChanges));

    let seen = servers.app.seen.lock().unwrap()[0].clone();
    let ua = seen.get(header::USER_AGENT).unwrap().to_str().unwrap();
    assert_eq!(ua, mejl_jmap::USER_AGENT);
    assert!(ua.starts_with("mejl/") && !ua.contains('@'));
    let encodings = seen.get(header::ACCEPT_ENCODING).unwrap().to_str().unwrap();
    assert!(encodings.contains("gzip"), "{encodings}");

    match client.call(&request).await {
        Err(Error::RateLimited { retry_after }) => {
            assert_eq!(retry_after, Some(Duration::from_secs(2)));
        }
        other => panic!("expected 429, got {other:?}"),
    }
    match client.call(&request).await {
        Err(Error::ServiceUnavailable { retry_after }) => {
            let wait = retry_after.unwrap();
            assert!(
                wait > Duration::from_secs(100) && wait <= Duration::from_secs(120),
                "{wait:?}"
            );
        }
        other => panic!("expected 503, got {other:?}"),
    }
    match client.call(&request).await {
        Err(Error::Request(problem)) => {
            assert_eq!(problem.kind, mejl_jmap::RequestErrorType::NotRequest);
        }
        other => panic!("expected a problem response, got {other:?}"),
    }
}

#[tokio::test]
async fn wrong_token_is_unauthorized() {
    let servers = start().await;
    let client = client(&servers.base, "/jmap/session", "wrong");
    match client.fetch_session().await {
        Err(Error::Unauthorized {
            status: 401,
            www_authenticate,
        }) => {
            assert_eq!(www_authenticate.as_deref(), Some("Bearer realm=\"test\""));
        }
        other => panic!("expected 401, got {other:?}"),
    }
}

#[tokio::test]
async fn push_stream_with_both_event_shapes_and_ping() {
    let servers = start().await;
    let client = client(&servers.base, "/jmap/session", TOKEN);
    client.fetch_session().await.unwrap();
    let options = PushOptions {
        ping: 30,
        ..Default::default()
    };
    let stream = client.push(&options).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    let events: Vec<PushEvent> = events.into_iter().map(Result::unwrap).collect();
    assert_eq!(events.len(), 3, "{events:?}");
    let PushEvent::StateChange { changed } = &events[0] else {
        panic!("{:?}", events[0]);
    };
    assert_eq!(changed[&Id::from("A1")]["Email"], "e2");
    assert_eq!(changed[&Id::from("A1")]["Mailbox"], "m8");
    let PushEvent::StateChange { changed } = &events[1] else {
        panic!("{:?}", events[1]);
    };
    assert_eq!(changed[&Id::from("A1")]["EmailDelivery"], "d1");
    assert_eq!(events[2], PushEvent::Ping { interval: Some(30) });
}

#[tokio::test]
async fn same_origin_redirect_followed_foreign_one_not() {
    let servers = start().await;
    let client1 = client(&servers.base, "/.well-known/jmap", TOKEN);
    let session = client1.fetch_session().await.unwrap();
    assert_eq!(session.username, "robin@example.com");

    let client2 = client(&servers.base, "/redirect-away", TOKEN);
    match client2.fetch_session().await {
        Err(Error::Redirect {
            status: 302,
            location,
        }) => {
            assert!(location.unwrap().ends_with("/steal"));
        }
        other => panic!("expected an unfollowed redirect, got {other:?}"),
    }
    let hits = servers.other_hits.lock().unwrap();
    assert!(hits.is_empty(), "the foreign host was contacted");
}

#[tokio::test]
async fn upload_and_download() {
    let servers = start().await;
    let client = client(&servers.base, "/jmap/session", TOKEN);
    client.fetch_session().await.unwrap();
    let up = client
        .upload(
            &Id::from("A1"),
            "text/plain",
            Bytes::from_static(b"hello world"),
        )
        .await
        .unwrap();
    assert_eq!(up.blob_id, "B42");
    assert_eq!(up.size, 11);
    assert_eq!(up.content_type, "text/plain");

    let too_big = client
        .upload(&Id::from("A1"), "text/plain", Bytes::from(vec![0u8; 1001]))
        .await;
    assert!(matches!(too_big, Err(Error::TooLarge { .. })));

    let body = client
        .download(&Id::from("A1"), &Id::from("B42"), "notes.txt", "text/plain")
        .await
        .unwrap();
    assert_eq!(&body[..], b"A1|B42|notes.txt|type=text%2Fplain");
}

#[tokio::test]
async fn request_timeout_applies_to_send_not_stream() {
    let servers = start().await;
    let transport = HttpTransport::with_config(HttpConfig {
        request_timeout: Duration::from_millis(200),
        ..Default::default()
    })
    .unwrap();
    let get = |path: &str| HttpRequest {
        method: HttpMethod::Get,
        url: format!("{}{path}", servers.base),
        headers: vec![],
        body: None,
    };
    match transport.send(get("/slow")).await {
        Err(TransportError::Timeout) => {}
        other => panic!("expected a timeout, got {other:?}"),
    }
    let resp = transport.stream(get("/slow-stream")).await.unwrap();
    assert_eq!(resp.status, 200);
    let chunks: Vec<_> = resp.body.collect().await;
    let data: Vec<u8> = chunks
        .into_iter()
        .flat_map(|c| c.unwrap().to_vec())
        .collect();
    assert_eq!(data, b"data: late\n\n");
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    // Bind and drop to get a port with nothing listening.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let transport = HttpTransport::new().unwrap();
    let result = transport
        .send(HttpRequest {
            method: HttpMethod::Get,
            url: format!("http://{addr}/"),
            headers: vec![],
            body: None,
        })
        .await;
    assert!(
        matches!(result, Err(TransportError::Connect(_))),
        "{result:?}"
    );
}
