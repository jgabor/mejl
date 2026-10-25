//! The JMAP client: session, API calls and blobs over a [`Transport`].

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::error::{Error, RequestError, body_snippet, parse_retry_after};
use crate::request::{Request, Response};
use crate::secret::Secret;
use crate::session::Session;
use crate::template;
use crate::transport::{HttpMethod, HttpRequest, HttpResponse, Transport};
use crate::types::core::{Extra, Id};

/// mejl's User-Agent. Never add anything that identifies the user.
pub const USER_AGENT: &str = concat!("mejl/", env!("CARGO_PKG_VERSION"));

/// How much of an error body to keep in [`Error::Http`].
const ERROR_BODY_LIMIT: usize = 512;

/// The result of a blob upload (RFC 8620 section 6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    pub account_id: Id,
    pub blob_id: Id,
    #[serde(rename = "type", default)]
    pub content_type: String,
    #[serde(default)]
    pub size: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A JMAP client. Cheap to clone; clones share the session and token.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) transport: Arc<dyn Transport>,
    session_url: String,
    token: RwLock<Secret>,
    session: RwLock<Option<Arc<Session>>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("session_url", &self.inner.session_url)
            .field("token", &"[redacted]")
            .field("has_session", &self.session().is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for the session resource at `session_url`.
    pub fn new(
        transport: Arc<dyn Transport>,
        session_url: impl Into<String>,
        token: Secret,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport,
                session_url: session_url.into(),
                token: RwLock::new(token),
                session: RwLock::new(None),
            }),
        }
    }

    /// The session URL this client was created with.
    pub fn session_url(&self) -> &str {
        &self.inner.session_url
    }

    /// Replace the token, for example after an OAuth refresh.
    pub fn set_token(&self, token: Secret) {
        *self
            .inner
            .token
            .write()
            .unwrap_or_else(PoisonError::into_inner) = token;
    }

    /// The loaded session, if any.
    pub fn session(&self) -> Option<Arc<Session>> {
        self.inner
            .session
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Load a session, for example a cached copy for the first frame.
    pub fn set_session(&self, session: Session) -> Arc<Session> {
        let session = Arc::new(session);
        *self
            .inner
            .session
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        session
    }

    /// Fetch the session resource and load it.
    pub async fn fetch_session(&self) -> Result<Arc<Session>, Error> {
        let url = absolute_url(&self.inner.session_url, &self.inner.session_url)?;
        let resp = self
            .send(
                HttpMethod::Get,
                url,
                None,
                &[("accept", "application/json")],
            )
            .await?;
        let session: Session =
            serde_json::from_slice(&resp.body).map_err(|source| Error::Decode {
                context: "session".into(),
                source,
            })?;
        Ok(self.set_session(session))
    }

    /// Post a request to the session's `apiUrl`.
    ///
    /// The response's `session_changed` is set when its `sessionState`
    /// differs from the loaded session. The caller decides when to refetch.
    pub async fn call(&self, request: &Request) -> Result<Response, Error> {
        let session = self.require_session()?;
        let url = self.resolve(&session.api_url)?;
        let body = serde_json::to_vec(request).map_err(|source| Error::Encode {
            context: "request".into(),
            source,
        })?;
        let resp = self
            .send(
                HttpMethod::Post,
                url,
                Some(Bytes::from(body)),
                &[
                    ("content-type", "application/json"),
                    ("accept", "application/json"),
                ],
            )
            .await?;
        let mut response: Response =
            serde_json::from_slice(&resp.body).map_err(|source| Error::Decode {
                context: "JMAP response".into(),
                source,
            })?;
        response.session_changed =
            !response.session_state.is_empty() && response.session_state != session.state;
        Ok(response)
    }

    /// Upload a blob to the session's `uploadUrl`.
    ///
    /// Fails early with [`Error::TooLarge`] when the session states a
    /// `maxSizeUpload` and the blob exceeds it. Without a stated limit the
    /// server decides.
    pub async fn upload(
        &self,
        account_id: &Id,
        content_type: &str,
        data: Bytes,
    ) -> Result<UploadResponse, Error> {
        let session = self.require_session()?;
        let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let stated = session
            .core_capability()
            .and_then(|c| c.max_size_upload)
            .filter(|limit| *limit > 0);
        if let Some(limit) = stated
            && size > limit
        {
            return Err(Error::TooLarge { size, limit });
        }
        let expanded = template::expand(&session.upload_url, |name| {
            (name == "accountId").then(|| account_id.to_string())
        });
        let url = self.resolve(&expanded)?;
        let resp = self
            .send(
                HttpMethod::Post,
                url,
                Some(data),
                &[
                    ("content-type", content_type),
                    ("accept", "application/json"),
                ],
            )
            .await?;
        serde_json::from_slice(&resp.body).map_err(|source| Error::Decode {
            context: "upload response".into(),
            source,
        })
    }

    /// The download URL for a blob, from the session's `downloadUrl`
    /// template.
    pub fn download_url(
        &self,
        account_id: &Id,
        blob_id: &Id,
        name: &str,
        content_type: &str,
    ) -> Result<String, Error> {
        let session = self.require_session()?;
        let expanded = expand_download_url(
            &session.download_url,
            account_id,
            blob_id,
            name,
            content_type,
        );
        self.resolve(&expanded)
    }

    /// Download a blob. The download host may differ from the API host;
    /// the token is sent there too, as JMAP requires.
    pub async fn download(
        &self,
        account_id: &Id,
        blob_id: &Id,
        name: &str,
        content_type: &str,
    ) -> Result<Bytes, Error> {
        let url = self.download_url(account_id, blob_id, name, content_type)?;
        let resp = self.send(HttpMethod::Get, url, None, &[]).await?;
        Ok(resp.body)
    }

    fn require_session(&self) -> Result<Arc<Session>, Error> {
        self.session().ok_or(Error::NoSession)
    }

    /// Resolve a session URL, which may be relative, against the session
    /// resource.
    pub(crate) fn resolve(&self, url: &str) -> Result<String, Error> {
        absolute_url(&self.inner.session_url, url)
    }

    /// Headers every request carries.
    pub(crate) fn base_headers(&self) -> Vec<(String, String)> {
        let token = self
            .inner
            .token
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        vec![
            ("authorization".into(), format!("Bearer {}", token.expose())),
            ("user-agent".into(), USER_AGENT.into()),
        ]
    }

    async fn send(
        &self,
        method: HttpMethod,
        url: String,
        body: Option<Bytes>,
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse, Error> {
        let mut all = self.base_headers();
        all.extend(
            headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        let req = HttpRequest {
            method,
            url,
            headers: all,
            body,
        };
        let resp = self.inner.transport.send(req).await?;
        check_status(resp.status, &resp.headers, &resp.body)?;
        Ok(resp)
    }
}

/// Expand a `downloadUrl` template.
pub fn expand_download_url(
    template: &str,
    account_id: &Id,
    blob_id: &Id,
    name: &str,
    content_type: &str,
) -> String {
    template::expand(template, |var| match var {
        "accountId" => Some(account_id.to_string()),
        "blobId" => Some(blob_id.to_string()),
        "name" => Some(name.to_owned()),
        "type" => Some(content_type.to_owned()),
        _ => None,
    })
}

/// Resolve `url` against `base` and require http or https.
pub(crate) fn absolute_url(base: &str, url: &str) -> Result<String, Error> {
    let invalid = |reason: String| Error::InvalidUrl {
        url: url.to_owned(),
        reason,
    };
    if url.is_empty() {
        return Err(invalid("empty".into()));
    }
    let base = reqwest::Url::parse(base).map_err(|e| Error::InvalidUrl {
        url: base.to_owned(),
        reason: e.to_string(),
    })?;
    let joined = base.join(url).map_err(|e| invalid(e.to_string()))?;
    if !matches!(joined.scheme(), "http" | "https") {
        return Err(invalid(format!("unsupported scheme {:?}", joined.scheme())));
    }
    Ok(joined.into())
}

/// Map an HTTP status to an error, or `Ok` for 2xx.
pub(crate) fn check_status(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(), Error> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let retry_after =
        || header("retry-after").and_then(|v| parse_retry_after(v, jiff::Timestamp::now()));
    match status {
        200..=299 => Ok(()),
        // Fastmail answers 403 for a token that lacks a scope or was
        // revoked; either way the fix is new credentials, as for 401.
        401 | 403 => Err(Error::Unauthorized {
            www_authenticate: header("www-authenticate").map(str::to_owned),
        }),
        429 => Err(Error::RateLimited {
            retry_after: retry_after(),
        }),
        503 => Err(Error::ServiceUnavailable {
            retry_after: retry_after(),
        }),
        300..=399 => Err(Error::Redirect {
            status,
            location: header("location").map(str::to_owned),
        }),
        _ => {
            let is_problem = header("content-type")
                .is_some_and(|ct| ct.to_ascii_lowercase().contains("problem+json"));
            match RequestError::from_body(body) {
                Some(problem)
                    if is_problem
                        || problem
                            .kind
                            .as_str()
                            .starts_with("urn:ietf:params:jmap:error:") =>
                {
                    Err(Error::Request(Box::new(problem)))
                }
                _ => Err(Error::Http {
                    status,
                    body: body_snippet(body, ERROR_BODY_LIMIT),
                }),
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::RequestErrorType;
    use crate::method::GetArgs;
    use crate::methods::MailboxGet;
    use crate::request::RequestBuilder;
    use crate::transport::{BoxFuture, StreamingResponse, TransportError};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Records requests and replays canned responses.
    #[derive(Default)]
    pub(crate) struct FakeTransport {
        pub(crate) requests: Mutex<Vec<HttpRequest>>,
        pub(crate) responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
        pub(crate) streams: Mutex<VecDeque<StreamingResponse>>,
    }

    impl FakeTransport {
        pub(crate) fn reply(&self, status: u16, headers: &[(&str, &str)], body: &str) {
            self.responses.lock().unwrap().push_back(Ok(HttpResponse {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                body: Bytes::from(body.to_owned()),
            }));
        }

        pub(crate) fn last_request(&self) -> HttpRequest {
            self.requests.lock().unwrap().last().cloned().unwrap()
        }
    }

    impl Transport for FakeTransport {
        fn send(
            &self,
            req: HttpRequest,
        ) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
            self.requests.lock().unwrap().push(req);
            let next = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportError::Closed));
            Box::pin(async move { next })
        }

        fn stream(
            &self,
            req: HttpRequest,
        ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
            self.requests.lock().unwrap().push(req);
            let next = self.streams.lock().unwrap().pop_front();
            Box::pin(async move { next.ok_or(TransportError::Closed) })
        }
    }

    pub(crate) const SESSION: &str = r#"{
        "capabilities": {"urn:ietf:params:jmap:core": {"maxSizeUpload": 10}, "urn:ietf:params:jmap:mail": {}},
        "accounts": {"A1": {"name": "robin@example.com", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {}}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
        "username": "robin@example.com",
        "apiUrl": "/jmap/api/",
        "downloadUrl": "https://dl.example.net/jmap/download/{accountId}/{blobId}/{name}?type={type}",
        "uploadUrl": "/jmap/upload/{accountId}/",
        "eventSourceUrl": "/jmap/event/",
        "state": "S1"
    }"#;

    pub(crate) fn client_with(fake: &Arc<FakeTransport>) -> Client {
        Client::new(
            fake.clone(),
            "https://jmap.example.com/jmap/session",
            Secret::new("tok-123"),
        )
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn futures_are_send() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        let request = RequestBuilder::new().build().unwrap();
        assert_send(&client.fetch_session());
        assert_send(&client.call(&request));
        assert_send(&client.upload(&Id::from("A1"), "text/plain", Bytes::new()));
    }

    #[test]
    fn fetch_session_sends_auth_and_user_agent() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(200, &[], SESSION);
        let client = client_with(&fake);
        let session = block_on(client.fetch_session()).unwrap();
        assert_eq!(session.state, "S1");
        let req = fake.last_request();
        assert_eq!(req.method, HttpMethod::Get);
        assert_eq!(req.url, "https://jmap.example.com/jmap/session");
        assert_eq!(req.header("authorization"), Some("Bearer tok-123"));
        assert_eq!(req.header("user-agent"), Some(USER_AGENT));
        assert!(USER_AGENT.starts_with("mejl/"));
        assert!(!USER_AGENT.contains('@'));
        assert!(!format!("{client:?}").contains("tok-123"));
    }

    #[test]
    fn call_posts_to_resolved_api_url_and_flags_session_change() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        client.set_session(serde_json::from_str(SESSION).unwrap());
        let mut req = RequestBuilder::new();
        let h = req.call::<MailboxGet>(GetArgs::all("A1"));
        let request = req.build().unwrap();

        fake.reply(
            200,
            &[],
            r#"{"methodResponses":[["Mailbox/get",{"accountId":"A1","state":"m1","list":[],"notFound":[]},"c0"]],"sessionState":"S1"}"#,
        );
        let resp = block_on(client.call(&request)).unwrap();
        assert!(!resp.session_changed);
        assert_eq!(resp.get(&h).unwrap().state, "m1");
        let sent = fake.last_request();
        assert_eq!(sent.url, "https://jmap.example.com/jmap/api/");
        assert_eq!(sent.header("content-type"), Some("application/json"));
        let body: serde_json::Value =
            serde_json::from_slice(sent.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["using"][0], "urn:ietf:params:jmap:core");

        fake.reply(200, &[], r#"{"methodResponses":[],"sessionState":"S2"}"#);
        assert!(block_on(client.call(&request)).unwrap().session_changed);
        fake.reply(200, &[], r#"{"methodResponses":[]}"#);
        assert!(!block_on(client.call(&request)).unwrap().session_changed);
    }

    #[test]
    fn call_without_session_fails() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        let request = RequestBuilder::new().build().unwrap();
        assert!(matches!(
            block_on(client.call(&request)),
            Err(Error::NoSession)
        ));
        assert!(fake.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn status_mapping() {
        let h = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect()
        };
        assert!(check_status(204, &[], b"").is_ok());
        match check_status(
            401,
            &h(&[("WWW-Authenticate", "Bearer resource_metadata=\"x\"")]),
            b"",
        ) {
            Err(Error::Unauthorized { www_authenticate }) => {
                assert_eq!(
                    www_authenticate.as_deref(),
                    Some("Bearer resource_metadata=\"x\"")
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            check_status(403, &[], b"forbidden"),
            Err(Error::Unauthorized { .. })
        ));
        match check_status(429, &h(&[("retry-after", "7")]), b"") {
            Err(Error::RateLimited { retry_after }) => {
                assert_eq!(retry_after, Some(Duration::from_secs(7)));
            }
            other => panic!("{other:?}"),
        }
        match check_status(503, &h(&[("retry-after", "garbage")]), b"") {
            Err(Error::ServiceUnavailable { retry_after: None }) => {}
            other => panic!("{other:?}"),
        }
        match check_status(503, &[], b"") {
            Err(Error::ServiceUnavailable { retry_after: None }) => {}
            other => panic!("{other:?}"),
        }
        match check_status(302, &h(&[("location", "https://evil.example.org/")]), b"") {
            Err(Error::Redirect {
                status: 302,
                location,
            }) => {
                assert_eq!(location.as_deref(), Some("https://evil.example.org/"));
            }
            other => panic!("{other:?}"),
        }
        let problem = br#"{"type":"urn:ietf:params:jmap:error:unknownCapability","status":400,"detail":"no"}"#;
        match check_status(
            400,
            &h(&[("content-type", "application/problem+json")]),
            problem,
        ) {
            Err(Error::Request(e)) => assert_eq!(e.kind, RequestErrorType::UnknownCapability),
            other => panic!("{other:?}"),
        }
        // JMAP problem types are recognised even with a plain JSON type.
        match check_status(400, &h(&[("content-type", "application/json")]), problem) {
            Err(Error::Request(_)) => {}
            other => panic!("{other:?}"),
        }
        // Unrelated JSON with a type member is not a JMAP problem.
        match check_status(500, &[], br#"{"type":"oops"}"#) {
            Err(Error::Http { status: 500, body }) => assert_eq!(body, r#"{"type":"oops"}"#),
            other => panic!("{other:?}"),
        }
        let big = vec![b'x'; 5000];
        match check_status(404, &[], &big) {
            Err(Error::Http { status: 404, body }) => assert!(body.len() < 600),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn http_errors_surface_from_call() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        client.set_session(serde_json::from_str(SESSION).unwrap());
        let request = RequestBuilder::new().build().unwrap();
        fake.reply(
            400,
            &[("content-type", "application/problem+json")],
            r#"{"type":"urn:ietf:params:jmap:error:limit","limit":"maxCallsInRequest","status":400}"#,
        );
        match block_on(client.call(&request)) {
            Err(Error::Request(e)) => assert_eq!(e.limit.as_deref(), Some("maxCallsInRequest")),
            other => panic!("{other:?}"),
        }
        fake.reply(200, &[], "not json");
        assert!(matches!(
            block_on(client.call(&request)),
            Err(Error::Decode { .. })
        ));
        assert!(matches!(
            block_on(client.call(&request)),
            Err(Error::Transport(TransportError::Closed))
        ));
    }

    #[test]
    fn upload_expands_account_and_checks_size() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        client.set_session(serde_json::from_str(SESSION).unwrap());
        fake.reply(
            201,
            &[],
            r#"{"accountId":"A/1","blobId":"B1","type":"text/plain","size":5}"#,
        );
        let up =
            block_on(client.upload(&Id::from("A/1"), "text/plain", Bytes::from_static(b"hello")))
                .unwrap();
        assert_eq!(up.blob_id, "B1");
        let req = fake.last_request();
        assert_eq!(req.url, "https://jmap.example.com/jmap/upload/A%2F1/");
        assert_eq!(req.header("content-type"), Some("text/plain"));
        assert_eq!(req.body.as_deref(), Some(&b"hello"[..]));

        let err = block_on(client.upload(&Id::from("A1"), "text/plain", Bytes::from(vec![0; 11])))
            .unwrap_err();
        assert!(matches!(
            err,
            Error::TooLarge {
                size: 11,
                limit: 10
            }
        ));
    }

    #[test]
    fn download_url_is_expanded_and_encoded() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        assert!(matches!(
            client.download_url(&"A1".into(), &"B1".into(), "a.txt", "text/plain"),
            Err(Error::NoSession)
        ));
        client.set_session(serde_json::from_str(SESSION).unwrap());
        let url = client
            .download_url(
                &"A1".into(),
                &"B 1".into(),
                "Q3 report?.pdf",
                "application/pdf",
            )
            .unwrap();
        assert_eq!(
            url,
            "https://dl.example.net/jmap/download/A1/B%201/Q3%20report%3F.pdf?type=application%2Fpdf"
        );
    }

    #[test]
    fn absolute_url_rules() {
        let base = "https://jmap.example.com/jmap/session";
        assert_eq!(
            absolute_url(base, "/a/").unwrap(),
            "https://jmap.example.com/a/"
        );
        assert_eq!(
            absolute_url(base, "https://other.example.org/x").unwrap(),
            "https://other.example.org/x"
        );
        assert!(absolute_url(base, "").is_err());
        assert!(absolute_url(base, "file:///etc/passwd").is_err());
        assert!(absolute_url("not a url", "/a").is_err());
    }

    #[test]
    fn set_token_changes_header() {
        let fake = Arc::new(FakeTransport::default());
        let client = client_with(&fake);
        client.set_token(Secret::new("tok-456"));
        let headers = client.base_headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer tok-456")
        );
    }
}
