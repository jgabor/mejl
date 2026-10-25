//! EventSource push (RFC 8620 section 7.3).
//!
//! This module connects and turns the byte stream into [`PushEvent`]s. It
//! does not reconnect: backoff, idle timeouts and the resync after a
//! reconnect belong to the sync engine.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use futures::stream::FusedStream;
use serde_json::Value;

use crate::client::{Client, absolute_url, base_headers, check_status, set_bearer};
use crate::error::Error;
use crate::sse::{SseEvent, SseParser, SseTooLarge};
use crate::template;
use crate::transport::{BoxStream, HttpMethod, HttpRequest, TransportError};
use crate::types::core::{Id, State};

/// Changed states: account id to type name to new state.
pub type Changed = HashMap<Id, HashMap<String, State>>;

/// What the push channel reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushEvent {
    /// Something changed on the server. `changed` may name types mejl does
    /// not sync, such as `EmailDelivery`, and may be empty after a connect
    /// notice; either way a sync is the right response.
    StateChange { changed: Changed },
    /// A keep-alive. `interval` is the server's ping interval in seconds.
    Ping { interval: Option<u64> },
}

/// Why the push stream ended with an error. A clean end of stream is not
/// an error: the stream just finishes.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    TooLarge(#[from] SseTooLarge),
}

/// How to open the EventSource connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOptions {
    /// Type names to watch, or `None` for every type (`*`).
    pub types: Option<Vec<String>>,
    /// Ask the server to close after the first state change.
    pub close_after_state: bool,
    /// Requested ping interval in seconds. 0 asks for no pings.
    pub ping: u32,
    /// Sent as `Last-Event-ID` when reconnecting.
    pub last_event_id: Option<String>,
    /// Cap on one line or event, in bytes.
    pub max_event_size: usize,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            types: None,
            close_after_state: false,
            ping: 60,
            last_event_id: None,
            max_event_size: SseParser::DEFAULT_MAX_SIZE,
        }
    }
}

/// Build the EventSource URL from the session's `eventSourceUrl`.
///
/// Template variables `types`, `closeafter` and `ping` are expanded with
/// RFC 6570 encoding (so `*` becomes `%2A`). Any of the three the URL does
/// not contain is appended as a query parameter instead, which covers
/// Fastmail's plain URL with no template at all. Relative URLs resolve
/// against `base`, the session URL.
pub fn event_source_url(
    base: &str,
    event_source_url: &str,
    options: &PushOptions,
) -> Result<String, Error> {
    let types = match &options.types {
        Some(types) if !types.is_empty() => types.join(","),
        _ => "*".to_owned(),
    };
    let close_after = if options.close_after_state {
        "state"
    } else {
        "no"
    };
    let ping = options.ping.to_string();
    let params = [
        ("types", types.as_str()),
        ("closeafter", close_after),
        ("ping", ping.as_str()),
    ];

    let present = template::variables(event_source_url);
    let expanded = template::expand(event_source_url, |name| {
        params
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    });
    let absolute = absolute_url(base, &expanded)?;
    let missing: Vec<_> = params
        .iter()
        .filter(|(key, _)| !present.contains(key))
        .collect();
    if missing.is_empty() {
        return Ok(absolute);
    }
    let mut url = reqwest::Url::parse(&absolute).map_err(|e| Error::InvalidUrl {
        url: absolute.clone(),
        reason: e.to_string(),
    })?;
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in missing {
            query.append_pair(key, value);
        }
    }
    Ok(url.into())
}

/// Map one server-sent event to a push event. Unknown or malformed events
/// give `None`; they are never fatal.
pub fn parse_event(event: &SseEvent) -> Option<PushEvent> {
    if event.event == "ping" {
        let interval = serde_json::from_str::<Value>(&event.data)
            .ok()
            .and_then(|v| v.get("interval")?.as_u64());
        return Some(PushEvent::Ping { interval });
    }
    let value: Value = match serde_json::from_str(&event.data) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(event = %event.event, %err, "ignoring push event with bad JSON");
            return None;
        }
    };
    let object = value.as_object()?;
    let kind = object
        .get("@type")
        .or_else(|| object.get("type"))
        .and_then(Value::as_str);
    let changed = object.get("changed").and_then(Value::as_object);
    // RFC servers send an event named "state" with "@type": "StateChange".
    // Fastmail was seen sending "type": "connect" instead; event names other
    // than "state" are accepted only when the object itself says what it is.
    let named_state = matches!(event.event.as_str(), "state" | "message");
    let accepted = match (kind, changed) {
        (Some("connect"), _) => true,
        (Some("StateChange"), Some(_)) => true,
        (_, Some(_)) => named_state,
        _ => false,
    };
    if !accepted {
        tracing::debug!(event = %event.event, kind, "ignoring unknown push event");
        return None;
    }
    Some(PushEvent::StateChange {
        changed: changed.map(parse_changed).unwrap_or_default(),
    })
}

fn parse_changed(changed: &serde_json::Map<String, Value>) -> Changed {
    let mut out = Changed::new();
    for (account, types) in changed {
        let Some(types) = types.as_object() else {
            tracing::warn!(%account, "ignoring non-object entry in push changed map");
            continue;
        };
        let entry = out.entry(Id::from(account.as_str())).or_default();
        for (type_name, state) in types {
            match state.as_str() {
                Some(state) => {
                    entry.insert(type_name.clone(), State::from(state));
                }
                None => tracing::warn!(%account, %type_name, "ignoring non-string push state"),
            }
        }
    }
    out
}

/// A stream of push events over an open EventSource connection.
///
/// It ends when the server closes the connection, and yields one error and
/// ends if the transport fails or the server sends an oversized event.
pub struct PushStream {
    body: BoxStream<'static, Result<Bytes, TransportError>>,
    parser: SseParser,
    pending: VecDeque<PushEvent>,
    error: Option<PushError>,
    done: bool,
    scratch: Vec<SseEvent>,
}

impl std::fmt::Debug for PushStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushStream")
            .field("pending", &self.pending.len())
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl PushStream {
    /// Parse push events from a raw `text/event-stream` body.
    pub fn new(
        body: BoxStream<'static, Result<Bytes, TransportError>>,
        max_event_size: usize,
    ) -> Self {
        Self {
            body,
            parser: SseParser::new(max_event_size),
            pending: VecDeque::new(),
            error: None,
            done: false,
            scratch: Vec::new(),
        }
    }

    /// The last event id seen, for [`PushOptions::last_event_id`] on
    /// reconnect.
    pub fn last_event_id(&self) -> Option<&str> {
        self.parser.last_event_id()
    }

    /// The reconnection delay the server asked for with `retry:`.
    pub fn retry(&self) -> Option<Duration> {
        self.parser.retry()
    }
}

impl Stream for PushStream {
    type Item = Result<PushEvent, PushError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if let Some(err) = this.error.take() {
                return Poll::Ready(Some(Err(err)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match this.body.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    let result = this.parser.feed(&chunk, &mut this.scratch);
                    this.pending
                        .extend(this.scratch.drain(..).filter_map(|e| parse_event(&e)));
                    if let Err(err) = result {
                        this.error = Some(err.into());
                        this.done = true;
                    }
                }
                Poll::Ready(Some(Err(err))) => {
                    this.error = Some(err.into());
                    this.done = true;
                }
                Poll::Ready(None) => {
                    this.parser.finish();
                    this.done = true;
                }
            }
        }
    }
}

impl FusedStream for PushStream {
    fn is_terminated(&self) -> bool {
        self.done && self.pending.is_empty() && self.error.is_none()
    }
}

impl Client {
    /// Open the EventSource connection from the session's
    /// `eventSourceUrl` and return its event stream.
    ///
    /// Connection problems and HTTP errors (401, 429 with `Retry-After`,
    /// and so on) come back here, before any event. On an OAuth client a
    /// 401 gets one refresh and one reconnect first. The stream has no
    /// overall timeout.
    pub async fn push(&self, options: &PushOptions) -> Result<PushStream, Error> {
        let session = self.session().ok_or(Error::NoSession)?;
        let url = event_source_url(self.session_url(), &session.event_source_url, options)?;
        // The token is taken at each connect, so a reconnect after a
        // refresh uses the new one.
        let (token, provider) = self.bearer().await?;
        let mut headers = base_headers(&token);
        headers.push(("accept".into(), "text/event-stream".into()));
        headers.push(("cache-control".into(), "no-cache".into()));
        if let Some(id) = options.last_event_id.as_deref().filter(|id| !id.is_empty()) {
            headers.push(("last-event-id".into(), id.to_owned()));
        }
        let mut req = HttpRequest {
            method: HttpMethod::Get,
            url,
            headers,
            body: None,
        };
        let mut resp = self.inner.transport.stream(req.clone()).await?;
        if resp.status == 401
            && let Some(provider) = provider
        {
            drop(resp);
            let fresh = provider.renew(&token).await?;
            set_bearer(&mut req.headers, &fresh);
            resp = self.inner.transport.stream(req).await?;
        }
        // The body of an error response is a stream that might never end,
        // so it is not read.
        check_status(resp.status, &resp.headers, &[])?;
        if let Some(content_type) = resp.header("content-type") {
            let essence = content_type.split(';').next().unwrap_or("").trim();
            if !essence.eq_ignore_ascii_case("text/event-stream") {
                return Err(Error::UnexpectedContentType(content_type.to_owned()));
            }
        }
        Ok(PushStream::new(resp.body, options.max_event_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{FakeTransport, SESSION, client_with};
    use crate::transport::StreamingResponse;
    use futures::StreamExt;
    use std::sync::Arc;

    const BASE: &str = "https://jmap.example.com/jmap/session";

    #[test]
    fn template_url_is_expanded() {
        let url = event_source_url(
            BASE,
            "https://jmap.example.com/jmap/event/?types={types}&closeafter={closeafter}&ping={ping}",
            &PushOptions::default(),
        )
        .unwrap();
        assert_eq!(
            url,
            "https://jmap.example.com/jmap/event/?types=%2A&closeafter=no&ping=60"
        );
    }

    #[test]
    fn plain_url_gets_query_parameters() {
        let url = event_source_url(
            BASE,
            "https://jmap.example.com/jmap/event/",
            &PushOptions::default(),
        )
        .unwrap();
        assert_eq!(
            url,
            "https://jmap.example.com/jmap/event/?types=*&closeafter=no&ping=60"
        );
        let url = event_source_url(BASE, "/jmap/event/?x=1", &PushOptions::default()).unwrap();
        assert_eq!(
            url,
            "https://jmap.example.com/jmap/event/?x=1&types=*&closeafter=no&ping=60"
        );
    }

    #[test]
    fn partial_template_appends_the_rest() {
        let options = PushOptions {
            types: Some(vec!["Email".into(), "Mailbox".into()]),
            close_after_state: true,
            ping: 0,
            ..Default::default()
        };
        let url = event_source_url(BASE, "/ev/{types}", &options).unwrap();
        assert_eq!(
            url,
            "https://jmap.example.com/ev/Email%2CMailbox?closeafter=state&ping=0"
        );
    }

    fn sse(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: None,
        }
    }

    fn changed(pairs: &[(&str, &str, &str)]) -> Changed {
        let mut out = Changed::new();
        for (account, kind, state) in pairs {
            out.entry(Id::from(*account))
                .or_default()
                .insert((*kind).to_owned(), State::from(*state));
        }
        out
    }

    #[test]
    fn rfc_state_change() {
        let data = r#"{"@type":"StateChange","changed":{"a3123":{"Email":"d35ecb040aab","EmailDelivery":"428d565f2440"},"a43461d":{"Mailbox":"0af7a512ce70"}}}"#;
        assert_eq!(
            parse_event(&sse("state", data)),
            Some(PushEvent::StateChange {
                changed: changed(&[
                    ("a3123", "Email", "d35ecb040aab"),
                    ("a3123", "EmailDelivery", "428d565f2440"),
                    ("a43461d", "Mailbox", "0af7a512ce70"),
                ])
            })
        );
        // The object names itself, so the event name does not matter.
        assert!(parse_event(&sse("whatever", data)).is_some());
    }

    #[test]
    fn fastmail_connect_shape() {
        let data = r#"{"type":"connect","changed":{"u1":{"EmailDelivery":"s9"}}}"#;
        assert_eq!(
            parse_event(&sse("message", data)),
            Some(PushEvent::StateChange {
                changed: changed(&[("u1", "EmailDelivery", "s9")])
            })
        );
        assert_eq!(
            parse_event(&sse("message", r#"{"type":"connect"}"#)),
            Some(PushEvent::StateChange {
                changed: Changed::new()
            })
        );
    }

    #[test]
    fn state_event_without_type() {
        assert_eq!(
            parse_event(&sse("state", r#"{"changed":{"u1":{"Email":"e2"}}}"#)),
            Some(PushEvent::StateChange {
                changed: changed(&[("u1", "Email", "e2")])
            })
        );
    }

    #[test]
    fn pings() {
        assert_eq!(
            parse_event(&sse("ping", r#"{"interval":30}"#)),
            Some(PushEvent::Ping { interval: Some(30) })
        );
        assert_eq!(
            parse_event(&sse("ping", "")),
            Some(PushEvent::Ping { interval: None })
        );
        assert_eq!(
            parse_event(&sse("ping", r#"{"interval":"x"}"#)),
            Some(PushEvent::Ping { interval: None })
        );
    }

    #[test]
    fn unknown_and_malformed_are_ignored() {
        for (name, data) in [
            ("state", "not json"),
            ("state", "[1,2]"),
            ("state", r#"{"@type":"StateChange"}"#),
            ("state", r#"{"changed":5}"#),
            ("other", r#"{"changed":{"u1":{"Email":"e"}}}"#),
            ("other", r#"{"@type":"Mystery","changed":{}}"#),
            ("message", "{}"),
        ] {
            assert_eq!(parse_event(&sse(name, data)), None, "{name} {data}");
        }
    }

    #[test]
    fn bad_entries_inside_changed_are_skipped() {
        let data =
            r#"{"@type":"StateChange","changed":{"u1":{"Email":"e1","Mailbox":7},"u2":"x"}}"#;
        assert_eq!(
            parse_event(&sse("state", data)),
            Some(PushEvent::StateChange {
                changed: changed(&[("u1", "Email", "e1")])
            })
        );
    }

    fn body(
        chunks: Vec<Result<&'static str, TransportError>>,
    ) -> BoxStream<'static, Result<Bytes, TransportError>> {
        Box::pin(futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| c.map(|s| Bytes::from_static(s.as_bytes()))),
        ))
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn stream_yields_events_then_error() {
        let stream = PushStream::new(
            body(vec![
                Ok("event: ping\ndata: {\"interval\":5}\n\nid: 9\nevent: st"),
                Ok("ate\ndata: {\"@type\":\"StateChange\",\"changed\":{}}\n\n: comment\n\n"),
                Err(TransportError::Closed),
                Ok("data: never seen\n\n"),
            ]),
            1024,
        );
        let items: Vec<_> = block_on(stream.collect::<Vec<_>>());
        assert_eq!(items.len(), 3);
        assert!(matches!(
            items[0],
            Ok(PushEvent::Ping { interval: Some(5) })
        ));
        assert!(matches!(items[1], Ok(PushEvent::StateChange { .. })));
        assert!(matches!(
            items[2],
            Err(PushError::Transport(TransportError::Closed))
        ));
    }

    #[test]
    fn stream_keeps_last_event_id_and_retry() {
        let mut stream = PushStream::new(
            body(vec![Ok("retry: 3000\nid: abc\nevent: ping\ndata: {}\n\n")]),
            1024,
        );
        block_on(async {
            assert!(stream.next().await.is_some());
            assert!(stream.next().await.is_none());
            assert!(stream.is_terminated());
        });
        assert_eq!(stream.last_event_id(), Some("abc"));
        assert_eq!(stream.retry(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn oversized_event_ends_stream_after_earlier_events() {
        let stream = PushStream::new(
            body(vec![Ok(
                "event: ping\ndata: {}\n\ndata: 0123456789012345678901234567890123456789\n",
            )]),
            32,
        );
        let items: Vec<_> = block_on(stream.collect::<Vec<_>>());
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        assert!(matches!(items[1], Err(PushError::TooLarge(_))));
    }

    fn streaming(
        status: u16,
        headers: &[(&str, &str)],
        chunks: Vec<Result<&'static str, TransportError>>,
    ) -> StreamingResponse {
        StreamingResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body(chunks),
        }
    }

    #[test]
    fn client_push_sends_headers_and_parses() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        client.set_session(serde_json::from_str(SESSION).unwrap());
        fake.streams.lock().unwrap().push_back(streaming(
            200,
            &[("content-type", "text/event-stream; charset=utf-8")],
            vec![Ok("event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"A1\":{\"Email\":\"e5\"}}}\n\n")],
        ));
        let options = PushOptions {
            last_event_id: Some("41".into()),
            ..Default::default()
        };
        let stream = block_on(client.push(&options)).unwrap();
        let req = fake.last_request();
        assert_eq!(
            req.url,
            "https://jmap.example.com/jmap/event/?types=*&closeafter=no&ping=60"
        );
        assert_eq!(req.header("accept"), Some("text/event-stream"));
        assert_eq!(req.header("last-event-id"), Some("41"));
        assert_eq!(req.header("authorization"), Some("Bearer tok-123"));
        let items: Vec<_> = block_on(stream.collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn client_push_maps_errors() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        assert!(matches!(
            block_on(client.push(&PushOptions::default())),
            Err(Error::NoSession)
        ));
        client.set_session(serde_json::from_str(SESSION).unwrap());
        {
            let mut streams = fake.streams.lock().unwrap();
            streams.push_back(streaming(401, &[], vec![]));
            streams.push_back(streaming(429, &[("retry-after", "30")], vec![]));
            streams.push_back(streaming(
                200,
                &[("content-type", "application/json")],
                vec![],
            ));
        }
        assert!(matches!(
            block_on(client.push(&PushOptions::default())),
            Err(Error::Unauthorized { status: 401, .. })
        ));
        match block_on(client.push(&PushOptions::default())) {
            Err(e) => assert_eq!(e.retry_after(), Some(Duration::from_secs(30))),
            Ok(_) => panic!("expected an error"),
        }
        assert!(matches!(
            block_on(client.push(&PushOptions::default())),
            Err(Error::UnexpectedContentType(_))
        ));
        assert!(matches!(
            block_on(client.push(&PushOptions::default())),
            Err(Error::Transport(TransportError::Closed))
        ));
    }

    #[test]
    fn push_future_and_stream_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        let options = PushOptions::default();
        assert_send(&client.push(&options));
        assert_send(&PushStream::new(body(vec![]), 16));
    }
}
