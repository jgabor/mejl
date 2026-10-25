//! The seam between the JMAP client and the network.
//!
//! The client only ever talks to a `Transport`. The production
//! implementation uses reqwest. `mejl-fake` provides an in-process one for
//! tests, benches and demo mode. Both deal in raw HTTP-shaped values, so the
//! client's JSON encoding and decoding and the EventSource parser run the
//! same way against both.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// A boxed, sendable stream.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// The HTTP methods JMAP needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// One outgoing HTTP request.
#[derive(Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    /// Header names are lower case.
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
}

impl HttpRequest {
    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

// Requests carry the bearer token, so Debug must never print header values
// that hold credentials. A stray `?req` in a log line would leak it.
impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &RedactedHeaders(&self.headers))
            .field("body", &self.body.as_ref().map(|b| b.len()))
            .finish()
    }
}

/// A buffered HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names are lower case.
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl HttpResponse {
    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

/// Network-level failures. HTTP error statuses are not transport errors;
/// they arrive as an `HttpResponse` with that status.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("request timed out")]
    Timeout,
    #[error("stream closed")]
    Closed,
    #[error("transport error: {0}")]
    Other(String),
}

/// A response whose body arrives as a byte stream, used for EventSource.
pub struct StreamingResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BoxStream<'static, Result<Bytes, TransportError>>,
}

impl StreamingResponse {
    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

impl fmt::Debug for StreamingResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamingResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Moves HTTP requests to a JMAP server and back.
pub trait Transport: Send + Sync + 'static {
    /// Send one request and buffer the whole response body.
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>>;

    /// Open a long-lived GET whose body is streamed (EventSource push).
    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>>;
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Headers whose values are credentials and must not be printed.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    ["authorization", "cookie", "proxy-authorization"]
        .iter()
        .any(|s| name.eq_ignore_ascii_case(s))
}

struct RedactedHeaders<'a>(&'a [(String, String)]);

impl fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        for (name, value) in self.0 {
            if is_sensitive_header(name) {
                list.entry(&(name.as_str(), "[redacted]"));
            } else {
                list.entry(&(name.as_str(), value.as_str()));
            }
        }
        list.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_authorization() {
        let req = HttpRequest {
            method: HttpMethod::Get,
            url: "https://jmap.example.com/session".into(),
            headers: vec![
                ("authorization".into(), "Bearer hunter2-token".into()),
                ("Cookie".into(), "s=abc".into()),
                ("accept".into(), "application/json".into()),
            ],
            body: Some(Bytes::from_static(b"{}")),
        };
        let shown = format!("{req:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("s=abc"), "{shown}");
        assert!(shown.contains("[redacted]"));
        assert!(shown.contains("application/json"));
    }

    #[test]
    fn header_lookup_ignores_case() {
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: String::new(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: None,
        };
        assert_eq!(req.header("Content-Type"), Some("application/json"));
        assert_eq!(req.header("accept"), None);
    }
}
