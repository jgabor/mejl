//! OAuth end to end over loopback HTTP: the fake's endpoints driven by
//! mejl-jmap's protocol functions and its self-refreshing client. Nothing
//! here leaves 127.0.0.1.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt;
use futures::future::join_all;
use mejl_fake::{FakeConfig, FakeServer, HttpHandle};
use mejl_jmap::method::GetArgs;
use mejl_jmap::methods::MailboxGet;
use mejl_jmap::oauth::{
    self, Callback, Pkce, ServerMetadata, TokenSet, authorize_url, discover, exchange_code,
    parse_callback, random_state, register, scope,
};
use mejl_jmap::transport::{
    BoxFuture, HttpMethod, HttpRequest, HttpResponse, StreamingResponse, Transport, TransportError,
};
use mejl_jmap::{
    Client, Error, HttpTransport, OAuthCredentials, OAuthErrorKind, PushOptions, RequestBuilder,
    Secret, TokenStore, capability,
};

/// A redirect URI on a port nothing listens on. The tests read the
/// redirect from the 302 instead of following it.
const REDIRECT: &str = "http://127.0.0.1:9/";

const SCOPES: &[&str] = &[scope::MAIL, scope::CONTACTS, scope::OFFLINE_ACCESS];

struct Env {
    server: FakeServer,
    handle: HttpHandle,
    transport: Arc<dyn Transport>,
}

async fn start() -> Env {
    let server = FakeServer::new(FakeConfig {
        now: Some("2026-10-23T14:27:00Z".parse().unwrap()),
        ..FakeConfig::default()
    });
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    Env {
        server,
        handle,
        transport: Arc::new(HttpTransport::new().unwrap()),
    }
}

async fn send(
    t: &dyn Transport,
    method: HttpMethod,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpResponse {
    t.send(HttpRequest {
        method,
        url: url.to_owned(),
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body: body.map(|b| Bytes::from(b.to_owned())),
    })
    .await
    .unwrap()
}

async fn session_status(env: &Env, token: &Secret) -> u16 {
    let auth = format!("Bearer {}", token.expose());
    send(
        &*env.transport,
        HttpMethod::Get,
        &env.handle.session_url(),
        &[("authorization", &auth)],
        None,
    )
    .await
    .status
}

async fn session_json(env: &Env, token: &str) -> serde_json::Value {
    let auth = format!("Bearer {token}");
    let resp = send(
        &*env.transport,
        HttpMethod::Get,
        &env.handle.session_url(),
        &[("authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(resp.status, 200);
    serde_json::from_slice(&resp.body).unwrap()
}

struct Approval {
    callback: Callback,
    pkce: Pkce,
    state: String,
}

async fn approve(env: &Env, meta: &ServerMetadata, client_id: &str, scopes: &[&str]) -> Approval {
    let pkce = Pkce::generate().unwrap();
    let state = random_state().unwrap();
    let url = authorize_url(
        &meta.authorization_endpoint,
        client_id,
        REDIRECT,
        scopes,
        &state,
        &pkce,
    )
    .unwrap();
    let resp = send(&*env.transport, HttpMethod::Get, &url, &[], None).await;
    assert_eq!(resp.status, 302, "{}", String::from_utf8_lossy(&resp.body));
    let location = resp.header("location").unwrap();
    assert!(location.starts_with(REDIRECT), "{location}");
    Approval {
        callback: parse_callback(location),
        pkce,
        state,
    }
}

struct Login {
    meta: ServerMetadata,
    client_id: String,
    tokens: TokenSet,
}

async fn login(env: &Env, scopes: &[&str]) -> Login {
    let meta = discover(&*env.transport, &env.handle.session_url())
        .await
        .unwrap();
    let client_id = register(
        &*env.transport,
        meta.registration_endpoint.as_deref().unwrap(),
    )
    .await
    .unwrap();
    let approval = approve(env, &meta, &client_id, scopes).await;
    let callback = &approval.callback;
    assert_eq!(callback.state.as_deref(), Some(approval.state.as_str()));
    assert_eq!(callback.iss.as_deref(), Some(meta.issuer.as_str()));
    assert_eq!(callback.error, None);
    let tokens = exchange_code(
        &*env.transport,
        &meta.token_endpoint,
        &client_id,
        callback.code.as_ref().unwrap(),
        REDIRECT,
        &approval.pkce,
    )
    .await
    .unwrap();
    Login {
        meta,
        client_id,
        tokens,
    }
}

// ----- the fake's endpoints

#[tokio::test]
async fn discovery_finds_the_fake_endpoints() {
    let env = start().await;
    let resp = send(
        &*env.transport,
        HttpMethod::Get,
        &env.handle.session_url(),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status, 401);
    let challenge = resp.header("www-authenticate").unwrap();
    assert_eq!(
        oauth::resource_metadata_url(challenge),
        Some(format!(
            "{}/.well-known/oauth-protected-resource",
            env.handle.base_url()
        ))
    );

    let meta = discover(&*env.transport, &env.handle.session_url())
        .await
        .unwrap();
    let base = env.handle.base_url();
    assert_eq!(meta.issuer, base);
    assert_eq!(meta.token_endpoint, format!("{base}/oauth/refresh"));
    assert_eq!(
        meta.authorization_endpoint,
        format!("{base}/oauth/authorize")
    );
    assert_eq!(
        meta.registration_endpoint,
        Some(format!("{base}/oauth/register"))
    );
    assert_eq!(
        meta.revocation_endpoint,
        Some(format!("{base}/oauth/revoke"))
    );
    assert!(meta.authorization_response_iss_parameter_supported);
    assert_eq!(
        meta.scopes_supported,
        [scope::MAIL, scope::CONTACTS, scope::OFFLINE_ACCESS]
    );

    // There is no /oauth/token, as at Fastmail.
    let resp = send(
        &*env.transport,
        HttpMethod::Post,
        &format!("{base}/oauth/token"),
        &[("content-type", "application/x-www-form-urlencoded")],
        Some("grant_type=refresh_token"),
    )
    .await;
    assert_eq!(resp.status, 404);
}

#[tokio::test]
async fn registration_rules() {
    let env = start().await;
    let endpoint = format!("{}/oauth/register", env.handle.base_url());
    let id = register(&*env.transport, &endpoint).await.unwrap();
    assert!(!id.is_empty());
    assert_eq!(env.server.oauth_registrations(), 1);

    let raw = |body: &'static str| {
        let t = env.transport.clone();
        let url = endpoint.clone();
        async move {
            let resp = send(
                &*t,
                HttpMethod::Post,
                &url,
                &[("content-type", "application/json")],
                Some(body),
            )
            .await;
            let v: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
            (resp.status, v["error"].as_str().unwrap_or("").to_owned())
        }
    };
    assert_eq!(
        raw(r#"{"redirect_uris":["https://app.example.com/cb"]}"#).await,
        (400, "invalid_redirect_uri".to_owned())
    );
    assert_eq!(
        raw(r#"{"redirect_uris":["http://127.0.0.1/"],"token_endpoint_auth_method":"client_secret_basic"}"#).await,
        (400, "invalid_client_metadata".to_owned())
    );
    assert_eq!(
        raw(r#"{"redirect_uris":["http://127.0.0.1/"],"grant_types":["implicit"]}"#).await,
        (400, "invalid_client_metadata".to_owned())
    );
    assert_eq!(env.server.oauth_registrations(), 1);

    env.server.quirks_mut().refuse_registration = true;
    match register(&*env.transport, &endpoint).await {
        Err(Error::OAuth(e)) => assert_eq!(e.error, OAuthErrorKind::AccessDenied),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn authorize_rules() {
    let env = start().await;
    let meta = discover(&*env.transport, &env.handle.session_url())
        .await
        .unwrap();
    let client_id = register(
        &*env.transport,
        meta.registration_endpoint.as_deref().unwrap(),
    )
    .await
    .unwrap();
    let pkce = Pkce::generate().unwrap();
    let get = |url: String| {
        let t = env.transport.clone();
        async move { send(&*t, HttpMethod::Get, &url, &[], None).await }
    };

    // Unknown clients and bad redirect URIs get an error page, no redirect.
    let url = authorize_url(
        &meta.authorization_endpoint,
        "nobody",
        REDIRECT,
        SCOPES,
        "s",
        &pkce,
    )
    .unwrap();
    let resp = get(url).await;
    assert_eq!(resp.status, 400);
    assert_eq!(resp.header("location"), None);
    for redirect in [
        "http://localhost:9/",
        "http://127.0.0.1:9/other",
        "https://127.0.0.1/",
    ] {
        let url = authorize_url(
            &meta.authorization_endpoint,
            &client_id,
            redirect,
            SCOPES,
            "s",
            &pkce,
        )
        .unwrap();
        let resp = get(url).await;
        assert_eq!(resp.status, 400, "{redirect}");
        assert_eq!(resp.header("location"), None);
    }
    // Any port is fine, and IPv6 loopback is not the registered host.
    let url = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        "http://127.0.0.1:65000/",
        SCOPES,
        "s",
        &pkce,
    )
    .unwrap();
    assert_eq!(get(url).await.status, 302);

    // Other problems redirect with an error, the state and the issuer.
    let redirected_error = |url: String| async move {
        let resp = get(url).await;
        assert_eq!(resp.status, 302);
        parse_callback(resp.header("location").unwrap())
    };
    let plain = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        REDIRECT,
        SCOPES,
        "s1",
        &pkce,
    )
    .unwrap()
    .replace("code_challenge_method=S256", "code_challenge_method=plain");
    let cb = redirected_error(plain).await;
    assert_eq!(cb.error.unwrap().error, OAuthErrorKind::InvalidRequest);
    assert_eq!(cb.state.as_deref(), Some("s1"));
    assert_eq!(cb.iss.as_deref(), Some(meta.issuer.as_str()));
    assert_eq!(cb.code, None);

    let token_type = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        REDIRECT,
        SCOPES,
        "s2",
        &pkce,
    )
    .unwrap()
    .replace("response_type=code", "response_type=token");
    let cb = redirected_error(token_type).await;
    assert_eq!(
        cb.error.unwrap().error,
        OAuthErrorKind::UnsupportedResponseType
    );

    let unknown = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        REDIRECT,
        &["urn:example:all"],
        "s3",
        &pkce,
    )
    .unwrap();
    let cb = redirected_error(unknown).await;
    assert_eq!(cb.error.unwrap().error, OAuthErrorKind::InvalidScope);
}

#[tokio::test]
async fn masked_email_scope_can_be_refused() {
    let env = start().await;
    env.server.quirks_mut().refuse_masked_scope = true;
    let meta = discover(&*env.transport, &env.handle.session_url())
        .await
        .unwrap();
    let client_id = register(
        &*env.transport,
        meta.registration_endpoint.as_deref().unwrap(),
    )
    .await
    .unwrap();
    let mut scopes = SCOPES.to_vec();
    scopes.extend([scope::JMAP_CORE, scope::MASKED_EMAIL]);
    let approval = approve(&env, &meta, &client_id, &scopes).await;
    let cb = approval.callback;
    assert_eq!(cb.error.unwrap().error, OAuthErrorKind::InvalidScope);
    assert_eq!(cb.state, Some(approval.state));
    assert_eq!(cb.code, None);
    // Without the Masked Email scope the same client gets a code.
    let approval = approve(&env, &meta, &client_id, SCOPES).await;
    assert!(approval.callback.code.is_some());
}

#[tokio::test]
async fn code_exchange_checks_pkce_redirect_and_single_use() {
    let env = start().await;
    let meta = discover(&*env.transport, &env.handle.session_url())
        .await
        .unwrap();
    let client_id = register(
        &*env.transport,
        meta.registration_endpoint.as_deref().unwrap(),
    )
    .await
    .unwrap();
    let approval = approve(&env, &meta, &client_id, SCOPES).await;
    let code = approval.callback.code.clone().unwrap();
    let exchange = |redirect: &'static str, pkce: Pkce, client: String| {
        let t = env.transport.clone();
        let endpoint = meta.token_endpoint.clone();
        let code = code.clone();
        async move { exchange_code(&*t, &endpoint, &client, &code, redirect, &pkce).await }
    };
    let grant_error = |r: Result<TokenSet, Error>| match r {
        Err(Error::OAuth(e)) => e.error,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        grant_error(exchange(REDIRECT, Pkce::generate().unwrap(), client_id.clone()).await),
        OAuthErrorKind::InvalidGrant
    );
    assert_eq!(
        grant_error(
            exchange(
                "http://127.0.0.1:10/",
                approval.pkce.clone(),
                client_id.clone()
            )
            .await
        ),
        OAuthErrorKind::InvalidGrant
    );
    assert_eq!(
        grant_error(exchange(REDIRECT, approval.pkce.clone(), "nobody".into()).await),
        OAuthErrorKind::InvalidClient
    );

    let tokens = exchange(REDIRECT, approval.pkce.clone(), client_id.clone())
        .await
        .unwrap();
    assert!(tokens.has_scope(scope::MAIL));
    assert_eq!(session_status(&env, &tokens.access).await, 200);
    assert_eq!(env.server.oauth_grants().len(), 1);
    assert_eq!(env.server.oauth_grants()[0].client_id, client_id);

    // A second use of the code fails and revokes what the first one made.
    assert_eq!(
        grant_error(exchange(REDIRECT, approval.pkce.clone(), client_id.clone()).await),
        OAuthErrorKind::InvalidGrant
    );
    assert_eq!(session_status(&env, &tokens.access).await, 401);
    assert!(env.server.oauth_grants().is_empty());
}

#[tokio::test]
async fn refresh_rotates_and_reuse_revokes_the_grant() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let t = &*env.transport;
    let r1 = l.tokens.clone();
    let r2 = oauth::refresh(t, &l.meta.token_endpoint, &l.client_id, &r1)
        .await
        .unwrap();
    assert_ne!(r2.refresh, r1.refresh);
    assert_ne!(r2.access, r1.access);
    assert_eq!(r2.scope, r1.scope);
    assert_eq!(env.server.oauth_refreshes(), 1);

    // Using the new access token ends the old refresh token's grace.
    assert_eq!(session_status(&env, &r2.access).await, 200);
    assert!(!env.server.oauth_refresh_token_reused());
    match oauth::refresh(t, &l.meta.token_endpoint, &l.client_id, &r1).await {
        Err(Error::OAuth(e)) => {
            assert_eq!(e.error, OAuthErrorKind::InvalidGrant);
            assert!(!e.temporary);
        }
        other => panic!("{other:?}"),
    }
    assert!(env.server.oauth_refresh_token_reused());
    assert!(env.server.oauth_grants().is_empty());
    assert_eq!(session_status(&env, &r2.access).await, 401);
    assert!(
        oauth::refresh(t, &l.meta.token_endpoint, &l.client_id, &r2)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn previous_refresh_token_works_until_the_new_pair_is_used() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let t = &*env.transport;
    let endpoint = &l.meta.token_endpoint;
    // A response the client never saw.
    let unseen = oauth::refresh(t, endpoint, &l.client_id, &l.tokens)
        .await
        .unwrap();
    // The retry with the same refresh token works and is not a reuse.
    let retried = oauth::refresh(t, endpoint, &l.client_id, &l.tokens)
        .await
        .unwrap();
    assert!(!env.server.oauth_refresh_token_reused());
    assert_eq!(session_status(&env, &retried.access).await, 200);
    // The unseen pair is gone.
    assert_eq!(session_status(&env, &unseen.access).await, 401);
    assert!(
        oauth::refresh(t, endpoint, &l.client_id, &unseen)
            .await
            .is_err()
    );
    assert!(env.server.oauth_refresh_token_reused());
}

#[tokio::test]
async fn revoke_ends_the_grant() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let t = &*env.transport;
    let endpoint = l.meta.revocation_endpoint.as_deref().unwrap();
    assert_eq!(session_status(&env, &l.tokens.access).await, 200);
    oauth::revoke(t, endpoint, &l.client_id, &l.tokens.refresh)
        .await
        .unwrap();
    assert!(env.server.oauth_grants().is_empty());
    assert_eq!(session_status(&env, &l.tokens.access).await, 401);
    // Revoking again, or a token the server does not know, is fine.
    oauth::revoke(t, endpoint, &l.client_id, &l.tokens.refresh)
        .await
        .unwrap();
    match oauth::revoke(t, endpoint, "nobody", &l.tokens.refresh).await {
        Err(Error::OAuth(e)) => assert_eq!(e.error, OAuthErrorKind::InvalidClient),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn session_follows_the_granted_scope() {
    let env = start().await;
    let plain = login(&env, SCOPES).await;
    let session = session_json(&env, plain.tokens.access.expose()).await;
    assert!(session["capabilities"].get(capability::MAIL).is_some());
    assert!(
        session["capabilities"]
            .get(capability::MASKED_EMAIL)
            .is_none()
    );
    let account = env.server.account_id();
    assert!(
        session["accounts"][account]["accountCapabilities"]
            .get(capability::MASKED_EMAIL)
            .is_none()
    );
    assert!(
        session["primaryAccounts"]
            .get(capability::MASKED_EMAIL)
            .is_none()
    );

    // Masked Email methods are refused without the scope.
    let auth = format!("Bearer {}", plain.tokens.access.expose());
    let body = format!(
        r#"{{"using":["{}","{}"],"methodCalls":[["MaskedEmail/get",{{"accountId":"{account}"}},"0"]]}}"#,
        capability::CORE,
        capability::MASKED_EMAIL
    );
    let resp = send(
        &*env.transport,
        HttpMethod::Post,
        &format!("{}/jmap/api/", env.handle.base_url()),
        &[
            ("authorization", &auth),
            ("content-type", "application/json"),
        ],
        Some(&body),
    )
    .await;
    assert_eq!(resp.status, 400);
    assert!(String::from_utf8_lossy(&resp.body).contains("unknownCapability"));

    let mut scopes = SCOPES.to_vec();
    scopes.extend([scope::JMAP_CORE, scope::MASKED_EMAIL]);
    let masked = login(&env, &scopes).await;
    assert!(masked.tokens.has_scope(scope::MASKED_EMAIL));
    let session = session_json(&env, masked.tokens.access.expose()).await;
    assert!(
        session["capabilities"]
            .get(capability::MASKED_EMAIL)
            .is_some()
    );

    // The fixed token keeps every capability.
    let session = session_json(&env, env.server.token()).await;
    assert!(
        session["capabilities"]
            .get(capability::MASKED_EMAIL)
            .is_some()
    );
}

#[tokio::test]
async fn lifetime_expiry_and_temporary_refusal() {
    let env = start().await;
    env.server.quirks_mut().access_token_lifetime = std::time::Duration::from_secs(120);
    let before = jiff::Timestamp::now();
    let l = login(&env, SCOPES).await;
    let left = l.tokens.expires_at.duration_since(before).as_secs();
    assert!((119..=121).contains(&left), "{left}");

    env.server.expire_access_tokens();
    assert_eq!(session_status(&env, &l.tokens.access).await, 401);

    env.server.quirks_mut().refresh_temporary_invalid_grant_next = 1;
    match oauth::refresh(
        &*env.transport,
        &l.meta.token_endpoint,
        &l.client_id,
        &l.tokens,
    )
    .await
    {
        Err(e) => {
            let oauth = e.oauth_error().unwrap();
            assert_eq!(oauth.error, OAuthErrorKind::InvalidGrant);
            assert!(oauth.temporary);
            assert!(e.is_unauthorized());
        }
        Ok(_) => panic!("expected a refusal"),
    }
    // Nothing changed on the server: the same refresh token works now.
    let fresh = oauth::refresh(
        &*env.transport,
        &l.meta.token_endpoint,
        &l.client_id,
        &l.tokens,
    )
    .await
    .unwrap();
    assert_eq!(session_status(&env, &fresh.access).await, 200);
    assert_eq!(env.server.oauth_refresh_requests(), 2);
    assert_eq!(env.server.oauth_refreshes(), 1);
}

// ----- the self-refreshing client

/// An in-memory store that records every save once it returns, failed or
/// not.
#[derive(Default)]
struct MemStore {
    saved: Mutex<Option<TokenSet>>,
    attempts: Mutex<Vec<TokenSet>>,
    fail_next: AtomicUsize,
    delay: Mutex<std::time::Duration>,
}

impl MemStore {
    fn with(tokens: &TokenSet) -> Arc<Self> {
        let store = Self::default();
        *store.saved.lock().unwrap() = Some(tokens.clone());
        Arc::new(store)
    }

    fn saved(&self) -> Option<TokenSet> {
        self.saved.lock().unwrap().clone()
    }

    fn attempted(&self, access: &str) -> bool {
        self.attempts
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.access.expose() == access)
    }
}

impl TokenStore for MemStore {
    fn load(&self) -> BoxFuture<'_, Option<TokenSet>> {
        let saved = self.saved();
        Box::pin(async move { saved })
    }

    fn save<'a>(&'a self, tokens: &'a TokenSet) -> BoxFuture<'a, Result<(), String>> {
        let delay = *self.delay.lock().unwrap();
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            self.attempts.lock().unwrap().push(tokens.clone());
            // Saves never overlap: the client refreshes one at a time.
            let left = self.fail_next.load(Ordering::SeqCst);
            if left > 0 {
                self.fail_next.store(left - 1, Ordering::SeqCst);
                return Err("keyring locked".into());
            }
            *self.saved.lock().unwrap() = Some(tokens.clone());
            Ok(())
        })
    }
}

/// Counts bearer tokens that went out before the store saw them, and
/// requests that carried the starting access token.
struct Watch {
    inner: Arc<dyn Transport>,
    store: Arc<MemStore>,
    initial: String,
    early: AtomicUsize,
    initial_sent: AtomicUsize,
}

impl Watch {
    fn check(&self, req: &HttpRequest) {
        let Some(token) = req
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return;
        };
        if token == self.initial {
            self.initial_sent.fetch_add(1, Ordering::SeqCst);
        } else if !self.store.attempted(token) {
            self.early.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Transport for Watch {
    fn send(&self, req: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        self.check(&req);
        self.inner.send(req)
    }

    fn stream(
        &self,
        req: HttpRequest,
    ) -> BoxFuture<'static, Result<StreamingResponse, TransportError>> {
        self.check(&req);
        self.inner.stream(req)
    }
}

struct Session {
    client: Client,
    watch: Arc<Watch>,
}

fn oauth_client(env: &Env, l: &Login, tokens: TokenSet, store: &Arc<MemStore>) -> Session {
    let watch = Arc::new(Watch {
        inner: env.transport.clone(),
        store: store.clone(),
        initial: tokens.access.expose().to_owned(),
        early: AtomicUsize::new(0),
        initial_sent: AtomicUsize::new(0),
    });
    let client = Client::with_oauth(
        watch.clone(),
        env.handle.session_url(),
        OAuthCredentials {
            tokens,
            token_endpoint: l.meta.token_endpoint.clone(),
            client_id: l.client_id.clone(),
            store: store.clone(),
        },
    )
    .unwrap();
    Session { client, watch }
}

async fn mailbox_call(client: &Client, account: &str) -> Result<(), Error> {
    let mut req = RequestBuilder::new();
    let h = req.call::<MailboxGet>(GetArgs::all(account));
    let resp = client.call(&req.build().unwrap()).await?;
    resp.get(&h)?;
    Ok(())
}

fn refresh_tokens_seen(store: &MemStore) -> BTreeSet<String> {
    store
        .attempts
        .lock()
        .unwrap()
        .iter()
        .map(|t| t.refresh.expose().to_owned())
        .collect()
}

#[tokio::test]
async fn concurrent_401s_share_one_refresh() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    s.client.fetch_session().await.unwrap();
    env.server.expire_access_tokens();

    let account = env.server.account_id().to_owned();
    let results = join_all((0..8).map(|_| mailbox_call(&s.client, &account))).await;
    for r in results {
        r.unwrap();
    }
    assert_eq!(env.server.oauth_refresh_requests(), 1);
    assert_eq!(env.server.oauth_refreshes(), 1);
    assert!(!env.server.oauth_refresh_token_reused());
    assert_eq!(store.saved(), s.client.oauth_tokens());
    assert_ne!(store.saved().unwrap().refresh, l.tokens.refresh);
    assert_eq!(s.watch.early.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rotations_never_reuse_and_always_save_first() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();
    for _ in 0..5 {
        env.server.expire_access_tokens();
        mailbox_call(&s.client, &account).await.unwrap();
    }
    assert_eq!(env.server.oauth_refreshes(), 5);
    assert_eq!(env.server.oauth_refresh_requests(), 5);
    assert!(!env.server.oauth_refresh_token_reused());
    assert_eq!(refresh_tokens_seen(&store).len(), 5);
    assert_eq!(s.watch.early.load(Ordering::SeqCst), 0);
    assert_eq!(env.server.oauth_grants().len(), 1);
}

#[tokio::test]
async fn a_new_token_waits_for_its_save() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    *store.delay.lock().unwrap() = std::time::Duration::from_millis(300);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();
    env.server.expire_access_tokens();

    // The second call starts while the first one's save is still running.
    let first = mailbox_call(&s.client, &account);
    let second = async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        mailbox_call(&s.client, &account).await
    };
    let (a, b) = futures::join!(first, second);
    a.unwrap();
    b.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
    assert_eq!(s.watch.early.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_token_about_to_expire_is_refreshed_first() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let mut tokens = l.tokens.clone();
    tokens.expires_at = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_secs(10))
        .unwrap();
    let store = MemStore::with(&tokens);
    let s = oauth_client(&env, &l, tokens, &store);
    s.client.fetch_session().await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
    // The old access token never went out.
    assert_eq!(s.watch.initial_sent.load(Ordering::SeqCst), 0);
    let fresh = s.client.oauth_tokens().unwrap();
    assert_ne!(fresh.access, l.tokens.access);
    s.client.fetch_session().await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
}

#[tokio::test]
async fn a_failed_save_keeps_working_and_saves_next_time() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    store.fail_next.store(1, Ordering::SeqCst);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();

    env.server.expire_access_tokens();
    mailbox_call(&s.client, &account).await.unwrap();
    assert_eq!(store.saved(), Some(l.tokens.clone()), "the save failed");
    let in_memory = s.client.oauth_tokens().unwrap();
    assert_ne!(in_memory.refresh, l.tokens.refresh);

    // The store still holds the spent token; the next refresh must not
    // adopt it, and saves the newest tokens.
    env.server.expire_access_tokens();
    mailbox_call(&s.client, &account).await.unwrap();
    assert_eq!(store.saved(), s.client.oauth_tokens());
    assert_eq!(env.server.oauth_refreshes(), 2);
    assert!(!env.server.oauth_refresh_token_reused());
}

#[tokio::test]
async fn a_refused_refresh_is_an_auth_error_and_sticks() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();
    env.server.quirks_mut().refresh_temporary_invalid_grant_next = 1;
    env.server.expire_access_tokens();

    let results = join_all((0..4).map(|_| mailbox_call(&s.client, &account))).await;
    for r in results {
        let e = r.unwrap_err();
        assert!(e.is_unauthorized(), "{e:?}");
        let oauth = e.oauth_error().unwrap();
        assert_eq!(oauth.error, OAuthErrorKind::InvalidGrant);
        assert!(oauth.temporary);
    }
    assert_eq!(env.server.oauth_refresh_requests(), 1);
    // Later calls fail the same way without asking the server again.
    assert!(
        mailbox_call(&s.client, &account)
            .await
            .unwrap_err()
            .is_unauthorized()
    );
    assert_eq!(env.server.oauth_refresh_requests(), 1);

    // New credentials start over; the lock has passed by now.
    s.client
        .set_oauth(OAuthCredentials {
            tokens: s.client.oauth_tokens().unwrap(),
            token_endpoint: l.meta.token_endpoint.clone(),
            client_id: l.client_id.clone(),
            store: store.clone(),
        })
        .unwrap();
    mailbox_call(&s.client, &account).await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
}

#[tokio::test]
async fn a_403_never_refreshes() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    s.client.fetch_session().await.unwrap();
    env.server.quirks_mut().http_403_next = 1;
    match mailbox_call(&s.client, env.server.account_id()).await {
        Err(Error::Unauthorized { status: 403, .. }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(env.server.oauth_refresh_requests(), 0);
    mailbox_call(&s.client, env.server.account_id())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_lost_refresh_is_retried_once_with_the_same_token() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();
    env.server.quirks_mut().lose_refresh_response_next = 1;
    env.server.expire_access_tokens();

    mailbox_call(&s.client, &account).await.unwrap();
    // Both refreshes took effect; the second used the first one's token.
    assert_eq!(env.server.oauth_refreshes(), 2);
    assert_eq!(env.server.oauth_refresh_requests(), 2);
    assert!(!env.server.oauth_refresh_token_reused());
    assert_eq!(env.server.oauth_grants().len(), 1);
    assert_eq!(store.saved(), s.client.oauth_tokens());

    // And the login keeps working afterwards.
    env.server.expire_access_tokens();
    mailbox_call(&s.client, &account).await.unwrap();
    assert!(!env.server.oauth_refresh_token_reused());
}

#[tokio::test]
async fn two_lost_refreshes_end_the_login() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    s.client.fetch_session().await.unwrap();
    env.server.quirks_mut().lose_refresh_response_next = 2;
    env.server.expire_access_tokens();

    let e = mailbox_call(&s.client, &account).await.unwrap_err();
    assert!(matches!(e, Error::RefreshLost), "{e:?}");
    assert!(e.is_unauthorized());
    assert_eq!(env.server.oauth_refresh_requests(), 2);
    assert!(matches!(
        mailbox_call(&s.client, &account).await,
        Err(Error::RefreshLost)
    ));
    assert_eq!(env.server.oauth_refresh_requests(), 2);
    assert!(!env.server.oauth_refresh_token_reused());
}

#[tokio::test]
async fn an_unreachable_token_endpoint_is_a_network_error() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    // Bind and drop a listener to find a port nothing listens on.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let client = Client::with_oauth(
        env.transport.clone(),
        env.handle.session_url(),
        OAuthCredentials {
            tokens: l.tokens.clone(),
            token_endpoint: format!("http://{closed}/oauth/refresh"),
            client_id: l.client_id.clone(),
            store,
        },
    )
    .unwrap();
    client.fetch_session().await.unwrap();
    env.server.expire_access_tokens();
    // The request never left, so it does not count towards the retry, and
    // the error stays a passing network problem every time.
    for _ in 0..3 {
        let e = client.fetch_session().await.unwrap_err();
        assert!(
            matches!(e, Error::Transport(TransportError::Connect(_))),
            "{e:?}"
        );
        assert!(e.is_transient());
        assert!(!e.is_unauthorized());
    }
}

#[tokio::test]
async fn a_login_refreshed_by_another_process_is_adopted() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let a = oauth_client(&env, &l, l.tokens.clone(), &store);
    let b = oauth_client(&env, &l, l.tokens.clone(), &store);
    let account = env.server.account_id().to_owned();
    a.client.fetch_session().await.unwrap();
    b.client.fetch_session().await.unwrap();

    env.server.expire_access_tokens();
    mailbox_call(&a.client, &account).await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
    // B's access token is expired too. It finds A's saved login and uses
    // it without a refresh of its own.
    mailbox_call(&b.client, &account).await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
    assert_eq!(b.client.oauth_tokens(), a.client.oauth_tokens());

    // Now B refreshes, and A adopts B's login.
    env.server.expire_access_tokens();
    mailbox_call(&b.client, &account).await.unwrap();
    mailbox_call(&a.client, &account).await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 2);
    assert_eq!(b.client.oauth_tokens(), a.client.oauth_tokens());
    assert!(!env.server.oauth_refresh_token_reused());
    assert_eq!(env.server.oauth_grants().len(), 1);
}

#[tokio::test]
async fn push_reconnects_with_a_refreshed_token() {
    let env = start().await;
    let l = login(&env, SCOPES).await;
    let store = MemStore::with(&l.tokens);
    let s = oauth_client(&env, &l, l.tokens.clone(), &store);
    s.client.fetch_session().await.unwrap();
    let options = PushOptions {
        close_after_state: true,
        ping: 0,
        ..PushOptions::default()
    };
    drop(s.client.push(&options).await.unwrap());

    env.server.expire_access_tokens();
    let mut stream = s.client.push(&options).await.unwrap();
    assert_eq!(env.server.oauth_refreshes(), 1);
    assert_eq!(s.watch.early.load(Ordering::SeqCst), 0);
    // The stream works: a change arrives on it.
    env.server
        .deliver_incoming(mejl_fake::IncomingMessage::new(
            mejl_fake::Address::new("Ada", "ada@example.com"),
            "Hi",
        ))
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    assert!(matches!(event, Some(Ok(_))), "{event:?}");

    // A refresh made by a call is picked up by the next connect.
    env.server.expire_access_tokens();
    mailbox_call(&s.client, env.server.account_id())
        .await
        .unwrap();
    assert_eq!(env.server.oauth_refreshes(), 2);
    drop(s.client.push(&options).await.unwrap());
    assert_eq!(env.server.oauth_refreshes(), 2);
}
