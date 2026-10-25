//! The in-process [`Transport`]: requests go straight to the fake server's
//! handlers, with no sockets.

use bytes::Bytes;
use mejl_jmap::transport::{
    BoxFuture, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
};

use crate::FakeServer;
use crate::http::{self, Reply};

/// A [`Transport`] that serves requests from a [`FakeServer`] in process.
///
/// Push streams carry real SSE bytes, so the client's EventSource parser
/// runs exactly as it does over HTTP. Clone it freely; every clone talks to
/// the same server.
#[derive(Clone)]
pub struct FakeTransport {
    server: FakeServer,
}

impl FakeTransport {
    pub(crate) fn new(server: FakeServer) -> Self {
        FakeTransport { server }
    }

    /// The server this transport talks to.
    pub fn server(&self) -> &FakeServer {
        &self.server
    }
}

impl std::fmt::Debug for FakeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeTransport").finish_non_exhaustive()
    }
}

impl Transport for FakeTransport {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let server = self.server.clone();
        Box::pin(async move {
            match http::handle(&server, req, None, None).await {
                Reply::Full(r) => Ok(r),
                Reply::Lost => Err(TransportError::Closed),
                Reply::Stream(_) => Err(TransportError::Other(
                    "the EventSource endpoint streams; use Transport::stream".to_owned(),
                )),
            }
        })
    }

    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
        let server = self.server.clone();
        Box::pin(async move {
            match http::handle(&server, req, None, None).await {
                Reply::Stream(s) => Ok(s),
                Reply::Lost => Err(TransportError::Closed),
                Reply::Full(r) => {
                    let chunks: Vec<Result<Bytes, TransportError>> = if r.body.is_empty() {
                        Vec::new()
                    } else {
                        vec![Ok(r.body)]
                    };
                    Ok(StreamingResponse {
                        status: r.status,
                        headers: r.headers,
                        body: Box::pin(futures::stream::iter(chunks)),
                    })
                }
            }
        })
    }
}
