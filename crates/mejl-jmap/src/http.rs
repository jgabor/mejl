//! The production [`Transport`] on reqwest with rustls.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::{Attempt, Policy};

use crate::client::USER_AGENT;
use crate::transport::{
    BoxFuture, HttpMethod, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
    is_sensitive_header,
};

/// Settings for [`HttpTransport`].
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Time allowed to open a connection.
    pub connect_timeout: Duration,
    /// Total time allowed for one buffered request. Streams have none.
    pub request_timeout: Duration,
    /// Largest buffered response body accepted.
    pub max_response_bytes: usize,
    /// Most same-origin redirects followed for one request.
    pub max_redirects: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            // Above Fastmail's published 250 MB maxSizeUpload, so any blob
            // the server accepts can be downloaded again.
            max_response_bytes: 512 * 1024 * 1024,
            max_redirects: 5,
        }
    }
}

/// HTTP over reqwest: rustls, HTTP/2, gzip, no proxy, and redirects only
/// within the same origin so the token never reaches another host.
#[derive(Debug, Clone)]
pub struct HttpTransport {
    client: reqwest::Client,
    config: HttpConfig,
}

impl HttpTransport {
    /// A transport with default settings.
    pub fn new() -> Result<Self, TransportError> {
        Self::with_config(HttpConfig::default())
    }

    /// A transport with custom settings.
    pub fn with_config(config: HttpConfig) -> Result<Self, TransportError> {
        let max_redirects = config.max_redirects;
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(config.connect_timeout)
            // No client-wide timeout: it would also cut the EventSource
            // stream. `send` sets a per-request timeout instead.
            .gzip(true)
            .no_proxy()
            .redirect(Policy::custom(move |attempt| {
                same_origin_policy(attempt, max_redirects)
            }))
            .build()
            .map_err(|e| TransportError::Other(format!("cannot build HTTP client: {e}")))?;
        Ok(Self { client, config })
    }

    fn build(&self, req: HttpRequest) -> Result<reqwest::RequestBuilder, TransportError> {
        let url = reqwest::Url::parse(&req.url)
            .map_err(|e| TransportError::Other(format!("invalid URL: {e}")))?;
        let mut builder = match req.method {
            HttpMethod::Get => self.client.get(url),
            HttpMethod::Post => self.client.post(url),
        };
        builder = builder.headers(header_map(&req.headers)?);
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        Ok(builder)
    }
}

fn same_origin_policy(attempt: Attempt<'_>, max_redirects: usize) -> reqwest::redirect::Action {
    if attempt.previous().len() > max_redirects {
        return attempt.stop();
    }
    let same = attempt
        .previous()
        .last()
        .is_some_and(|prev| prev.origin() == attempt.url().origin());
    if same {
        attempt.follow()
    } else {
        attempt.stop()
    }
}

fn header_map(headers: &[(String, String)]) -> Result<HeaderMap, TransportError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| TransportError::Other(format!("invalid header name {name:?}")))?;
        // The value is not echoed: it may be the token.
        let mut header_value = HeaderValue::from_str(value)
            .map_err(|_| TransportError::Other(format!("invalid value for header {name:?}")))?;
        if is_sensitive_header(name) {
            header_value.set_sensitive(true);
        }
        map.append(header_name, header_value);
    }
    Ok(map)
}

fn response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

fn map_error(err: reqwest::Error) -> TransportError {
    // Without the URL: it may carry query parameters worth keeping out of
    // logs, and callers know which request failed.
    let err = err.without_url();
    // A timeout while connecting is a connect error: the request was never
    // sent, which the OAuth refresh relies on to tell a lost answer apart.
    if err.is_connect() {
        TransportError::Connect(describe(&err))
    } else if err.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Other(describe(&err))
    }
}

// reqwest's own message is only "error sending request". The cause, such as
// a refused connection or an untrusted certificate, sits in the source chain,
// and the user needs it to tell a network problem from a TLS one.
fn describe(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        let part = cause.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = cause.source();
    }
    text
}

impl Transport for HttpTransport {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let built = self.build(req);
        let timeout = self.config.request_timeout;
        let limit = self.config.max_response_bytes;
        Box::pin(async move {
            let resp = built?.timeout(timeout).send().await.map_err(map_error)?;
            let status = resp.status().as_u16();
            let headers = response_headers(resp.headers());
            let mut body = Vec::new();
            let mut chunks = resp.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(map_error)?;
                if body.len() + chunk.len() > limit {
                    return Err(TransportError::Other(format!(
                        "response body exceeds {limit} bytes"
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status,
                headers,
                body: Bytes::from(body),
            })
        })
    }

    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
        let built = self.build(req);
        Box::pin(async move {
            let resp = built?.send().await.map_err(map_error)?;
            let status = resp.status().as_u16();
            let headers = response_headers(resp.headers());
            let body = resp.bytes_stream().map(|chunk| chunk.map_err(map_error));
            Ok(StreamingResponse {
                status,
                headers,
                body: Box::pin(body),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_headers_without_echoing_values() {
        let err = header_map(&[("authorization".into(), "Bearer a\nb".into())]).unwrap_err();
        let shown = err.to_string();
        assert!(!shown.contains("Bearer"), "{shown}");
        assert!(header_map(&[("bad name".into(), "x".into())]).is_err());
        let ok = header_map(&[("authorization".into(), "Bearer t".into())]).unwrap();
        assert!(ok["authorization"].is_sensitive());
    }

    #[test]
    fn builds_with_defaults() {
        assert!(HttpTransport::new().is_ok());
    }
}
