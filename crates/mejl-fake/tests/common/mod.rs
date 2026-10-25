//! Helpers shared by the integration tests: a tiny JMAP client over the
//! in-process transport, and an SSE frame reader.

#![allow(dead_code)]

use bytes::Bytes;
use futures::StreamExt;
use mejl_fake::{FakeServer, IN_PROCESS_BASE};
use mejl_jmap::transport::{
    BoxStream, HttpMethod, HttpRequest, HttpResponse, Transport, TransportError,
};
use serde_json::{Value, json};

pub const ALL: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "urn:ietf:params:jmap:contacts",
    "https://www.fastmail.com/dev/maskedemail",
];

pub fn now() -> jiff::Timestamp {
    "2026-10-23T14:27:00Z".parse().unwrap()
}

pub struct Client {
    pub server: FakeServer,
    pub account: String,
}

impl Client {
    pub fn new(server: FakeServer) -> Self {
        let account = server.account_id().to_owned();
        Client { server, account }
    }

    pub fn empty() -> Self {
        Client::new(FakeServer::new(mejl_fake::FakeConfig {
            now: Some(now()),
            ..Default::default()
        }))
    }

    pub fn demo() -> Self {
        Client::new(FakeServer::demo(now()))
    }

    pub fn auth(&self) -> Vec<(String, String)> {
        vec![(
            "authorization".to_owned(),
            format!("Bearer {}", self.server.token()),
        )]
    }

    pub async fn get(&self, path: &str) -> HttpResponse {
        self.server
            .transport()
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: format!("{IN_PROCESS_BASE}{path}"),
                headers: self.auth(),
                body: None,
            })
            .await
            .unwrap()
    }

    pub async fn post_raw(&self, body: impl Into<Bytes>) -> HttpResponse {
        self.server
            .transport()
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: format!("{IN_PROCESS_BASE}/jmap/api/"),
                headers: self.auth(),
                body: Some(body.into()),
            })
            .await
            .unwrap()
    }

    /// Sends a request and returns the whole response object.
    pub async fn request(&self, using: &[&str], calls: Value) -> Value {
        let body = json!({"using": using, "methodCalls": calls}).to_string();
        let resp = self.post_raw(body).await;
        assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
        serde_json::from_slice(&resp.body).unwrap()
    }

    /// Sends calls with every capability and returns `methodResponses`.
    pub async fn calls(&self, calls: Value) -> Vec<Value> {
        let r = self.request(ALL, calls).await;
        r["methodResponses"].as_array().unwrap().clone()
    }

    /// One call; returns the response arguments, panicking on an error.
    pub async fn call(&self, method: &str, mut args: Value) -> Value {
        if args.get("accountId").is_none() {
            args["accountId"] = json!(self.account);
        }
        let r = self.calls(json!([[method, args, "0"]])).await;
        assert_eq!(r[0][0], method, "{}", r[0]);
        r[0][1].clone()
    }

    /// One call expected to fail; returns the error type.
    pub async fn call_err(&self, method: &str, mut args: Value) -> String {
        if args.get("accountId").is_none() {
            args["accountId"] = json!(self.account);
        }
        let r = self.calls(json!([[method, args, "0"]])).await;
        assert_eq!(r[0][0], "error", "expected an error, got {}", r[0]);
        r[0][1]["type"].as_str().unwrap().to_owned()
    }

    pub fn mailbox(&self, key: &str) -> String {
        self.server.mailbox_id(key).unwrap()
    }

    pub async fn open_events(
        &self,
        query: &str,
    ) -> BoxStream<'static, Result<Bytes, TransportError>> {
        let resp = self
            .server
            .transport()
            .stream(HttpRequest {
                method: HttpMethod::Get,
                url: format!("{IN_PROCESS_BASE}/jmap/event/{query}"),
                headers: self.auth(),
                body: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        resp.body
    }
}

/// Reads one complete SSE frame (up to a blank line) from a byte stream.
pub struct Frames<S> {
    stream: S,
    buf: Vec<u8>,
    pub chunks: usize,
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    Event { event: String, data: Value },
    Error,
    End,
}

impl<S, E> Frames<S>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
{
    pub fn new(stream: S) -> Self {
        Frames {
            stream,
            buf: Vec::new(),
            chunks: 0,
        }
    }

    pub async fn next(&mut self) -> Frame {
        loop {
            if let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
                let frame: Vec<u8> = self.buf.drain(..pos + 2).collect();
                let text = String::from_utf8(frame).unwrap();
                let mut event = String::new();
                let mut data = String::new();
                for line in text.lines() {
                    if let Some(v) = line.strip_prefix("event: ") {
                        event = v.to_owned();
                    } else if let Some(v) = line.strip_prefix("data: ") {
                        data.push_str(v);
                    }
                }
                return Frame::Event {
                    event,
                    data: serde_json::from_str(&data).unwrap(),
                };
            }
            match self.stream.next().await {
                Some(Ok(chunk)) => {
                    self.chunks += 1;
                    self.buf.extend_from_slice(&chunk);
                }
                Some(Err(_)) => return Frame::Error,
                None => return Frame::End,
            }
        }
    }
}

/// Waits for the next frame, failing the test after a few seconds.
pub async fn next_frame<S, E>(frames: &mut Frames<S>) -> Frame
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
        .await
        .expect("timed out waiting for a push frame")
}
