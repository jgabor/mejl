//! OAuth 2.0 for a native app, as Fastmail and
//! draft-ietf-mailmaint-oauth-public describe it.
//!
//! - [`discover`]: the 401 `resource_metadata` pointer (RFC 9728), then the
//!   authorization server metadata (RFC 8414).
//! - [`register`]: open dynamic client registration (RFC 7591).
//! - [`Pkce`] and [`random_state`]: PKCE with S256 (RFC 7636) and the
//!   `state` value.
//! - [`authorize_url`], [`exchange_code`], [`refresh`] and [`revoke`]: the
//!   authorization code grant (RFC 6749) and revocation (RFC 7009).
//! - [`parse_callback`]: the query of the loopback redirect.
//!
//! Everything is plain HTTP over a [`Transport`]. No request here carries a
//! bearer token. Every endpoint must be https, or http on a loopback host.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::client::{USER_AGENT, absolute_url};
use crate::error::{Error, OAuthError, OAuthErrorKind, parse_retry_after};
use crate::secret::Secret;
use crate::transport::{HttpMethod, HttpRequest, HttpResponse, Transport};

/// Scope values mejl may ask for.
pub mod scope {
    /// Full mail access, from draft-ietf-mailmaint-oauth-public.
    pub const MAIL: &str = "urn:ietf:params:oauth:scope:mail";
    /// Contacts, from the same draft.
    pub const CONTACTS: &str = "urn:ietf:params:oauth:scope:contacts";
    /// Ask for a refresh token.
    pub const OFFLINE_ACCESS: &str = "offline_access";
    /// Fastmail's documented JMAP scopes, for a client registered by hand.
    pub const JMAP_CORE: &str = "urn:ietf:params:jmap:core";
    /// JMAP mail.
    pub const JMAP_MAIL: &str = "urn:ietf:params:jmap:mail";
    /// JMAP submission.
    pub const JMAP_SUBMISSION: &str = "urn:ietf:params:jmap:submission";
    /// JMAP contacts.
    pub const JMAP_CONTACTS: &str = "urn:ietf:params:jmap:contacts";
    /// Fastmail's Masked Email.
    pub const MASKED_EMAIL: &str = "https://www.fastmail.com/dev/maskedemail";
}

/// The redirect URI mejl registers. The loopback listener adds its port
/// when it sends the user to the authorization endpoint, which RFC 8252
/// section 7.3 allows.
pub const REDIRECT_URI: &str = "http://127.0.0.1/";

/// The `client_name` mejl registers.
pub const CLIENT_NAME: &str = "mejl";

/// The access token lifetime assumed when a token response has no
/// `expires_in`. A shorter real lifetime is caught by the 401 refresh.
const DEFAULT_LIFETIME_SECS: i64 = 3600;

/// The longest `expires_in` taken at face value, ten years.
const MAX_LIFETIME_SECS: i64 = 10 * 365 * 24 * 3600;

/// What [`discover`] learnt about the authorization server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerMetadata {
    /// The issuer. The `iss` of the authorization response must equal it.
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    pub scopes_supported: Vec<String>,
    /// RFC 9207: the authorization response carries `iss`, so a callback
    /// without one must be refused.
    pub authorization_response_iss_parameter_supported: bool,
}

/// An access token, its refresh token and when the access token expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access: Secret,
    pub refresh: Secret,
    pub expires_at: Timestamp,
    /// The granted scope values. Empty when the server did not say.
    pub scope: Vec<String>,
}

impl TokenSet {
    /// Whether the access token expires within `margin` of `now`.
    pub fn expires_within(&self, margin: SignedDuration, now: Timestamp) -> bool {
        now.checked_add(margin)
            .map_or(true, |limit| self.expires_at <= limit)
    }

    /// Whether the granted scope includes `value`.
    pub fn has_scope(&self, value: &str) -> bool {
        self.scope.iter().any(|s| s == value)
    }
}

/// Require https, or http on a loopback host, with a host and no user
/// info. Every OAuth endpoint passes through this.
pub fn check_endpoint(url: &str) -> Result<(), Error> {
    let invalid = |reason: &str| Error::InvalidUrl {
        url: url.to_owned(),
        reason: reason.to_owned(),
    };
    let parsed = reqwest::Url::parse(url).map_err(|e| invalid(&e.to_string()))?;
    let host = parsed.host_str().ok_or_else(|| invalid("no host"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("user info is not allowed"));
    }
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        _ => Err(invalid("must be https, or http on a loopback host")),
    }
}

fn is_loopback_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Find the authorization server for the JMAP session at `session_url`.
///
/// It fetches the session without a token and reads `resource_metadata`
/// from the 401's `WWW-Authenticate`, then the protected resource metadata
/// it points at, then the authorization server metadata at the RFC 8414
/// well-known path, falling back to `{origin}/.well-known/oauth-authorization-server`.
/// Without a `resource_metadata` pointer the session URL's origin is taken
/// as the issuer. The metadata's `issuer` must match the issuer it was
/// looked up for.
pub async fn discover(
    transport: &dyn Transport,
    session_url: &str,
) -> Result<ServerMetadata, Error> {
    check_endpoint(session_url)?;
    let resp = get(transport, session_url).await?;
    let pointer = if resp.status == 401 {
        resp.header("www-authenticate")
            .and_then(resource_metadata_url)
    } else {
        None
    };
    let issuer = match pointer {
        Some(url) => {
            let url = absolute_url(session_url, &url)?;
            check_endpoint(&url)?;
            let body = fetch_document(transport, &url, "protected resource metadata").await?;
            parse_resource_metadata(&body)?
        }
        None => origin(session_url)?,
    };
    check_endpoint(&issuer)?;
    let mut last = None;
    for url in metadata_urls(&issuer)? {
        match fetch_document(transport, &url, "authorization server metadata").await {
            Ok(body) => return parse_server_metadata(&body, &issuer),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| Error::InvalidOAuth("no metadata URL".into())))
}

/// The `resource_metadata` parameter of a `WWW-Authenticate` header
/// (RFC 9728 section 5.1).
pub fn resource_metadata_url(www_authenticate: &str) -> Option<String> {
    auth_param(www_authenticate, "resource_metadata")
}

/// One parameter of a challenge, quoted or not. Parameter names are
/// matched without regard to case.
fn auth_param(header: &str, name: &str) -> Option<String> {
    let bytes = header.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Read a token, which is a scheme or a parameter name.
        let start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b',' | b' ' | b'\t') {
            i += 1;
        }
        let token = &header[start..i];
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' && !token.is_empty() {
            i += 1;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                i += 1;
            }
            let value = if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
                let mut value = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                    }
                    let ch = header[i..].chars().next()?;
                    value.push(ch);
                    i += ch.len_utf8();
                }
                i += 1;
                value
            } else {
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b',' | b' ' | b'\t') {
                    i += 1;
                }
                header[start..i].to_owned()
            };
            if token.eq_ignore_ascii_case(name) {
                return Some(value);
            }
        }
        while i < bytes.len() && matches!(bytes[i], b',' | b' ' | b'\t') {
            i += 1;
        }
    }
    None
}

/// `scheme://host[:port]` of a URL.
fn origin(url: &str) -> Result<String, Error> {
    let parsed = reqwest::Url::parse(url).map_err(|e| Error::InvalidUrl {
        url: url.to_owned(),
        reason: e.to_string(),
    })?;
    Ok(parsed.origin().ascii_serialization())
}

/// The RFC 8414 metadata URL for an issuer, then the origin fallback when
/// it differs.
fn metadata_urls(issuer: &str) -> Result<Vec<String>, Error> {
    let base = origin(issuer)?;
    let parsed = reqwest::Url::parse(issuer).map_err(|e| Error::InvalidUrl {
        url: issuer.to_owned(),
        reason: e.to_string(),
    })?;
    let path = parsed.path().trim_end_matches('/');
    let fallback = format!("{base}/.well-known/oauth-authorization-server");
    if path.is_empty() {
        return Ok(vec![fallback]);
    }
    Ok(vec![
        format!("{base}/.well-known/oauth-authorization-server{path}"),
        fallback,
    ])
}

fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The first authorization server named by protected resource metadata.
fn parse_resource_metadata(body: &[u8]) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        authorization_servers: Option<Vec<String>>,
    }
    let raw: Raw = serde_json::from_slice(body).map_err(|e| {
        Error::InvalidOAuth(format!("protected resource metadata does not decode: {e}"))
    })?;
    raw.authorization_servers
        .unwrap_or_default()
        .into_iter()
        .find(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::InvalidOAuth("protected resource metadata names no authorization server".into())
        })
}

fn parse_server_metadata(body: &[u8], issuer: &str) -> Result<ServerMetadata, Error> {
    #[derive(Deserialize)]
    struct Raw {
        issuer: String,
        authorization_endpoint: Option<String>,
        token_endpoint: Option<String>,
        registration_endpoint: Option<String>,
        revocation_endpoint: Option<String>,
        scopes_supported: Option<Vec<String>>,
        response_types_supported: Option<Vec<String>>,
        code_challenge_methods_supported: Option<Vec<String>>,
        #[serde(default)]
        authorization_response_iss_parameter_supported: Option<bool>,
    }
    let bad = |why: &str| Error::InvalidOAuth(format!("authorization server metadata {why}"));
    let raw: Raw =
        serde_json::from_slice(body).map_err(|e| bad(&format!("does not decode: {e}")))?;
    if !same_issuer(&raw.issuer, issuer) {
        return Err(bad(&format!(
            "is for issuer {:?}, not {issuer:?}",
            raw.issuer
        )));
    }
    if let Some(types) = &raw.response_types_supported
        && !types.iter().any(|t| t == "code")
    {
        return Err(bad("does not support the code response type"));
    }
    if let Some(methods) = &raw.code_challenge_methods_supported
        && !methods.iter().any(|m| m == "S256")
    {
        return Err(bad("does not support PKCE with S256"));
    }
    let authorization_endpoint = raw
        .authorization_endpoint
        .ok_or_else(|| bad("has no authorization_endpoint"))?;
    let token_endpoint = raw
        .token_endpoint
        .ok_or_else(|| bad("has no token_endpoint"))?;
    let optional = |url: Option<String>| -> Result<Option<String>, Error> {
        match url.filter(|u| !u.is_empty()) {
            Some(url) => {
                check_endpoint(&url)?;
                Ok(Some(url))
            }
            None => Ok(None),
        }
    };
    check_endpoint(&authorization_endpoint)?;
    check_endpoint(&token_endpoint)?;
    Ok(ServerMetadata {
        issuer: raw.issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint: optional(raw.registration_endpoint)?,
        revocation_endpoint: optional(raw.revocation_endpoint)?,
        scopes_supported: raw.scopes_supported.unwrap_or_default(),
        authorization_response_iss_parameter_supported: raw
            .authorization_response_iss_parameter_supported
            .unwrap_or(false),
    })
}

/// Register mejl as a public client (RFC 7591) and return its `client_id`.
pub async fn register(
    transport: &dyn Transport,
    registration_endpoint: &str,
) -> Result<String, Error> {
    check_endpoint(registration_endpoint)?;
    let body = serde_json::json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [REDIRECT_URI],
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
    });
    let req = HttpRequest {
        method: HttpMethod::Post,
        url: registration_endpoint.to_owned(),
        headers: vec![
            ("user-agent".into(), USER_AGENT.into()),
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "application/json".into()),
        ],
        body: Some(Bytes::from(body.to_string())),
    };
    let resp = transport.send(req).await?;
    if !(200..300).contains(&resp.status) {
        return Err(endpoint_error(&resp));
    }
    let value: serde_json::Value = serde_json::from_slice(&resp.body)
        .map_err(|e| Error::InvalidOAuth(format!("registration response does not decode: {e}")))?;
    value
        .get("client_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidOAuth("registration response has no client_id".into()))
}

/// A PKCE verifier and its S256 challenge.
#[derive(Debug, Clone)]
pub struct Pkce {
    verifier: Secret,
    challenge: String,
}

impl Pkce {
    /// A fresh verifier: 32 random bytes as base64url, 43 characters.
    pub fn generate() -> Result<Self, Error> {
        Ok(Self::from_verifier(Secret::new(random_string(32)?)))
    }

    /// The pair for a known verifier.
    pub fn from_verifier(verifier: Secret) -> Self {
        let challenge = s256_challenge(verifier.expose());
        Self {
            verifier,
            challenge,
        }
    }

    /// The `code_verifier`, sent with the code exchange.
    pub fn verifier(&self) -> &Secret {
        &self.verifier
    }

    /// The `code_challenge`, sent to the authorization endpoint.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// base64url without padding of the SHA-256 of `verifier`.
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A random `state` value: 16 bytes as base64url.
pub fn random_state() -> Result<String, Error> {
    random_string(16)
}

fn random_string(bytes: usize) -> Result<String, Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Error::Random(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

/// The URL to open in the browser. `redirect_uri` is the loopback URL with
/// the listener's port; the same value goes to [`exchange_code`].
pub fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[impl AsRef<str>],
    state: &str,
    pkce: &Pkce,
) -> Result<String, Error> {
    check_endpoint(authorization_endpoint)?;
    let mut url = reqwest::Url::parse(authorization_endpoint).map_err(|e| Error::InvalidUrl {
        url: authorization_endpoint.to_owned(),
        reason: e.to_string(),
    })?;
    let scope = scopes
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(" ");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scope)
        .append_pair("state", state)
        .append_pair("code_challenge", pkce.challenge())
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

/// Exchange an authorization code for tokens. `redirect_uri` must be the
/// one sent to the authorization endpoint.
pub async fn exchange_code(
    transport: &dyn Transport,
    token_endpoint: &str,
    client_id: &str,
    code: &Secret,
    redirect_uri: &str,
    pkce: &Pkce,
) -> Result<TokenSet, Error> {
    check_endpoint(token_endpoint)?;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.expose()),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", pkce.verifier().expose()),
    ];
    match token_request(transport, token_endpoint, &form, None).await? {
        TokenReply::Tokens(tokens) => Ok(tokens),
        TokenReply::Unusable(why) => Err(Error::InvalidOAuth(why)),
    }
}

/// Trade the refresh token in `tokens` for a new set. When the response
/// leaves out a new refresh token or the scope, the old ones are kept.
///
/// The refresh token rotates on every use at Fastmail and must not be sent
/// again after this succeeds. A [`Client`](crate::Client) built with
/// OAuth credentials does this itself; call this only outside one.
pub async fn refresh(
    transport: &dyn Transport,
    token_endpoint: &str,
    client_id: &str,
    tokens: &TokenSet,
) -> Result<TokenSet, Error> {
    check_endpoint(token_endpoint)?;
    match refresh_reply(transport, token_endpoint, client_id, tokens).await? {
        TokenReply::Tokens(tokens) => Ok(tokens),
        TokenReply::Unusable(why) => Err(Error::InvalidOAuth(why)),
    }
}

/// Revoke a refresh token, which ends its whole grant (RFC 7009).
pub async fn revoke(
    transport: &dyn Transport,
    revocation_endpoint: &str,
    client_id: &str,
    refresh_token: &Secret,
) -> Result<(), Error> {
    check_endpoint(revocation_endpoint)?;
    let form = [
        ("token", refresh_token.expose()),
        ("token_type_hint", "refresh_token"),
        ("client_id", client_id),
    ];
    let resp = post_form(transport, revocation_endpoint, &form).await?;
    if (200..300).contains(&resp.status) {
        Ok(())
    } else {
        Err(endpoint_error(&resp))
    }
}

/// A 2xx token response, usable or not. An unusable one may still have
/// rotated the refresh token, so the caller treats it as lost.
pub(crate) enum TokenReply {
    Tokens(TokenSet),
    Unusable(String),
}

pub(crate) async fn refresh_reply(
    transport: &dyn Transport,
    token_endpoint: &str,
    client_id: &str,
    tokens: &TokenSet,
) -> Result<TokenReply, Error> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", tokens.refresh.expose()),
        ("client_id", client_id),
    ];
    token_request(transport, token_endpoint, &form, Some(tokens)).await
}

async fn token_request(
    transport: &dyn Transport,
    token_endpoint: &str,
    form: &[(&str, &str)],
    previous: Option<&TokenSet>,
) -> Result<TokenReply, Error> {
    let resp = post_form(transport, token_endpoint, form).await?;
    if !(200..300).contains(&resp.status) {
        return Err(endpoint_error(&resp));
    }
    Ok(
        match parse_token_response(&resp.body, Timestamp::now(), previous) {
            Ok(tokens) => TokenReply::Tokens(tokens),
            Err(why) => TokenReply::Unusable(why),
        },
    )
}

/// Parse a successful token response (RFC 6749 section 5.1).
fn parse_token_response(
    body: &[u8],
    now: Timestamp,
    previous: Option<&TokenSet>,
) -> Result<TokenSet, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("token response does not decode: {e}"))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let access = text("access_token").ok_or("token response has no access_token")?;
    if let Some(kind) = text("token_type")
        && !kind.eq_ignore_ascii_case("bearer")
    {
        return Err(format!("token type {kind:?} is not bearer"));
    }
    let refresh = match (text("refresh_token"), previous) {
        (Some(new), _) => Secret::new(new),
        (None, Some(old)) => old.refresh.clone(),
        (None, None) => return Err("token response has no refresh_token".into()),
    };
    let lifetime = match value.get("expires_in") {
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(DEFAULT_LIFETIME_SECS),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(DEFAULT_LIFETIME_SECS),
        _ => DEFAULT_LIFETIME_SECS,
    }
    .clamp(0, MAX_LIFETIME_SECS);
    let expires_at = now
        .checked_add(SignedDuration::from_secs(lifetime))
        .unwrap_or(now);
    let scope = match text("scope") {
        Some(scope) => scope.split_whitespace().map(str::to_owned).collect(),
        None => previous.map(|p| p.scope.clone()).unwrap_or_default(),
    };
    Ok(TokenSet {
        access: Secret::new(access),
        refresh,
        expires_at,
        scope,
    })
}

async fn get(transport: &dyn Transport, url: &str) -> Result<HttpResponse, Error> {
    let req = HttpRequest {
        method: HttpMethod::Get,
        url: url.to_owned(),
        headers: vec![
            ("user-agent".into(), USER_AGENT.into()),
            ("accept".into(), "application/json".into()),
        ],
        body: None,
    };
    Ok(transport.send(req).await?)
}

async fn fetch_document(transport: &dyn Transport, url: &str, what: &str) -> Result<Bytes, Error> {
    let resp = get(transport, url).await?;
    if (200..300).contains(&resp.status) {
        Ok(resp.body)
    } else {
        Err(Error::InvalidOAuth(format!(
            "{what} at {url} answered HTTP {}",
            resp.status
        )))
    }
}

async fn post_form(
    transport: &dyn Transport,
    url: &str,
    form: &[(&str, &str)],
) -> Result<HttpResponse, Error> {
    let req = HttpRequest {
        method: HttpMethod::Post,
        url: url.to_owned(),
        headers: vec![
            ("user-agent".into(), USER_AGENT.into()),
            (
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("accept".into(), "application/json".into()),
        ],
        body: Some(Bytes::from(form_encode(form))),
    };
    Ok(transport.send(req).await?)
}

/// `application/x-www-form-urlencoded`, with everything but the unreserved
/// characters escaped, so `+` goes out as `%2B`.
fn form_encode(pairs: &[(&str, &str)]) -> String {
    fn push(out: &mut String, s: &str) {
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(char::from(b));
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        push(&mut out, key);
        out.push('=');
        push(&mut out, value);
    }
    out
}

/// Map an error response from an OAuth endpoint. The body is never put in
/// the error: a token endpoint might echo what it was sent.
fn endpoint_error(resp: &HttpResponse) -> Error {
    if let Some(e) = OAuthError::from_body(&resp.body) {
        return Error::OAuth(Box::new(e));
    }
    let retry_after = || {
        resp.header("retry-after")
            .and_then(|v| parse_retry_after(v, Timestamp::now()))
    };
    match resp.status {
        429 => Error::RateLimited {
            retry_after: retry_after(),
        },
        503 => Error::ServiceUnavailable {
            retry_after: retry_after(),
        },
        status if status >= 500 => Error::Http {
            status,
            body: String::new(),
        },
        status => Error::InvalidOAuth(format!("the server answered HTTP {status}")),
    }
}

/// The parameters of the loopback redirect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Callback {
    /// The path of the request, so the listener can ignore `/favicon.ico`
    /// and the like.
    pub path: String,
    pub code: Option<Secret>,
    pub state: Option<String>,
    pub iss: Option<String>,
    /// The error the server redirected with, such as `access_denied` or
    /// `invalid_scope`.
    pub error: Option<OAuthError>,
}

/// Parse the request target of the loopback callback, such as
/// `/?code=...&state=...&iss=...`. An absolute target is accepted too.
///
/// When `code`, `state`, `iss` or `error` appears more than once, none of
/// them is taken (RFC 6749 section 3.1), so the request does not match the
/// expected `state` and is ignored.
pub fn parse_callback(target: &str) -> Callback {
    let full = if target.starts_with('/') {
        format!("http://127.0.0.1{target}")
    } else {
        target.to_owned()
    };
    let Ok(url) = reqwest::Url::parse(&full) else {
        return Callback {
            path: target.to_owned(),
            ..Callback::default()
        };
    };
    let mut callback = Callback {
        path: url.path().to_owned(),
        ..Callback::default()
    };
    let mut code = None;
    let mut state = None;
    let mut iss = None;
    let mut error = None;
    let mut description = None;
    let mut repeated = false;
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "code" => &mut code,
            "state" => &mut state,
            "iss" => &mut iss,
            "error" => &mut error,
            "error_description" => &mut description,
            _ => continue,
        };
        if slot.is_some() {
            repeated = true;
        }
        *slot = Some(value.into_owned());
    }
    if repeated {
        return callback;
    }
    callback.code = code.filter(|c| !c.is_empty()).map(Secret::new);
    callback.state = state;
    callback.iss = iss;
    callback.error = error.filter(|e| !e.is_empty()).map(|e| OAuthError {
        error: OAuthErrorKind::from(e.as_str()),
        description,
        temporary: false,
    });
    callback
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::FakeTransport;
    use std::sync::Arc;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        let pkce = Pkce::from_verifier(Secret::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(!format!("{pkce:?}").contains("dBjft"));
    }

    #[test]
    fn generated_values_are_random_and_url_safe() {
        let a = Pkce::generate().unwrap();
        let b = Pkce::generate().unwrap();
        assert_ne!(a.verifier(), b.verifier());
        assert_eq!(a.verifier().expose().len(), 43);
        assert_eq!(a.challenge(), s256_challenge(a.verifier().expose()));
        let state = random_state().unwrap();
        assert_eq!(state.len(), 22);
        assert_ne!(state, random_state().unwrap());
        for s in [a.verifier().expose(), a.challenge(), &state] {
            assert!(
                s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "{s}"
            );
        }
    }

    #[test]
    fn www_authenticate_parameters() {
        let h = r#"Bearer realm="mejl-fake", resource_metadata="https://api.example.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_url(h).as_deref(),
            Some("https://api.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            resource_metadata_url(r#"Bearer resource_metadata="x\"y""#).as_deref(),
            Some("x\"y")
        );
        assert_eq!(
            resource_metadata_url(
                "Bearer error=invalid_token, Resource_Metadata=https://a.example/m"
            )
            .as_deref(),
            Some("https://a.example/m")
        );
        assert_eq!(
            resource_metadata_url(
                r#"Basic realm="x", Bearer resource_metadata="https://a.example/m""#
            )
            .as_deref(),
            Some("https://a.example/m")
        );
        assert_eq!(
            resource_metadata_url(r#"Bearer realm="resource_metadata""#),
            None
        );
        assert_eq!(resource_metadata_url("Bearer"), None);
        assert_eq!(resource_metadata_url(""), None);
        assert_eq!(
            resource_metadata_url(r#"Bearer resource_metadata="open"#).as_deref(),
            Some("open")
        );
    }

    #[test]
    fn endpoint_rules() {
        assert!(check_endpoint("https://api.example.com/oauth/refresh").is_ok());
        assert!(check_endpoint("http://127.0.0.1:8080/oauth/refresh").is_ok());
        assert!(check_endpoint("http://[::1]:8080/x").is_ok());
        assert!(check_endpoint("http://localhost/x").is_ok());
        assert!(check_endpoint("http://api.example.com/oauth/refresh").is_err());
        assert!(check_endpoint("http://mejl-fake.invalid/oauth/refresh").is_err());
        assert!(check_endpoint("ftp://api.example.com/").is_err());
        assert!(check_endpoint("https://user:pw@api.example.com/").is_err());
        assert!(check_endpoint("/relative").is_err());
    }

    #[test]
    fn metadata_url_paths() {
        assert_eq!(
            metadata_urls("https://api.example.com").unwrap(),
            ["https://api.example.com/.well-known/oauth-authorization-server"]
        );
        assert_eq!(
            metadata_urls("https://api.example.com/").unwrap(),
            ["https://api.example.com/.well-known/oauth-authorization-server"]
        );
        assert_eq!(
            metadata_urls("https://example.com/tenant/1").unwrap(),
            [
                "https://example.com/.well-known/oauth-authorization-server/tenant/1",
                "https://example.com/.well-known/oauth-authorization-server"
            ]
        );
    }

    const METADATA: &str = r#"{
        "issuer": "https://api.example.com",
        "registration_endpoint": "https://api.example.com/oauth/register",
        "authorization_endpoint": "https://api.example.com/oauth/authorize",
        "token_endpoint": "https://api.example.com/oauth/refresh",
        "scopes_supported": ["urn:ietf:params:oauth:scope:mail", "offline_access"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "response_types_supported": ["code"],
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "revocation_endpoint": "https://api.example.com/oauth/revoke"
    }"#;

    #[test]
    fn server_metadata_parse() {
        let m = parse_server_metadata(METADATA.as_bytes(), "https://api.example.com/").unwrap();
        assert_eq!(m.issuer, "https://api.example.com");
        assert_eq!(m.token_endpoint, "https://api.example.com/oauth/refresh");
        assert_eq!(
            m.registration_endpoint.as_deref(),
            Some("https://api.example.com/oauth/register")
        );
        assert_eq!(
            m.revocation_endpoint.as_deref(),
            Some("https://api.example.com/oauth/revoke")
        );
        assert_eq!(m.scopes_supported.len(), 2);
        assert!(m.authorization_response_iss_parameter_supported);

        let wrong = parse_server_metadata(METADATA.as_bytes(), "https://evil.example.org");
        assert!(matches!(wrong, Err(Error::InvalidOAuth(_))), "{wrong:?}");
        let plain = METADATA.replace(
            "https://api.example.com/oauth/refresh",
            "http://api.example.com/oauth/refresh",
        );
        assert!(matches!(
            parse_server_metadata(plain.as_bytes(), "https://api.example.com"),
            Err(Error::InvalidUrl { .. })
        ));
        let plain_method = METADATA.replace(r#"["S256"]"#, r#"["plain"]"#);
        assert!(parse_server_metadata(plain_method.as_bytes(), "https://api.example.com").is_err());
        let minimal = r#"{"issuer":"https://a.example","authorization_endpoint":"https://a.example/a","token_endpoint":"https://a.example/t","scopes_supported":null}"#;
        let m = parse_server_metadata(minimal.as_bytes(), "https://a.example").unwrap();
        assert_eq!(m.registration_endpoint, None);
        assert!(m.scopes_supported.is_empty());
        assert!(!m.authorization_response_iss_parameter_supported);
        assert!(
            parse_server_metadata(br#"{"issuer":"https://a.example"}"#, "https://a.example")
                .is_err()
        );
        assert!(parse_server_metadata(b"<html>", "https://a.example").is_err());
    }

    #[test]
    fn resource_metadata_parse() {
        let body = br#"{"resource":"https://api.example.com/jmap/session","authorization_servers":["https://api.example.com"],"scopes_supported":["offline_access"]}"#;
        assert_eq!(
            parse_resource_metadata(body).unwrap(),
            "https://api.example.com"
        );
        assert!(parse_resource_metadata(br#"{"authorization_servers":[]}"#).is_err());
        assert!(parse_resource_metadata(br#"{}"#).is_err());
        assert!(parse_resource_metadata(b"nope").is_err());
    }

    #[test]
    fn discover_follows_the_pointers() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            401,
            &[(
                "www-authenticate",
                r#"Bearer resource_metadata="https://api.example.com/.well-known/oauth-protected-resource/jmap/session""#,
            )],
            "",
        );
        fake.reply(
            200,
            &[],
            r#"{"authorization_servers":["https://api.example.com"]}"#,
        );
        fake.reply(200, &[], METADATA);
        let m = block_on(discover(&*fake, "https://api.example.com/jmap/session")).unwrap();
        assert_eq!(m.issuer, "https://api.example.com");
        let reqs = fake.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].header("authorization"), None);
        assert_eq!(
            reqs[1].url,
            "https://api.example.com/.well-known/oauth-protected-resource/jmap/session"
        );
        assert_eq!(
            reqs[2].url,
            "https://api.example.com/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn discover_without_a_pointer_uses_the_origin() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(401, &[("www-authenticate", "Bearer realm=\"x\"")], "");
        fake.reply(200, &[], METADATA);
        let m = block_on(discover(&*fake, "https://api.example.com/jmap/session")).unwrap();
        assert_eq!(m.token_endpoint, "https://api.example.com/oauth/refresh");

        // A pointer to plain http on a remote host is refused before it is
        // fetched.
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            401,
            &[(
                "www-authenticate",
                r#"Bearer resource_metadata="http://api.example.com/m""#,
            )],
            "",
        );
        assert!(matches!(
            block_on(discover(&*fake, "https://api.example.com/jmap/session")),
            Err(Error::InvalidUrl { .. })
        ));
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn form_encoding_escapes_reserved_characters() {
        assert_eq!(
            form_encode(&[("a", "x+y/z=="), ("b c", "é&")]),
            "a=x%2By%2Fz%3D%3D&b+c=%C3%A9%26"
        );
        assert_eq!(form_encode(&[]), "");
    }

    #[test]
    fn authorize_url_carries_every_parameter() {
        let pkce = Pkce::from_verifier(Secret::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        let url = authorize_url(
            "https://api.example.com/oauth/authorize?x=1",
            "client-1",
            "http://127.0.0.1:4321/",
            &[scope::MAIL, scope::OFFLINE_ACCESS],
            "st+ate",
            &pkce,
        )
        .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("x"), Some("1"));
        assert_eq!(get("response_type"), Some("code"));
        assert_eq!(get("client_id"), Some("client-1"));
        assert_eq!(get("redirect_uri"), Some("http://127.0.0.1:4321/"));
        assert_eq!(
            get("scope"),
            Some("urn:ietf:params:oauth:scope:mail offline_access")
        );
        assert_eq!(get("state"), Some("st+ate"));
        assert_eq!(get("code_challenge"), Some(pkce.challenge()));
        assert_eq!(get("code_challenge_method"), Some("S256"));
        assert!(!url.contains("dBjft"));
        assert!(authorize_url("http://a.example/", "c", "r", &[scope::MAIL], "s", &pkce).is_err());
    }

    fn now() -> Timestamp {
        "2026-10-23T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn token_response_parse() {
        let t = parse_token_response(
            br#"{"access_token":"a1","token_type":"Bearer","expires_in":3600,"refresh_token":"r1","scope":"urn:ietf:params:oauth:scope:mail offline_access"}"#,
            now(),
            None,
        )
        .unwrap();
        assert_eq!(t.access.expose(), "a1");
        assert_eq!(t.refresh.expose(), "r1");
        assert_eq!(t.expires_at, "2026-10-23T13:00:00Z".parse().unwrap());
        assert!(t.has_scope(scope::MAIL));
        assert!(!t.has_scope(scope::MASKED_EMAIL));
        assert!(!format!("{t:?}").contains("a1\""));

        // Without a new refresh token or scope, the old ones stay.
        let t2 = parse_token_response(
            br#"{"access_token":"a2","expires_in":"60"}"#,
            now(),
            Some(&t),
        )
        .unwrap();
        assert_eq!(t2.refresh.expose(), "r1");
        assert_eq!(t2.scope, t.scope);
        assert_eq!(t2.expires_at, "2026-10-23T12:01:00Z".parse().unwrap());

        // A missing lifetime gets the default.
        let t3 = parse_token_response(br#"{"access_token":"a","refresh_token":"r"}"#, now(), None)
            .unwrap();
        assert_eq!(t3.expires_at, "2026-10-23T13:00:00Z".parse().unwrap());
        assert!(t3.scope.is_empty());

        for bad in [
            &br#"{"token_type":"Bearer"}"#[..],
            br#"{"access_token":"a","refresh_token":"r","token_type":"mac"}"#,
            br#"{"access_token":"a"}"#,
            b"not json",
        ] {
            assert!(parse_token_response(bad, now(), None).is_err());
        }
    }

    #[test]
    fn expiry_margin() {
        let t = TokenSet {
            access: Secret::new("a"),
            refresh: Secret::new("r"),
            expires_at: "2026-10-23T12:01:00Z".parse().unwrap(),
            scope: Vec::new(),
        };
        assert!(t.expires_within(SignedDuration::from_secs(60), now()));
        assert!(!t.expires_within(SignedDuration::from_secs(59), now()));
    }

    #[test]
    fn endpoint_errors() {
        let resp = |status: u16, body: &str| HttpResponse {
            status,
            headers: vec![("retry-after".into(), "5".into())],
            body: Bytes::from(body.to_owned()),
        };
        match endpoint_error(&resp(401, r#"{"error":"invalid_client"}"#)) {
            Error::OAuth(e) => assert_eq!(e.error, OAuthErrorKind::InvalidClient),
            other => panic!("{other:?}"),
        }
        match endpoint_error(&resp(400, r#"{"error":"invalid_grant","temporary":true}"#)) {
            Error::OAuth(e) => assert!(e.temporary),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            endpoint_error(&resp(429, "")),
            Error::RateLimited {
                retry_after: Some(_)
            }
        ));
        assert!(endpoint_error(&resp(502, "<html>")).is_transient());
        let e = endpoint_error(&resp(400, "refresh_token=secret-value"));
        assert!(matches!(e, Error::InvalidOAuth(_)));
        assert!(!e.to_string().contains("secret-value"));
    }

    #[test]
    fn callback_parse() {
        let c = parse_callback("/?code=c%2B1&state=s1&iss=https%3A%2F%2Fapi.example.com");
        assert_eq!(c.path, "/");
        assert_eq!(c.code.as_ref().map(Secret::expose), Some("c+1"));
        assert_eq!(c.state.as_deref(), Some("s1"));
        assert_eq!(c.iss.as_deref(), Some("https://api.example.com"));
        assert_eq!(c.error, None);
        assert!(!format!("{c:?}").contains("c+1"));

        let c = parse_callback("/?error=invalid_scope&error_description=no+masks&state=s1");
        let e = c.error.unwrap();
        assert_eq!(e.error, OAuthErrorKind::InvalidScope);
        assert_eq!(e.description.as_deref(), Some("no masks"));
        assert_eq!(c.code, None);

        let c = parse_callback("/favicon.ico");
        assert_eq!(c.path, "/favicon.ico");
        assert_eq!(c.state, None);

        let c = parse_callback("http://127.0.0.1:5000/cb?code=x&state=y");
        assert_eq!(c.path, "/cb");
        assert_eq!(c.state.as_deref(), Some("y"));

        let c = parse_callback("/?code=a&state=s1&state=s2");
        assert_eq!(c.code, None);
        assert_eq!(c.state, None);

        assert_eq!(parse_callback("").path, "");
    }
}
