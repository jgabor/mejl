//! Serving the fake over loopback HTTP with axum.
//!
//! Every request goes through the same handler as the in-process transport,
//! so both paths behave the same, faults and quirks included.

use std::io;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use futures::StreamExt;
use mejl_jmap::transport::{HttpMethod, HttpRequest};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::FakeServer;
use crate::http::{self, Reply};

#[derive(Clone)]
struct HttpState {
    server: FakeServer,
    shutdown: watch::Receiver<bool>,
    fallback_base: String,
}

/// A running loopback HTTP server. Dropping it starts a shutdown; call
/// [`HttpHandle::shutdown`] to wait for it to finish.
#[derive(Debug)]
pub struct HttpHandle {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl HttpHandle {
    /// The address the server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The session URL clients should start from.
    pub fn session_url(&self) -> String {
        format!("{}/jmap/session", self.base_url())
    }

    /// Stops accepting connections, closes open push streams and waits for
    /// the server task to end.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Only plain host names and IP literals are trusted from the Host header.
fn usable_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
}

async fn handler(State(st): State<HttpState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let method = match parts.method {
        axum::http::Method::GET => HttpMethod::Get,
        axum::http::Method::POST => HttpMethod::Post,
        _ => return plain(405, "method not allowed"),
    };
    let base = parts
        .headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| usable_host(h))
        .map(|h| format!("http://{h}"))
        .unwrap_or_else(|| st.fallback_base.clone());
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_ascii_lowercase(), v.to_owned()))
        })
        .collect();
    let limit = st.server.body_limit();
    let bytes = match axum::body::to_bytes(body, limit).await {
        Ok(b) => b,
        Err(_) => return plain(413, "request body too large"),
    };
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let request = HttpRequest {
        method,
        url: format!("{base}{path}"),
        headers,
        body: if bytes.is_empty() { None } else { Some(bytes) },
    };
    let reply = http::handle(&st.server, request, Some(base), Some(st.shutdown.clone())).await;
    match reply {
        Reply::Full(r) => build(r.status, &r.headers, Body::from(r.body)),
        Reply::Stream(s) => {
            let body = Body::from_stream(s.body.map(|chunk| chunk.map_err(io::Error::other)));
            build(s.status, &s.headers, body)
        }
        Reply::Lost => {
            // A body that fails at once makes hyper abort the response, so
            // the client sees the connection drop.
            let failing = futures::stream::once(async {
                Err::<bytes::Bytes, io::Error>(io::Error::other("response lost on purpose"))
            });
            build(
                200,
                &[("content-type".to_owned(), "application/json".to_owned())],
                Body::from_stream(failing),
            )
        }
    }
}

fn plain(status: u16, msg: &'static str) -> Response {
    build(
        status,
        &[("content-type".to_owned(), "text/plain".to_owned())],
        Body::from(msg),
    )
}

fn build(status: u16, headers: &[(String, String)], body: Body) -> Response {
    let mut builder = Response::builder().status(status);
    for (k, v) in headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder.body(body).unwrap_or_else(|_| {
        let mut r = Response::new(Body::from("bad response"));
        *r.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        r
    })
}

impl FakeServer {
    /// Serves this fake over HTTP on a loopback address, for tests that use
    /// a real HTTP client.
    ///
    /// Pass port 0 to get a free port; [`HttpHandle::local_addr`] tells
    /// which. Non-loopback addresses are refused, so the fake is never
    /// reachable from the network. Must be called inside a tokio runtime.
    pub async fn serve_http(&self, addr: SocketAddr) -> io::Result<HttpHandle> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mejl-fake only listens on loopback addresses",
            ));
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        let (stop, shutdown) = watch::channel(false);
        let state = HttpState {
            server: self.clone(),
            shutdown: shutdown.clone(),
            fallback_base: format!("http://{local}"),
        };
        let app = Router::new().fallback(handler).with_state(state);
        let mut signal = shutdown;
        let task = tokio::spawn(async move {
            let stopped = async move {
                let _ = signal.wait_for(|v| *v).await;
            };
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(stopped)
                .await
            {
                tracing::warn!("mejl-fake http server stopped: {e}");
            }
        });
        Ok(HttpHandle {
            addr: local,
            stop,
            task: Some(task),
        })
    }
}
