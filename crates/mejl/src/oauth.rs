//! The stored OAuth login, and the [`TokenStore`] that keeps it current.
//!
//! An OAuth login lives in its own keyring entry, user `<account>:oauth`,
//! next to the API token entry. It is one compact JSON object:
//!
//! ```json
//! {"v":1,"client":"…","from":"registered","iss":"https://api.fastmail.com",
//!  "token":"https://api.fastmail.com/oauth/refresh",
//!  "revoke":"https://api.fastmail.com/oauth/revoke",
//!  "at":"…","rt":"…","exp":1761400000,"scope":"urn:… offline_access"}
//! ```
//!
//! - `client` is the client id, and `from` says where it came from:
//!   `registered` by `mejl login`, or `config` for `oauth_client_id`.
//! - `iss` is the issuer, `token` the token endpoint and `revoke` the
//!   revocation endpoint, if there is one.
//! - `at` and `rt` are the access and refresh tokens, `exp` the access
//!   token's expiry in Unix seconds and `scope` the granted scope.
//!
//! Keys are short and the expiry is a number because some keyrings, such
//! as Windows Credential Manager, cap a secret at a few KB.
//!
//! [`LoginStore`] is what a [`Client`] built with
//! [`Client::with_oauth`] saves its refreshed tokens through. It reads and
//! writes the record on a plain thread, because the keyring may block on
//! D-Bus and its calls must not run inside the client's refresh on an async
//! thread. Every token it sees is registered with the log redactor.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use jiff::Timestamp;
use mejl_jmap::transport::{BoxFuture, Transport};
use mejl_jmap::{Client, OAuthCredentials, Secret, TokenSet, TokenStore};
use serde::{Deserialize, Serialize};

use crate::secrets::{SecretError, SecretStore};

/// The record format version.
const VERSION: u32 = 1;

/// Where an OAuth client id came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSource {
    /// `mejl login` registered it through the server's open registration.
    Registered,
    /// The account's `oauth_client_id` in the config.
    Config,
}

impl ClientSource {
    fn key(self) -> &'static str {
        match self {
            ClientSource::Registered => "registered",
            ClientSource::Config => "config",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "registered" => Some(ClientSource::Registered),
            "config" => Some(ClientSource::Config),
            _ => None,
        }
    }

    /// Plain words for `mejl doctor`.
    pub fn describe(self) -> &'static str {
        match self {
            ClientSource::Registered => "registered by `mejl login`",
            ClientSource::Config => "oauth_client_id in the config",
        }
    }
}

/// An OAuth login as it is stored. `Debug` never shows a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRecord {
    /// The client id the login belongs to.
    pub client_id: String,
    /// Where the client id came from.
    pub client_source: ClientSource,
    /// The authorization server's issuer.
    pub issuer: String,
    /// The token endpoint, for refreshes.
    pub token_endpoint: String,
    /// The revocation endpoint, for `mejl logout`.
    pub revocation_endpoint: Option<String>,
    /// The tokens, expiry and granted scope.
    pub tokens: TokenSet,
}

#[derive(Serialize, Deserialize)]
struct Raw {
    v: u32,
    client: String,
    from: String,
    iss: String,
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoke: Option<String>,
    at: String,
    rt: String,
    exp: i64,
    #[serde(default)]
    scope: String,
}

/// The fixed text for a record that does not parse. A serde message could
/// quote a token.
const DAMAGED: &str = "the stored OAuth login is damaged; run `mejl login` again";

impl OAuthRecord {
    /// The record as compact JSON. It holds both tokens.
    pub fn to_json(&self) -> Secret {
        let raw = Raw {
            v: VERSION,
            client: self.client_id.clone(),
            from: self.client_source.key().to_owned(),
            iss: self.issuer.clone(),
            token: self.token_endpoint.clone(),
            revoke: self.revocation_endpoint.clone(),
            at: self.tokens.access.expose().to_owned(),
            rt: self.tokens.refresh.expose().to_owned(),
            exp: self.tokens.expires_at.as_second(),
            scope: self.tokens.scope.join(" "),
        };
        // Serializing plain strings and numbers cannot fail.
        Secret::new(serde_json::to_string(&raw).unwrap_or_default())
    }

    /// Reads a stored record. The error never quotes the text. Endpoints
    /// must be https, or http on a loopback host, so a changed record
    /// cannot send a token in the clear.
    pub fn from_json(text: &str) -> Result<Self, SecretError> {
        let damaged = || SecretError::Corrupt(DAMAGED.to_owned());
        let raw: Raw = serde_json::from_str(text).map_err(|_| damaged())?;
        if raw.v != VERSION {
            return Err(SecretError::Corrupt(format!(
                "the stored OAuth login has format {}, which this mejl does not know; run `mejl login` again",
                raw.v
            )));
        }
        let client_source = ClientSource::from_key(&raw.from).ok_or_else(damaged)?;
        if raw.client.is_empty() || raw.at.is_empty() || raw.rt.is_empty() {
            return Err(damaged());
        }
        let endpoints = std::iter::once(&raw.token).chain(raw.revoke.as_ref());
        for url in endpoints {
            mejl_jmap::oauth::check_endpoint(url).map_err(|e| {
                SecretError::Corrupt(format!("the stored OAuth login has a bad endpoint: {e}"))
            })?;
        }
        let expires_at = Timestamp::from_second(raw.exp).map_err(|_| damaged())?;
        Ok(OAuthRecord {
            client_id: raw.client,
            client_source,
            issuer: raw.iss,
            token_endpoint: raw.token,
            revocation_endpoint: raw.revoke,
            tokens: TokenSet {
                access: Secret::new(raw.at),
                refresh: Secret::new(raw.rt),
                expires_at,
                scope: raw.scope.split_whitespace().map(str::to_owned).collect(),
            },
        })
    }

    /// Registers both tokens with the log redactor.
    pub fn register_secrets(&self) {
        register_tokens(&self.tokens);
    }

    /// The record with other tokens, for the same client and server.
    pub fn with_tokens(&self, tokens: TokenSet) -> Self {
        OAuthRecord {
            tokens,
            ..self.clone()
        }
    }
}

fn register_tokens(tokens: &TokenSet) {
    crate::logging::register_secret(&tokens.access);
    crate::logging::register_secret(&tokens.refresh);
}

type SaveHook = Box<dyn Fn(String) + Send + Sync>;

/// The [`TokenStore`] of the TUI's client, backed by the account's OAuth
/// record in a [`SecretStore`].
///
/// - `load` reads the record again, so a login saved by another mejl
///   process is adopted. A record for another client or server is ignored.
///   So is a record with a refresh token this store already held before its
///   latest one: after a failed save the keyring still has a rotated token,
///   and sending that again would make the server revoke the login. A
///   client that starts over, as after a lock, must not take it for a newer
///   login.
/// - `save` registers the new tokens with the log redactor, then writes the
///   record. The first failed write calls the hook set with
///   [`LoginStore::on_save_error`], once per run.
pub struct LoginStore {
    secrets: Arc<dyn SecretStore>,
    account: String,
    template: OAuthRecord,
    /// Every refresh token this store was given, oldest first: the one it
    /// started with, then each one passed to `save`.
    held: Mutex<Vec<Secret>>,
    hook: OnceLock<SaveHook>,
    warned: AtomicBool,
}

impl fmt::Debug for LoginStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginStore")
            .field("account", &self.account)
            .field("client_id", &self.template.client_id)
            .finish_non_exhaustive()
    }
}

impl LoginStore {
    /// A store for `account`'s login, which belongs to `record`'s client
    /// and server.
    pub fn new(secrets: Arc<dyn SecretStore>, account: &str, record: &OAuthRecord) -> Self {
        LoginStore {
            secrets,
            account: account.to_owned(),
            template: record.clone(),
            held: Mutex::new(vec![record.tokens.refresh.clone()]),
            hook: OnceLock::new(),
            warned: AtomicBool::new(false),
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Vec<Secret>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `refresh` is a token this store held before its latest one.
    fn superseded(&self, refresh: &Secret) -> bool {
        let held = self.held();
        held.split_last()
            .is_some_and(|(_, older)| older.contains(refresh))
    }

    /// Calls `hook` with the reason the first time a save fails. Only the
    /// first hook set counts.
    pub fn on_save_error(&self, hook: impl Fn(String) + Send + Sync + 'static) {
        let _ = self.hook.set(Box::new(hook));
    }

    fn same_login(&self, record: &OAuthRecord) -> bool {
        record.client_id == self.template.client_id
            && record.token_endpoint == self.template.token_endpoint
    }
}

/// Runs `job` on a new plain thread and waits for it without blocking the
/// async thread. `None` when the thread could not start or panicked.
async fn off_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("mejl-keyring".into())
        .spawn(move || {
            let _ = tx.send(job());
        });
    if spawned.is_err() {
        return None;
    }
    rx.await.ok()
}

impl TokenStore for LoginStore {
    fn load(&self) -> BoxFuture<'_, Option<TokenSet>> {
        Box::pin(async move {
            let secrets = self.secrets.clone();
            let account = self.account.clone();
            let found = off_thread(move || secrets.get_oauth(&account)).await;
            match found {
                Some(Ok(Some(record))) if self.same_login(&record) => {
                    record.register_secrets();
                    if self.superseded(&record.tokens.refresh) {
                        tracing::debug!("the stored OAuth login is older than the one in use");
                        return None;
                    }
                    Some(record.tokens)
                }
                Some(Ok(_)) => None,
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "cannot read the stored OAuth login");
                    None
                }
                None => None,
            }
        })
    }

    fn save<'a>(&'a self, tokens: &'a TokenSet) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            register_tokens(tokens);
            {
                let mut held = self.held();
                if held.last() != Some(&tokens.refresh) {
                    held.push(tokens.refresh.clone());
                }
            }
            let record = self.template.with_tokens(tokens.clone());
            let secrets = self.secrets.clone();
            let account = self.account.clone();
            let result = off_thread(move || secrets.set_oauth(&account, &record))
                .await
                .unwrap_or_else(|| Err(SecretError::Other("the keyring thread failed".into())))
                .map_err(|e| e.to_string());
            if let Err(why) = &result
                && !self.warned.swap(true, Ordering::SeqCst)
                && let Some(hook) = self.hook.get()
            {
                hook(why.clone());
            }
            result
        })
    }
}

/// Revokes a stored login's refresh token at the server, which ends the
/// whole grant (RFC 7009). It needs no refresh, so `mejl logout` and a
/// token login may call it.
pub async fn revoke(transport: &dyn Transport, record: &OAuthRecord) -> Result<(), String> {
    let Some(endpoint) = &record.revocation_endpoint else {
        return Err("the server offers no way to revoke a login".into());
    };
    mejl_jmap::oauth::revoke(
        transport,
        endpoint,
        &record.client_id,
        &record.tokens.refresh,
    )
    .await
    .map_err(|e| e.to_string())
}

/// What the TUI needs for an OAuth login: the client's credentials, also
/// kept to reset the client after a temporary lock, and the store, to hook
/// up the save warning.
#[derive(Debug, Clone)]
pub struct OAuthSession {
    /// The credentials the client was built with.
    pub credentials: OAuthCredentials,
    /// The store inside them.
    pub store: Arc<LoginStore>,
}

/// A client for an OAuth login that refreshes itself and saves through a
/// [`LoginStore`] over `secrets`. Only the TUI may build one: it refreshes
/// on its own, and only one process should refresh.
pub fn oauth_client(
    transport: Arc<dyn Transport>,
    session_url: &str,
    secrets: Arc<dyn SecretStore>,
    account: &str,
    record: OAuthRecord,
) -> Result<(Client, OAuthSession), mejl_jmap::Error> {
    record.register_secrets();
    let store = Arc::new(LoginStore::new(secrets, account, &record));
    let credentials = OAuthCredentials {
        tokens: record.tokens,
        token_endpoint: record.token_endpoint,
        client_id: record.client_id,
        store: store.clone(),
    };
    let client = Client::with_oauth(transport, session_url, credentials.clone())?;
    Ok((client, OAuthSession { credentials, store }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    pub(crate) fn record() -> OAuthRecord {
        OAuthRecord {
            client_id: "client-1".into(),
            client_source: ClientSource::Registered,
            issuer: "https://api.example.com".into(),
            token_endpoint: "https://api.example.com/oauth/refresh".into(),
            revocation_endpoint: Some("https://api.example.com/oauth/revoke".into()),
            tokens: TokenSet {
                access: Secret::new("access-token-0001"),
                refresh: Secret::new("refresh-token-0001"),
                expires_at: Timestamp::from_second(1_761_400_000).unwrap(),
                scope: vec![
                    "urn:ietf:params:oauth:scope:mail".into(),
                    "offline_access".into(),
                ],
            },
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn record_round_trip_is_compact() {
        let r = record();
        let json = r.to_json();
        assert_eq!(OAuthRecord::from_json(json.expose()).unwrap(), r);
        assert!(json.expose().len() < 400, "{}", json.expose().len());
        assert!(json.expose().contains(r#""exp":1761400000"#));
        assert!(json.expose().contains(r#""from":"registered""#));
        // Debug never shows a token.
        let shown = format!("{r:?}");
        assert!(!shown.contains("access-token"), "{shown}");
        assert!(!shown.contains("refresh-token"), "{shown}");

        let mut none = r.clone();
        none.revocation_endpoint = None;
        none.client_source = ClientSource::Config;
        let json = none.to_json();
        assert!(!json.expose().contains("revoke"));
        assert_eq!(OAuthRecord::from_json(json.expose()).unwrap(), none);
    }

    #[test]
    fn damaged_records_never_echo_the_text() {
        let good = record().to_json();
        for bad in [
            "not json at all".to_owned(),
            good.expose()
                .replace(r#""v":1"#, r#""v":"secret-in-version""#),
            good.expose().replace("registered", "elsewhere"),
            good.expose().replace("access-token-0001", ""),
            good.expose().replace(
                "https://api.example.com/oauth/refresh",
                "http://api.example.com/oauth/refresh",
            ),
        ] {
            let e = OAuthRecord::from_json(&bad).unwrap_err().to_string();
            assert!(!e.contains("access-token"), "{e}");
            assert!(!e.contains("refresh-token"), "{e}");
            assert!(!e.contains("secret-in"), "{e}");
        }
        let newer = good.expose().replace(r#""v":1"#, r#""v":2"#);
        assert!(
            OAuthRecord::from_json(&newer)
                .unwrap_err()
                .to_string()
                .contains("format 2")
        );
    }

    #[test]
    fn login_store_loads_saves_and_ignores_other_logins() {
        let secrets = Arc::new(MemoryStore::new());
        let r = record();
        secrets.set_oauth("default", &r).unwrap();
        let store = LoginStore::new(secrets.clone(), "default", &r);
        assert_eq!(block_on(store.load()), Some(r.tokens.clone()));

        let mut newer = r.tokens.clone();
        newer.access = Secret::new("access-token-0002");
        newer.refresh = Secret::new("refresh-token-0002");
        block_on(store.save(&newer)).unwrap();
        let saved = secrets.get_oauth("default").unwrap().unwrap();
        assert_eq!(saved, r.with_tokens(newer.clone()));
        assert_eq!(block_on(store.load()), Some(newer));

        // A login for another client is not adopted.
        let mut other = r.clone();
        other.client_id = "client-2".into();
        secrets.set_oauth("default", &other).unwrap();
        assert_eq!(block_on(store.load()), None);
        secrets.delete_oauth("default").unwrap();
        assert_eq!(block_on(store.load()), None);
    }

    #[test]
    fn a_token_this_store_already_replaced_is_never_loaded() {
        let secrets = Arc::new(MemoryStore::new());
        let r = record();
        secrets.set_oauth("default", &r).unwrap();
        let store = LoginStore::new(secrets.clone(), "default", &r);
        let mut newer = r.tokens.clone();
        newer.refresh = Secret::new("refresh-token-0002");
        // The save fails, so the store keeps the first token.
        secrets.fail_with(Some("locked"));
        assert!(block_on(store.save(&newer)).is_err());
        secrets.fail_with(None);
        assert_eq!(block_on(store.load()), None, "the stored token is older");
        // A token from another process is still adopted.
        let mut other = r.tokens.clone();
        other.refresh = Secret::new("refresh-token-from-elsewhere");
        secrets
            .set_oauth("default", &r.with_tokens(other.clone()))
            .unwrap();
        assert_eq!(block_on(store.load()), Some(other));
    }

    #[test]
    fn a_failed_save_calls_the_hook_once() {
        let secrets = Arc::new(MemoryStore::new());
        let r = record();
        let store = LoginStore::new(secrets.clone(), "default", &r);
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        store.on_save_error(move |why| seen.lock().unwrap().push(why));
        secrets.fail_with(Some("locked"));
        assert!(block_on(store.save(&r.tokens)).is_err());
        assert!(block_on(store.save(&r.tokens)).is_err());
        assert_eq!(block_on(store.load()), None);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("locked"), "{calls:?}");
    }
}
