//! Errors at each JMAP layer: transport and HTTP, request (RFC 7807 problem
//! details), method (`["error", ...]` responses) and per-object `/set`
//! failures.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transport::TransportError;
use crate::types::core::{Extra, string_enum};

string_enum! {
    /// Method-level error types (RFC 8620 sections 3.6.2 and 5, RFC 8621).
    pub enum MethodErrorType {
        ServerUnavailable => "serverUnavailable",
        ServerFail => "serverFail",
        ServerPartialFail => "serverPartialFail",
        UnknownMethod => "unknownMethod",
        InvalidArguments => "invalidArguments",
        InvalidResultReference => "invalidResultReference",
        Forbidden => "forbidden",
        AccountNotFound => "accountNotFound",
        AccountNotSupportedByMethod => "accountNotSupportedByMethod",
        AccountReadOnly => "accountReadOnly",
        RequestTooLarge => "requestTooLarge",
        CannotCalculateChanges => "cannotCalculateChanges",
        StateMismatch => "stateMismatch",
        FromAccountNotFound => "fromAccountNotFound",
        FromAccountNotSupportedByMethod => "fromAccountNotSupportedByMethod",
        AnchorNotFound => "anchorNotFound",
        UnsupportedSort => "unsupportedSort",
        UnsupportedFilter => "unsupportedFilter",
        TooManyChanges => "tooManyChanges",
        @other Other,
    }
}

/// An `["error", {...}, callId]` method response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind}{}", .description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
pub struct MethodError {
    #[serde(rename = "type")]
    pub kind: MethodErrorType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Extra members, such as `arguments` on `invalidArguments`.
    #[serde(flatten)]
    pub extra: Extra,
}

impl MethodError {
    /// A method error with no description.
    pub fn new(kind: MethodErrorType) -> Self {
        Self {
            kind,
            description: None,
            extra: Extra::new(),
        }
    }

    /// Attach a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Whether retrying the same call later may succeed.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            MethodErrorType::ServerUnavailable
                | MethodErrorType::ServerFail
                | MethodErrorType::ServerPartialFail
        )
    }
}

string_enum! {
    /// Per-object `/set` error types (RFC 8620 section 5.3, RFC 8621).
    pub enum SetErrorType {
        Forbidden => "forbidden",
        OverQuota => "overQuota",
        TooLarge => "tooLarge",
        RateLimit => "rateLimit",
        NotFound => "notFound",
        InvalidPatch => "invalidPatch",
        WillDestroy => "willDestroy",
        InvalidProperties => "invalidProperties",
        Singleton => "singleton",
        AlreadyExists => "alreadyExists",
        MailboxHasChild => "mailboxHasChild",
        MailboxHasEmail => "mailboxHasEmail",
        BlobNotFound => "blobNotFound",
        TooManyKeywords => "tooManyKeywords",
        TooManyMailboxes => "tooManyMailboxes",
        InvalidEmail => "invalidEmail",
        TooManyRecipients => "tooManyRecipients",
        NoRecipients => "noRecipients",
        InvalidRecipients => "invalidRecipients",
        ForbiddenMailFrom => "forbiddenMailFrom",
        ForbiddenFrom => "forbiddenFrom",
        ForbiddenToSend => "forbiddenToSend",
        CannotUnsend => "cannotUnsend",
        @other Other,
    }
}

/// Why one object in a `/set` call was not created, updated or destroyed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind}{}", .description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
pub struct SetError {
    #[serde(rename = "type")]
    pub kind: SetErrorType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The offending properties, for `invalidProperties`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<String>>,
    /// Extra members, such as `existingId`, `notFound`, `maxRecipients` or
    /// Fastmail's `subType`.
    #[serde(flatten)]
    pub extra: Extra,
}

impl SetError {
    /// A set error with no description.
    pub fn new(kind: SetErrorType) -> Self {
        Self {
            kind,
            description: None,
            properties: None,
            extra: Extra::new(),
        }
    }

    /// A string extra member, such as `subType` or `existingId`.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key)?.as_str()
    }

    /// Fastmail's `subType`, for example `addressInUse`.
    pub fn sub_type(&self) -> Option<&str> {
        self.extra_str("subType")
    }
}

string_enum! {
    /// Request-level problem types (RFC 8620 section 3.6.1).
    pub enum RequestErrorType {
        UnknownCapability => "urn:ietf:params:jmap:error:unknownCapability",
        NotJson => "urn:ietf:params:jmap:error:notJSON",
        NotRequest => "urn:ietf:params:jmap:error:notRequest",
        Limit => "urn:ietf:params:jmap:error:limit",
        @other Other,
    }
}

/// An RFC 7807 problem details response to a whole request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("request rejected: {kind}{}", .detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
pub struct RequestError {
    #[serde(rename = "type")]
    pub kind: RequestErrorType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// For `limit`: the name of the limit that was hit, such as
    /// `maxSizeRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl RequestError {
    /// Parse a problem details body. Only bodies with a `type` member are
    /// accepted, so an unrelated JSON error page is not mistaken for one.
    pub fn from_body(body: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        value.get("type")?.as_str()?;
        Self::deserialize(&value).ok()
    }
}

string_enum! {
    /// OAuth error codes: RFC 6749 sections 4.1.2.1 and 5.2, RFC 7591
    /// section 3.2.2 and RFC 7009 section 2.2.1.
    pub enum OAuthErrorKind {
        InvalidRequest => "invalid_request",
        InvalidClient => "invalid_client",
        InvalidGrant => "invalid_grant",
        UnauthorizedClient => "unauthorized_client",
        UnsupportedGrantType => "unsupported_grant_type",
        UnsupportedResponseType => "unsupported_response_type",
        UnsupportedTokenType => "unsupported_token_type",
        InvalidScope => "invalid_scope",
        AccessDenied => "access_denied",
        ServerError => "server_error",
        TemporarilyUnavailable => "temporarily_unavailable",
        InvalidRedirectUri => "invalid_redirect_uri",
        InvalidClientMetadata => "invalid_client_metadata",
        @other Other,
    }
}

/// An OAuth error response, from a token, registration or revocation
/// endpoint or from the authorization redirect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "OAuth error {error}{}{}",
    .description.as_deref().map(|d| format!(": {d}")).unwrap_or_default(),
    if *.temporary { " (temporary)" } else { "" }
)]
pub struct OAuthError {
    /// The `error` code.
    pub error: OAuthErrorKind,
    /// The `error_description`, when the server gave one.
    pub description: Option<String>,
    /// Fastmail's `temporary` member on `invalid_grant`: the login is
    /// blocked for now, for example by an admin lock, but not revoked.
    pub temporary: bool,
}

impl OAuthError {
    /// An error with no description.
    pub fn new(error: OAuthErrorKind) -> Self {
        Self {
            error,
            description: None,
            temporary: false,
        }
    }

    /// Parse an error body. Only a JSON object with a string `error` is
    /// accepted. `temporary` may be a boolean or the string `"true"`.
    pub fn from_body(body: &[u8]) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        let error = value.get("error")?.as_str()?;
        if error.is_empty() {
            return None;
        }
        let temporary = match value.get("temporary") {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        };
        Some(Self {
            error: OAuthErrorKind::from(error),
            description: value
                .get("error_description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            temporary,
        })
    }

    /// Whether the same request may work later: the server had a problem.
    /// A `temporary` `invalid_grant` is not transient here: the login is
    /// blocked until something outside mejl changes.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.error,
            OAuthErrorKind::ServerError | OAuthErrorKind::TemporarilyUnavailable
        )
    }
}

/// Anything that can go wrong talking to a JMAP server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// `call` or a blob method was used before a session was loaded.
    #[error("no session loaded")]
    NoSession,
    #[error("invalid URL {url:?}: {reason}")]
    InvalidUrl { url: String, reason: String },
    /// HTTP 401 or 403. The token is wrong, expired, revoked or lacks a
    /// scope.
    #[error("not authorized (HTTP {status})")]
    Unauthorized {
        /// 401 for a token the server does not accept, which an OAuth
        /// client answers with a refresh. 403 for a token that lacks a
        /// scope or was revoked, which a refresh does not fix.
        status: u16,
        /// The `WWW-Authenticate` header, which points at OAuth metadata.
        www_authenticate: Option<String>,
    },
    /// The OAuth server refused a request with an RFC 6749 error, such as
    /// `invalid_grant` for a refresh token that no longer works.
    #[error(transparent)]
    OAuth(Box<OAuthError>),
    /// A token refresh got no usable response twice in a row. The refresh
    /// token may have been used up, so the login has to be renewed.
    #[error("the OAuth token refresh got no response")]
    RefreshLost,
    /// OAuth metadata, a registration or a token response mejl cannot use.
    #[error("unusable OAuth response: {0}")]
    InvalidOAuth(String),
    /// The system's random number source failed.
    #[error("cannot get random bytes: {0}")]
    Random(String),
    /// HTTP 429.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    /// HTTP 503.
    #[error("service unavailable")]
    ServiceUnavailable { retry_after: Option<Duration> },
    /// A redirect the transport would not follow, such as one to another
    /// host, which must not receive the token.
    #[error("unfollowed redirect (HTTP {status}) to {location:?}")]
    Redirect {
        status: u16,
        location: Option<String>,
    },
    /// A request-level problem details response.
    #[error(transparent)]
    Request(Box<RequestError>),
    /// Any other unexpected HTTP status.
    #[error("HTTP {status}: {body}")]
    Http {
        status: u16,
        /// The start of the body, for diagnostics.
        body: String,
    },
    /// The server sent something that does not decode.
    #[error("cannot decode {context}: {source}")]
    Decode {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    /// Arguments could not be encoded.
    #[error("cannot encode {context}: {source}")]
    Encode {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    /// The server answered a call with a method error.
    #[error("{name} ({call_id}) failed: {error}")]
    Method {
        call_id: String,
        name: String,
        error: Box<MethodError>,
    },
    /// The response has no result for a call.
    #[error("no {name} response for call {call_id}")]
    MissingResponse { call_id: String, name: String },
    /// A blob is larger than the session's `maxSizeUpload`.
    #[error("upload of {size} bytes exceeds the server limit of {limit}")]
    TooLarge { size: u64, limit: u64 },
    /// An `EventSource` response that is not `text/event-stream`.
    #[error("unexpected content type {0:?}")]
    UnexpectedContentType(String),
}

impl Error {
    /// The server's requested wait, if it gave one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::RateLimited { retry_after } | Error::ServiceUnavailable { retry_after } => {
                *retry_after
            }
            _ => None,
        }
    }

    /// Whether the credentials were rejected: a 401 or 403, an OAuth error
    /// other than a passing server problem, or a refresh that got no
    /// response. Each needs new credentials.
    pub fn is_unauthorized(&self) -> bool {
        match self {
            Error::Unauthorized { .. } | Error::RefreshLost => true,
            Error::OAuth(e) => !e.is_transient(),
            _ => false,
        }
    }

    /// The OAuth error, if this is one. Its `temporary` flag tells a
    /// passing lock from a revoked login.
    pub fn oauth_error(&self) -> Option<&OAuthError> {
        match self {
            Error::OAuth(e) => Some(e),
            _ => None,
        }
    }

    /// Whether the same request may succeed later without changes: network
    /// trouble, rate limits, 5xx statuses and transient method errors.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport(_) => true,
            Error::RateLimited { .. } | Error::ServiceUnavailable { .. } => true,
            Error::Http { status, .. } => *status >= 500,
            Error::Method { error, .. } => error.is_transient(),
            Error::OAuth(e) => e.is_transient(),
            _ => false,
        }
    }

    /// The method error, if this is one.
    pub fn method_error(&self) -> Option<&MethodError> {
        match self {
            Error::Method { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Whether this is a method error of the given type, such as
    /// `cannotCalculateChanges`.
    pub fn is_method_error(&self, kind: &MethodErrorType) -> bool {
        self.method_error().is_some_and(|e| &e.kind == kind)
    }
}

/// Parse a `Retry-After` value: delay seconds or an HTTP-date.
///
/// A date in the past gives zero. Garbage or an out-of-range value gives
/// `None`.
pub fn parse_retry_after(value: &str, now: jiff::Timestamp) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(Duration::from_secs);
    }
    let at = parse_http_date(value)?;
    let span = at.duration_since(now);
    if span.is_negative() {
        return Some(Duration::ZERO);
    }
    Duration::try_from(span).ok()
}

/// Parse the three HTTP-date formats of RFC 9110 section 5.6.7.
fn parse_http_date(value: &str) -> Option<jiff::Timestamp> {
    use jiff::fmt::{rfc2822, strtime};
    use jiff::tz::TimeZone;

    // IMF-fixdate, the only format current servers send.
    if let Ok(ts) = rfc2822::DateTimeParser::new().parse_timestamp(value) {
        return Some(ts);
    }
    // Obsolete RFC 850 and asctime formats, both in GMT.
    for format in ["%A, %d-%b-%y %H:%M:%S GMT", "%a %b %e %H:%M:%S %Y"] {
        if let Ok(tm) = strtime::parse(format, value)
            && let Ok(dt) = tm.to_datetime()
            && let Ok(zoned) = dt.to_zoned(TimeZone::UTC)
        {
            return Some(zoned.timestamp());
        }
    }
    None
}

/// Keep at most `max` bytes of a body for an error message, cut at a char
/// boundary.
pub(crate) fn body_snippet(body: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(body);
    if text.len() <= max {
        return text.into_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> jiff::Timestamp {
        "2026-10-23T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn retry_after_seconds() {
        assert_eq!(
            parse_retry_after("120", now()),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after(" 0 ", now()), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("99999999999999999999999", now()), None);
        assert_eq!(parse_retry_after("-5", now()), None);
        assert_eq!(parse_retry_after("1.5", now()), None);
        assert_eq!(parse_retry_after("", now()), None);
        assert_eq!(parse_retry_after("soon", now()), None);
    }

    #[test]
    fn retry_after_http_dates() {
        assert_eq!(
            parse_retry_after("Fri, 23 Oct 2026 12:01:30 GMT", now()),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            parse_retry_after("Friday, 23-Oct-26 12:00:10 GMT", now()),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            parse_retry_after("Fri Oct 23 12:00:05 2026", now()),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_retry_after("Fri Oct  2 12:00:00 2026", now()),
            Some(Duration::ZERO),
            "past dates clamp to zero"
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now()),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_retry_after("Fri, 99 Oct 2026 12:00:00 GMT", now()),
            None
        );
    }

    #[test]
    fn method_error_parses_known_and_unknown() {
        let e: MethodError =
            serde_json::from_value(json!({"type": "cannotCalculateChanges"})).unwrap();
        assert_eq!(e.kind, MethodErrorType::CannotCalculateChanges);
        let e: MethodError = serde_json::from_value(json!({
            "type": "invalidArguments",
            "description": "bad",
            "arguments": ["ids"]
        }))
        .unwrap();
        assert_eq!(e.kind, MethodErrorType::InvalidArguments);
        assert_eq!(e.to_string(), "invalidArguments: bad");
        assert_eq!(e.extra["arguments"], json!(["ids"]));
        let e: MethodError = serde_json::from_value(json!({"type": "x-new"})).unwrap();
        assert_eq!(e.kind, MethodErrorType::Other("x-new".into()));
    }

    #[test]
    fn set_error_fields() {
        let e: SetError = serde_json::from_value(json!({
            "type": "invalidProperties",
            "properties": ["emailPrefix"],
            "description": "Name is reserved"
        }))
        .unwrap();
        assert_eq!(e.kind, SetErrorType::InvalidProperties);
        assert_eq!(
            e.properties.as_deref(),
            Some(&["emailPrefix".to_string()][..])
        );
        let e: SetError =
            serde_json::from_value(json!({"type": "forbidden", "subType": "addressInUse"}))
                .unwrap();
        assert_eq!(e.sub_type(), Some("addressInUse"));
        let e: SetError = serde_json::from_value(json!({"type": "rateLimit"})).unwrap();
        assert_eq!(e.kind, SetErrorType::RateLimit);
    }

    #[test]
    fn problem_details() {
        let body = br#"{"type":"urn:ietf:params:jmap:error:limit","limit":"maxSizeRequest","status":400,"detail":"too big"}"#;
        let e = RequestError::from_body(body).unwrap();
        assert_eq!(e.kind, RequestErrorType::Limit);
        assert_eq!(e.limit.as_deref(), Some("maxSizeRequest"));
        assert_eq!(e.status, Some(400));
        let e = RequestError::from_body(
            br#"{"type":"urn:ietf:params:jmap:error:notJSON","status":400}"#,
        )
        .unwrap();
        assert_eq!(e.kind, RequestErrorType::NotJson);
        assert!(RequestError::from_body(b"<html>").is_none());
        assert!(RequestError::from_body(br#"{"error":"x"}"#).is_none());
        assert!(RequestError::from_body(br#"{"type":7}"#).is_none());
    }

    #[test]
    fn oauth_error_bodies() {
        let e = OAuthError::from_body(
            br#"{"error":"invalid_grant","error_description":"Account locked","temporary":true}"#,
        )
        .unwrap();
        assert_eq!(e.error, OAuthErrorKind::InvalidGrant);
        assert_eq!(e.description.as_deref(), Some("Account locked"));
        assert!(e.temporary);
        assert_eq!(
            e.to_string(),
            "OAuth error invalid_grant: Account locked (temporary)"
        );
        let e = OAuthError::from_body(br#"{"error":"invalid_grant"}"#).unwrap();
        assert!(!e.temporary);
        assert_eq!(e.to_string(), "OAuth error invalid_grant");
        let e = OAuthError::from_body(br#"{"error":"invalid_grant","temporary":"true"}"#).unwrap();
        assert!(e.temporary);
        let e = OAuthError::from_body(br#"{"error":"slow_down"}"#).unwrap();
        assert_eq!(e.error, OAuthErrorKind::Other("slow_down".into()));
        assert!(
            OAuthError::from_body(br#"{"error":"temporarily_unavailable"}"#)
                .unwrap()
                .is_transient()
        );
        assert!(OAuthError::from_body(b"<html>").is_none());
        assert!(OAuthError::from_body(br#"{"error":7}"#).is_none());
        assert!(OAuthError::from_body(br#"{"error":""}"#).is_none());
        assert!(OAuthError::from_body(br#"{"type":"about:blank"}"#).is_none());
    }

    #[test]
    fn snippet_cuts_at_char_boundary() {
        assert_eq!(body_snippet(b"short", 10), "short");
        let s = body_snippet("ééé".as_bytes(), 3);
        assert_eq!(s, "é...");
    }

    #[test]
    fn error_classification() {
        assert!(Error::RateLimited { retry_after: None }.is_transient());
        assert!(
            Error::Http {
                status: 502,
                body: String::new()
            }
            .is_transient()
        );
        assert!(
            !Error::Http {
                status: 404,
                body: String::new()
            }
            .is_transient()
        );
        assert!(
            Error::Unauthorized {
                status: 401,
                www_authenticate: None
            }
            .is_unauthorized()
        );
        assert!(Error::RefreshLost.is_unauthorized());
        let grant = Error::OAuth(Box::new(OAuthError::new(OAuthErrorKind::InvalidGrant)));
        assert!(grant.is_unauthorized());
        assert!(!grant.is_transient());
        let busy = Error::OAuth(Box::new(OAuthError::new(
            OAuthErrorKind::TemporarilyUnavailable,
        )));
        assert!(!busy.is_unauthorized());
        assert!(busy.is_transient());
        assert!(!Error::InvalidOAuth("x".into()).is_unauthorized());
        let e = Error::Method {
            call_id: "c0".into(),
            name: "Email/changes".into(),
            error: Box::new(MethodError::new(MethodErrorType::CannotCalculateChanges)),
        };
        assert!(e.is_method_error(&MethodErrorType::CannotCalculateChanges));
        assert!(!e.is_transient());
        // serverFail is an unexpected server error; mejl-sync retries it
        // with backoff, so the shared classification agrees.
        for kind in [
            MethodErrorType::ServerFail,
            MethodErrorType::ServerUnavailable,
            MethodErrorType::ServerPartialFail,
        ] {
            assert!(MethodError::new(kind).is_transient());
        }
        assert_eq!(
            Error::ServiceUnavailable {
                retry_after: Some(Duration::from_secs(3))
            }
            .retry_after(),
            Some(Duration::from_secs(3))
        );
    }
}
