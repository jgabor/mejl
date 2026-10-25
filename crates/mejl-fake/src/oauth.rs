//! OAuth as Fastmail serves it: discovery documents, open registration,
//! the authorization endpoint, the token endpoint at `/oauth/refresh` and
//! revocation.
//!
//! It is written from RFC 6749, RFC 7591, RFC 7636, RFC 8414, RFC 9728
//! and draft-ietf-mailmaint-oauth-public, plus Fastmail's documented
//! rules: PKCE with S256 only, single-use codes, and refresh tokens that
//! rotate on every use. Sending a rotated refresh token again revokes the
//! whole grant. As the draft asks, the previous refresh token keeps working
//! until the new refresh token or its access token is first used, so a
//! client whose refresh response was lost can retry.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::{SignedDuration, Timestamp};
use mejl_jmap::transport::{HttpMethod, HttpRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::FakeServer;
use crate::api::MASKED;
use crate::http::{Reply, json_reply, response, with_header};
use crate::ids::IdGen;
use crate::text;

/// Scopes the authorization server advertises, as Fastmail's metadata
/// lists them for JMAP.
pub(crate) const ADVERTISED_SCOPES: &[&str] = &[
    "urn:ietf:params:oauth:scope:mail",
    "urn:ietf:params:oauth:scope:contacts",
    "offline_access",
];

/// Every scope the fake grants: the advertised ones, Fastmail's documented
/// JMAP scopes and Masked Email.
const KNOWN_SCOPES: &[&str] = &[
    "urn:ietf:params:oauth:scope:mail",
    "urn:ietf:params:oauth:scope:contacts",
    "offline_access",
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "urn:ietf:params:jmap:vacationresponse",
    "urn:ietf:params:jmap:contacts",
    MASKED,
];

/// How long an authorization code lives.
const CODE_LIFETIME: SignedDuration = SignedDuration::from_secs(600);

/// A live grant, for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthGrant {
    /// The client the grant was made to.
    pub client_id: String,
    /// The granted scope values.
    pub scope: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct OAuthState {
    ids: IdGen,
    clients: BTreeMap<String, Vec<String>>,
    codes: BTreeMap<String, Code>,
    grants: BTreeMap<String, Grant>,
    access: BTreeMap<String, Access>,
    registrations: usize,
    refreshes: usize,
    refresh_requests: usize,
    reuses: usize,
}

#[derive(Debug)]
struct Code {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    scope: Vec<String>,
    issued: Timestamp,
    /// The grant made from this code once it was exchanged.
    grant: Option<String>,
}

#[derive(Debug)]
struct Grant {
    client_id: String,
    scope: Vec<String>,
    revoked: bool,
    /// The refresh token to use next.
    refresh: String,
    /// The refresh token before `refresh`. It still works until `refresh`
    /// or its access token is first used.
    previous: Option<String>,
    /// Rotated refresh tokens. Sending one revokes the grant.
    retired: BTreeSet<String>,
}

#[derive(Debug)]
struct Access {
    grant: String,
    /// The refresh token issued together with this access token.
    refresh: String,
    expires: Timestamp,
}

/// How a JMAP request was authorized.
pub(crate) enum Auth {
    /// The fixed token from the config, with every capability.
    Static,
    /// An OAuth access token with these scopes.
    OAuth { scope: Vec<String> },
}

impl Auth {
    /// Whether this request may use Masked Email.
    pub(crate) fn masked_email(&self) -> bool {
        match self {
            Auth::Static => true,
            Auth::OAuth { scope } => scope.iter().any(|s| s == MASKED),
        }
    }
}

/// The `WWW-Authenticate` value of a 401, pointing at the protected
/// resource metadata.
pub(crate) fn challenge(base: &str, token_given: bool) -> String {
    let error = if token_given {
        ", error=\"invalid_token\""
    } else {
        ""
    };
    format!(
        "Bearer realm=\"mejl-fake\", resource_metadata=\"{base}/.well-known/oauth-protected-resource\"{error}"
    )
}

fn oauth_error(status: u16, error: &str, description: &str) -> Reply {
    json_reply(
        status,
        &json!({"error": error, "error_description": description}),
    )
}

fn plain(status: u16, msg: &str) -> Reply {
    response(status, "text/plain", msg.to_owned())
}

fn form(req: &HttpRequest) -> BTreeMap<String, String> {
    let body = req.body.as_deref().unwrap_or_default();
    let mut out = BTreeMap::new();
    for pair in String::from_utf8_lossy(body).split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.entry(text::percent_decode(k))
            .or_insert_with(|| text::percent_decode(v));
    }
    out
}

fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Host and path of a loopback http redirect URI, with the port dropped.
/// Only `127.0.0.1` and `[::1]` are loopback here, as in the draft.
fn loopback_redirect(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("http://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if path.contains('#') || authority.contains('@') {
        return None;
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, port) = v6.split_once(']')?;
            if !port.is_empty() && !valid_port(port.strip_prefix(':')?) {
                return None;
            }
            format!("[{host}]")
        }
        None => {
            let (host, port) = authority.split_once(':').unwrap_or((authority, ""));
            if authority.contains(':') && !valid_port(port) {
                return None;
            }
            host.to_owned()
        }
    };
    if host != "127.0.0.1" && host != "[::1]" {
        return None;
    }
    let path = if path.is_empty() { "/" } else { path };
    Some((host, path.to_owned()))
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.parse::<u16>().is_ok()
}

fn redirect(redirect_uri: &str, params: &[(&str, &str)]) -> Reply {
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", text::percent_encode(v)))
        .collect();
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    let location = format!("{redirect_uri}{sep}{}", query.join("&"));
    with_header(response(302, "text/plain", ""), "location", &location)
}

impl OAuthState {
    fn new_token(&mut self, kind: &str, prefix: char) -> String {
        format!("mejl-fake-{kind}-{}", self.ids.make(prefix))
    }

    /// Issue a new access and refresh token pair for a grant.
    fn issue(&mut self, grant_id: &str, now: Timestamp, lifetime: Duration) -> (String, String) {
        let access = self.new_token("at", 'a');
        let refresh = self.new_token("rt", 'r');
        let expires = now
            .checked_add(SignedDuration::try_from(lifetime).unwrap_or(SignedDuration::MAX))
            .unwrap_or(Timestamp::MAX);
        self.access.insert(
            access.clone(),
            Access {
                grant: grant_id.to_owned(),
                refresh: refresh.clone(),
                expires,
            },
        );
        (access, refresh)
    }

    fn revoke_grant(&mut self, grant_id: &str) {
        if let Some(grant) = self.grants.get_mut(grant_id) {
            grant.revoked = true;
        }
        self.access.retain(|_, a| a.grant != grant_id);
    }

    /// The scope of a live access token. Its first use ends the grace of
    /// the refresh token before it.
    fn check_access(&mut self, token: &str, now: Timestamp) -> Option<Vec<String>> {
        let access = self.access.get(token)?;
        if access.expires <= now {
            return None;
        }
        let grant = self.grants.get_mut(&access.grant)?;
        if grant.revoked {
            return None;
        }
        if grant.refresh == access.refresh
            && let Some(previous) = grant.previous.take()
        {
            grant.retired.insert(previous);
        }
        Some(grant.scope.clone())
    }
}

fn token_json(access: &str, refresh: &str, lifetime: Duration, scope: &[String]) -> Value {
    json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": lifetime.as_secs(),
        "refresh_token": refresh,
        "scope": scope.join(" "),
    })
}

impl FakeServer {
    fn with_oauth<R>(&self, f: impl FnOnce(&mut OAuthState) -> R) -> R {
        let mut state = self.inner.oauth.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut state)
    }

    fn oauth_now(&self) -> Timestamp {
        self.with_store(|s| s.clock.peek())
    }

    /// Who a bearer token belongs to: the fixed token, a live OAuth access
    /// token, or nobody.
    pub(crate) fn authenticate(&self, token: &str) -> Option<Auth> {
        if token.is_empty() {
            return None;
        }
        if token == self.inner.token {
            return Some(Auth::Static);
        }
        let now = self.oauth_now();
        self.with_oauth(|s| s.check_access(token, now))
            .map(|scope| Auth::OAuth { scope })
    }

    /// Makes every OAuth access token expire now. Refresh tokens keep
    /// working, so a client has to refresh.
    pub fn expire_access_tokens(&self) {
        let now = self.oauth_now();
        self.with_oauth(|s| {
            for access in s.access.values_mut() {
                access.expires = access.expires.min(now);
            }
        });
    }

    /// How many clients registered through `/oauth/register`.
    pub fn oauth_registrations(&self) -> usize {
        self.with_oauth(|s| s.registrations)
    }

    /// How many refreshes succeeded.
    pub fn oauth_refreshes(&self) -> usize {
        self.with_oauth(|s| s.refreshes)
    }

    /// How many refresh requests reached the token endpoint, refused or
    /// not.
    pub fn oauth_refresh_requests(&self) -> usize {
        self.with_oauth(|s| s.refresh_requests)
    }

    /// Whether a rotated refresh token was ever sent again, which revoked
    /// its grant. A retry with the previous token before the new one was
    /// used does not count.
    pub fn oauth_refresh_token_reused(&self) -> bool {
        self.with_oauth(|s| s.reuses > 0)
    }

    /// The grants that are not revoked.
    pub fn oauth_grants(&self) -> Vec<OAuthGrant> {
        self.with_oauth(|s| {
            s.grants
                .values()
                .filter(|g| !g.revoked)
                .map(|g| OAuthGrant {
                    client_id: g.client_id.clone(),
                    scope: g.scope.clone(),
                })
                .collect()
        })
    }
}

/// Handles the OAuth paths, or returns `None` for any other path.
pub(crate) fn route(
    server: &FakeServer,
    req: &HttpRequest,
    segs: &[&str],
    query: &[(String, String)],
    base: &str,
) -> Option<Reply> {
    let method = req.method;
    let wrong_method = || plain(405, "wrong method for this path");
    Some(match segs {
        [".well-known", "oauth-protected-resource", rest @ ..] => match method {
            HttpMethod::Get => protected_resource(base, rest),
            HttpMethod::Post => wrong_method(),
        },
        [".well-known", "oauth-authorization-server"] => match method {
            HttpMethod::Get => json_reply(200, &server_metadata(base)),
            HttpMethod::Post => wrong_method(),
        },
        ["oauth", "register"] => match method {
            HttpMethod::Post => register(server, req),
            HttpMethod::Get => wrong_method(),
        },
        ["oauth", "authorize"] => match method {
            HttpMethod::Get => authorize(server, query, base),
            HttpMethod::Post => wrong_method(),
        },
        ["oauth", "refresh"] => match method {
            HttpMethod::Post => token(server, req),
            HttpMethod::Get => wrong_method(),
        },
        ["oauth", "revoke"] => match method {
            HttpMethod::Post => revoke(server, req),
            HttpMethod::Get => wrong_method(),
        },
        _ => return None,
    })
}

fn protected_resource(base: &str, rest: &[&str]) -> Reply {
    let resource = if rest.is_empty() {
        base.to_owned()
    } else {
        format!("{base}/{}", rest.join("/"))
    };
    json_reply(
        200,
        &json!({
            "resource": resource,
            "authorization_servers": [base],
            "scopes_supported": ADVERTISED_SCOPES,
            "bearer_methods_supported": ["header"],
        }),
    )
}

fn server_metadata(base: &str) -> Value {
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/refresh"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "scopes_supported": ADVERTISED_SCOPES,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
    })
}

fn register(server: &FakeServer, req: &HttpRequest) -> Reply {
    if server.quirks_mut().refuse_registration {
        return oauth_error(403, "access_denied", "open registration is closed");
    }
    let body = req.body.as_deref().unwrap_or_default();
    let Ok(Value::Object(meta)) = serde_json::from_slice::<Value>(body) else {
        return oauth_error(
            400,
            "invalid_client_metadata",
            "the body is not a JSON object",
        );
    };
    let strings = |key: &str| -> Option<Vec<String>> {
        match meta.get(key) {
            None => Some(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Some(_) => None,
        }
    };
    let Some(redirect_uris) = strings("redirect_uris").filter(|u| !u.is_empty()) else {
        return oauth_error(400, "invalid_redirect_uri", "redirect_uris is required");
    };
    if redirect_uris.iter().any(|u| loopback_redirect(u).is_none()) {
        return oauth_error(
            400,
            "invalid_redirect_uri",
            "only loopback http redirect URIs are allowed",
        );
    }
    match meta.get("token_endpoint_auth_method") {
        None => {}
        Some(v) if v.as_str() == Some("none") => {}
        Some(_) => {
            return oauth_error(
                400,
                "invalid_client_metadata",
                "token_endpoint_auth_method must be none",
            );
        }
    }
    let grant_types = strings("grant_types");
    let response_types = strings("response_types");
    let grants_ok = grant_types.as_ref().is_some_and(|g| {
        g.iter()
            .all(|t| t == "authorization_code" || t == "refresh_token")
    });
    let responses_ok = response_types
        .as_ref()
        .is_some_and(|r| r.iter().all(|t| t == "code"));
    if !grants_ok || !responses_ok {
        return oauth_error(
            400,
            "invalid_client_metadata",
            "only the code flow with refresh tokens is supported",
        );
    }
    let client_name = meta.get("client_name").cloned().unwrap_or(Value::Null);
    let client_id = server.with_oauth(|s| {
        let id = format!("mejl-fake-client-{}", s.ids.make('c'));
        s.clients.insert(id.clone(), redirect_uris.clone());
        s.registrations += 1;
        id
    });
    json_reply(
        201,
        &json!({
            "client_id": client_id,
            "client_name": client_name,
            "redirect_uris": redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        }),
    )
}

fn authorize(server: &FakeServer, query: &[(String, String)], base: &str) -> Reply {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    // An unknown client or a bad redirect URI gets an error page, never a
    // redirect, as RFC 6749 section 4.1.2.1 requires.
    let client_id = param("client_id").unwrap_or("");
    let Some(registered) = server.with_oauth(|s| s.clients.get(client_id).cloned()) else {
        return plain(400, "unknown client");
    };
    let Some(redirect_uri) = param("redirect_uri") else {
        return plain(400, "redirect_uri is required");
    };
    let Some(wanted) = loopback_redirect(redirect_uri) else {
        return plain(400, "redirect_uri is not a loopback URI");
    };
    if !registered
        .iter()
        .any(|r| loopback_redirect(r).as_ref() == Some(&wanted))
    {
        return plain(400, "redirect_uri is not registered");
    }
    let state = param("state");
    let fail = |error: &str, description: &str| {
        let mut params = vec![("error", error), ("error_description", description)];
        if let Some(state) = state {
            params.push(("state", state));
        }
        params.push(("iss", base));
        redirect(redirect_uri, &params)
    };
    if param("response_type") != Some("code") {
        return fail("unsupported_response_type", "only code is supported");
    }
    let challenge = param("code_challenge").unwrap_or("");
    if challenge.is_empty() || param("code_challenge_method") != Some("S256") {
        return fail("invalid_request", "PKCE with S256 is required");
    }
    let scope: Vec<String> = param("scope")
        .unwrap_or("")
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if scope.is_empty() {
        return fail("invalid_scope", "no scope was asked for");
    }
    if let Some(unknown) = scope.iter().find(|s| !KNOWN_SCOPES.contains(&s.as_str())) {
        return fail("invalid_scope", &format!("unknown scope {unknown}"));
    }
    if scope.iter().any(|s| s == MASKED) && server.quirks_mut().refuse_masked_scope {
        return fail("invalid_scope", "Masked Email is not available");
    }
    let now = server.oauth_now();
    let code = server.with_oauth(|s| {
        let code = s.new_token("code", 'k');
        s.codes.insert(
            code.clone(),
            Code {
                client_id: client_id.to_owned(),
                redirect_uri: redirect_uri.to_owned(),
                challenge: challenge.to_owned(),
                scope,
                issued: now,
                grant: None,
            },
        );
        code
    });
    let mut params = vec![("code", code.as_str())];
    if let Some(state) = state {
        params.push(("state", state));
    }
    params.push(("iss", base));
    redirect(redirect_uri, &params)
}

fn token(server: &FakeServer, req: &HttpRequest) -> Reply {
    let form = form(req);
    let get = |key: &str| form.get(key).map(String::as_str).unwrap_or("");
    let client_id = get("client_id");
    if !server.with_oauth(|s| s.clients.contains_key(client_id)) {
        return oauth_error(401, "invalid_client", "unknown client");
    }
    let lifetime = server.quirks_mut().access_token_lifetime;
    let now = server.oauth_now();
    match get("grant_type") {
        "authorization_code" => exchange(server, &form, client_id, now, lifetime),
        "refresh_token" => refresh(server, get("refresh_token"), client_id, now, lifetime),
        "" => oauth_error(400, "invalid_request", "grant_type is required"),
        _ => oauth_error(400, "unsupported_grant_type", "unsupported grant type"),
    }
}

fn exchange(
    server: &FakeServer,
    form: &BTreeMap<String, String>,
    client_id: &str,
    now: Timestamp,
    lifetime: Duration,
) -> Reply {
    let get = |key: &str| form.get(key).map(String::as_str).unwrap_or("");
    let verifier = get("code_verifier");
    let verifier_ok = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    if !verifier_ok {
        return oauth_error(400, "invalid_request", "a valid code_verifier is required");
    }
    server.with_oauth(|s| {
        let Some(code) = s.codes.get_mut(get("code")) else {
            return oauth_error(400, "invalid_grant", "unknown code");
        };
        // A second use revokes everything made from the code.
        if let Some(grant) = code.grant.clone() {
            s.revoke_grant(&grant);
            return oauth_error(400, "invalid_grant", "the code was already used");
        }
        let expired = code
            .issued
            .checked_add(CODE_LIFETIME)
            .is_ok_and(|end| end <= now);
        if code.client_id != client_id
            || code.redirect_uri != get("redirect_uri")
            || code.challenge != s256(verifier)
            || expired
        {
            return oauth_error(400, "invalid_grant", "the code does not match");
        }
        let scope = code.scope.clone();
        let grant_id = s.ids.make('g');
        code.grant = Some(grant_id.clone());
        let (access, refresh) = s.issue(&grant_id, now, lifetime);
        s.grants.insert(
            grant_id,
            Grant {
                client_id: client_id.to_owned(),
                scope: scope.clone(),
                revoked: false,
                refresh: refresh.clone(),
                previous: None,
                retired: BTreeSet::new(),
            },
        );
        json_reply(200, &token_json(&access, &refresh, lifetime, &scope))
    })
}

fn refresh(
    server: &FakeServer,
    token: &str,
    client_id: &str,
    now: Timestamp,
    lifetime: Duration,
) -> Reply {
    let (temporary, lose) = {
        let mut q = server.quirks_mut();
        let temporary = q.refresh_temporary_invalid_grant_next > 0;
        q.refresh_temporary_invalid_grant_next =
            q.refresh_temporary_invalid_grant_next.saturating_sub(1);
        let lose = !temporary && q.lose_refresh_response_next > 0;
        if lose {
            q.lose_refresh_response_next -= 1;
        }
        (temporary, lose)
    };
    let reply = server.with_oauth(|s| {
        s.refresh_requests += 1;
        if temporary {
            return json_reply(
                400,
                &json!({
                    "error": "invalid_grant",
                    "error_description": "the account is locked for now",
                    "temporary": true,
                }),
            );
        }
        enum Found {
            Current(String),
            Previous(String),
            Retired(String),
        }
        let found = s.grants.iter().find_map(|(id, g)| {
            if g.refresh == token {
                Some(Found::Current(id.clone()))
            } else if g.previous.as_deref() == Some(token) {
                Some(Found::Previous(id.clone()))
            } else if g.retired.contains(token) {
                Some(Found::Retired(id.clone()))
            } else {
                None
            }
        });
        let (grant_id, from_previous) = match found {
            None => return oauth_error(400, "invalid_grant", "unknown refresh token"),
            Some(Found::Retired(id)) => {
                s.reuses += 1;
                s.revoke_grant(&id);
                return oauth_error(
                    400,
                    "invalid_grant",
                    "a rotated refresh token was used again; the grant is revoked",
                );
            }
            Some(Found::Current(id)) => (id, false),
            Some(Found::Previous(id)) => (id, true),
        };
        let Some(grant) = s.grants.get(&grant_id) else {
            return oauth_error(400, "invalid_grant", "unknown refresh token");
        };
        if grant.revoked || grant.client_id != client_id {
            return oauth_error(400, "invalid_grant", "the refresh token is not valid");
        }
        let old_current = grant.refresh.clone();
        let scope = grant.scope.clone();
        let (access, refresh) = s.issue(&grant_id, now, lifetime);
        if from_previous {
            // The client never saw the pair issued last time, so it is
            // dropped and the previous refresh token stays in grace.
            s.access.retain(|_, a| a.refresh != old_current);
        }
        let Some(grant) = s.grants.get_mut(&grant_id) else {
            return oauth_error(400, "invalid_grant", "unknown refresh token");
        };
        if from_previous {
            grant.retired.insert(old_current);
        } else {
            if let Some(previous) = grant.previous.take() {
                grant.retired.insert(previous);
            }
            grant.previous = Some(old_current);
        }
        grant.refresh = refresh.clone();
        s.refreshes += 1;
        json_reply(200, &token_json(&access, &refresh, lifetime, &scope))
    });
    if lose { Reply::Lost } else { reply }
}

fn revoke(server: &FakeServer, req: &HttpRequest) -> Reply {
    let form = form(req);
    let client_id = form.get("client_id").map(String::as_str).unwrap_or("");
    let token = form.get("token").map(String::as_str).unwrap_or("");
    server.with_oauth(|s| {
        if !s.clients.contains_key(client_id) {
            return oauth_error(401, "invalid_client", "unknown client");
        }
        let grant = s
            .grants
            .iter()
            .find(|(_, g)| {
                g.client_id == client_id
                    && (g.refresh == token
                        || g.previous.as_deref() == Some(token)
                        || g.retired.contains(token))
            })
            .map(|(id, _)| id.clone());
        if let Some(grant) = grant {
            s.revoke_grant(&grant);
        } else if s
            .access
            .get(token)
            .and_then(|a| s.grants.get(&a.grant))
            .is_some_and(|g| g.client_id == client_id)
        {
            s.access.remove(token);
        }
        // RFC 7009: an unknown token is not an error.
        response(200, "text/plain", "")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_redirects() {
        assert_eq!(
            loopback_redirect("http://127.0.0.1/"),
            Some(("127.0.0.1".into(), "/".into()))
        );
        assert_eq!(
            loopback_redirect("http://127.0.0.1:4321/cb?x=1"),
            Some(("127.0.0.1".into(), "/cb?x=1".into()))
        );
        assert_eq!(
            loopback_redirect("http://[::1]:80"),
            Some(("[::1]".into(), "/".into()))
        );
        for bad in [
            "https://127.0.0.1/",
            "http://localhost/",
            "http://example.com/",
            "http://127.0.0.1:x/",
            "http://127.0.0.1:/",
            "http://user@127.0.0.1/",
            "http://127.0.0.1/#frag",
            "com.example:/cb",
        ] {
            assert_eq!(loopback_redirect(bad), None, "{bad}");
        }
    }

    #[test]
    fn s256_matches_rfc_7636() {
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
