//! Login storage: API tokens and OAuth logins.
//!
//! Both live in the OS keyring under service [`KEYRING_SERVICE`]. An API
//! token's entry is named after the account; an OAuth login's entry is
//! `<account>:oauth` and holds an [`OAuthRecord`]. A `:` cannot appear in
//! an account name, so the two never collide. Secrets are held in memory as
//! [`mejl_jmap::Secret`], whose `Debug` and `Display` print `[redacted]`,
//! and each one is registered with the log redactor as soon as it is read.
//!
//! Where the login comes from, first match wins ([`login_source`] and
//! [`resolve_login`]):
//!
//! 1. `MEJL_TOKEN` in the environment, for scripted use. It applies to the
//!    selected account and is never written anywhere.
//! 2. The account's `token_command`, for systems without a keyring.
//! 3. The OAuth login that `mejl login` stored.
//! 4. The API token that `mejl login` stored.
//!
//! An account has one stored login at a time: an OAuth login removes the
//! stored token, and a token login removes the OAuth login.
//!
//! Tests use [`MemoryStore`]. Nothing in a test may build a
//! [`KeyringStore`]: the first keyring call connects to the real Secret
//! Service over D-Bus.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use mejl_jmap::Secret;

use crate::config::AccountConfig;
use crate::env::{Env, var};
use crate::oauth::OAuthRecord;

/// The keyring service name.
pub const KEYRING_SERVICE: &str = "mejl";
/// The longest token accepted. Fastmail tokens are far shorter.
pub const MAX_TOKEN_LEN: usize = 4096;
/// The longest stored OAuth login accepted. It holds two tokens and a few
/// URLs.
pub const MAX_RECORD_LEN: usize = 16 * 1024;

/// The keyring user name of an account's OAuth login.
pub fn oauth_entry(account: &str) -> String {
    format!("{account}:oauth")
}

/// Failures of a secret store. Messages never contain the secret.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The keyring is missing, locked or refused access.
    #[error("the OS keyring is not available: {0}")]
    Unavailable(String),
    /// The stored value is not a usable token.
    #[error("the stored token is damaged: {0}")]
    Corrupt(String),
    /// `token_command` failed.
    #[error("token_command failed: {0}")]
    Command(String),
    /// Any other store failure.
    #[error("keyring error: {0}")]
    Other(String),
}

/// A place logins are kept: one API token and one OAuth login per account
/// name.
pub trait SecretStore: Send + Sync {
    /// The token for an account, or `None` when there is none.
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError>;
    /// Stores or replaces the token for an account.
    fn set(&self, account: &str, token: &Secret) -> Result<(), SecretError>;
    /// Removes the token. Returns whether one existed.
    fn delete(&self, account: &str) -> Result<bool, SecretError>;
    /// The OAuth login for an account, or `None` when there is none.
    fn get_oauth(&self, account: &str) -> Result<Option<OAuthRecord>, SecretError>;
    /// Stores or replaces the OAuth login for an account.
    fn set_oauth(&self, account: &str, record: &OAuthRecord) -> Result<(), SecretError>;
    /// Removes the OAuth login. Returns whether one existed.
    fn delete_oauth(&self, account: &str) -> Result<bool, SecretError>;
    /// A short name for messages, such as "OS keyring".
    fn describe(&self) -> &'static str;
}

/// The OS keyring: Keychain on macOS, Credential Manager on Windows and the
/// Secret Service elsewhere.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// The store for service [`KEYRING_SERVICE`].
    pub fn new() -> Self {
        KeyringStore {
            service: KEYRING_SERVICE.to_owned(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(map_keyring_error)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

// keyring's errors can carry the raw secret bytes (BadEncoding,
// BadDataFormat), so they are turned into words here and never formatted
// with Debug.
fn map_keyring_error(e: keyring::Error) -> SecretError {
    use keyring::Error as K;
    match e {
        K::NoStorageAccess(inner) => SecretError::Unavailable(inner.to_string()),
        K::NoDefaultStore => SecretError::Unavailable(
            "no keyring service was found; set token_command in the config or MEJL_TOKEN".into(),
        ),
        K::PlatformFailure(inner) => SecretError::Unavailable(inner.to_string()),
        K::BadEncoding(_) => SecretError::Corrupt("it is not UTF-8 text".into()),
        K::BadDataFormat(_, _) => SecretError::Corrupt("the keyring could not decode it".into()),
        K::NoEntry => SecretError::Other("no entry".into()),
        K::TooLong(what, limit) => SecretError::Other(format!(
            "{what} is longer than the keyring allows ({limit})"
        )),
        K::Invalid(what, why) => SecretError::Other(format!("{what}: {why}")),
        K::Ambiguous(_) => SecretError::Other("more than one keyring entry matches".into()),
        _ => SecretError::Other("unexpected keyring failure".into()),
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => check_token(&value)
                .map(Some)
                .map_err(|e| SecretError::Corrupt(e.to_string())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring_error(e)),
        }
    }

    fn set(&self, account: &str, token: &Secret) -> Result<(), SecretError> {
        self.entry(account)?
            .set_password(token.expose())
            .map_err(map_keyring_error)
    }

    fn delete(&self, account: &str) -> Result<bool, SecretError> {
        delete_entry(&self.entry(account)?)
    }

    fn get_oauth(&self, account: &str) -> Result<Option<OAuthRecord>, SecretError> {
        match self.entry(&oauth_entry(account))?.get_password() {
            Ok(value) => parse_record(&value).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring_error(e)),
        }
    }

    fn set_oauth(&self, account: &str, record: &OAuthRecord) -> Result<(), SecretError> {
        self.entry(&oauth_entry(account))?
            .set_password(record.to_json().expose())
            .map_err(map_keyring_error)
    }

    fn delete_oauth(&self, account: &str) -> Result<bool, SecretError> {
        delete_entry(&self.entry(&oauth_entry(account))?)
    }

    fn describe(&self) -> &'static str {
        "OS keyring"
    }
}

fn delete_entry(entry: &keyring::Entry) -> Result<bool, SecretError> {
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(map_keyring_error(e)),
    }
}

/// Checks a stored OAuth login's size, then parses it.
fn parse_record(text: &str) -> Result<OAuthRecord, SecretError> {
    if text.len() > MAX_RECORD_LEN {
        return Err(SecretError::Corrupt(format!(
            "the stored OAuth login is longer than {MAX_RECORD_LEN} bytes"
        )));
    }
    OAuthRecord::from_json(text)
}

/// An in-memory store for tests. OAuth logins are kept as the same JSON
/// text the keyring gets.
#[derive(Debug, Default)]
pub struct MemoryStore {
    tokens: Mutex<HashMap<String, String>>,
    records: Mutex<HashMap<String, String>>,
    fail_with: Mutex<Option<String>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call fail as an unavailable keyring would, or
    /// clears that with `None`.
    pub fn fail_with(&self, message: Option<&str>) {
        *self
            .fail_with
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = message.map(Into::into);
    }

    fn check(&self) -> Result<(), SecretError> {
        match &*self
            .fail_with
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            Some(m) => Err(SecretError::Unavailable(m.clone())),
            None => Ok(()),
        }
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        self.check()?;
        Ok(self
            .tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .map(|t| Secret::new(t.clone())))
    }

    fn set(&self, account: &str, token: &Secret) -> Result<(), SecretError> {
        self.check()?;
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_owned(), token.expose().to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, SecretError> {
        self.check()?;
        Ok(self
            .tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account)
            .is_some())
    }

    fn get_oauth(&self, account: &str) -> Result<Option<OAuthRecord>, SecretError> {
        self.check()?;
        let text = self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned();
        text.map(|t| parse_record(&t)).transpose()
    }

    fn set_oauth(&self, account: &str, record: &OAuthRecord) -> Result<(), SecretError> {
        self.check()?;
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_owned(), record.to_json().expose().to_owned());
        Ok(())
    }

    fn delete_oauth(&self, account: &str) -> Result<bool, SecretError> {
        self.check()?;
        Ok(self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account)
            .is_some())
    }

    fn describe(&self) -> &'static str {
        "memory store"
    }
}

/// Why a string is not a usable token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenProblem {
    /// Nothing was entered.
    #[error("the token is empty")]
    Empty,
    /// Spaces or control characters inside: usually a bad copy.
    #[error("the token has spaces or control characters inside; copy it again")]
    BadCharacters,
    /// Longer than [`MAX_TOKEN_LEN`].
    #[error("the token is longer than {MAX_TOKEN_LEN} characters")]
    TooLong,
}

/// Trims surrounding whitespace and checks a token's shape.
pub fn check_token(raw: &str) -> Result<Secret, TokenProblem> {
    let token = raw.trim();
    if token.is_empty() {
        return Err(TokenProblem::Empty);
    }
    if token.len() > MAX_TOKEN_LEN {
        return Err(TokenProblem::TooLong);
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(TokenProblem::BadCharacters);
    }
    Ok(Secret::new(token))
}

/// Where a token came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// The `MEJL_TOKEN` variable.
    Env,
    /// The account's `token_command`.
    Command,
    /// The secret store, usually the OS keyring.
    Store,
}

impl TokenSource {
    /// Plain words for messages and `mejl doctor`.
    pub fn describe(self) -> &'static str {
        match self {
            TokenSource::Env => "the MEJL_TOKEN environment variable",
            TokenSource::Command => "token_command",
            TokenSource::Store => "the keyring",
        }
    }
}

/// Which login source wins for an account, found without running
/// `token_command`. Sources after the winner are not looked at, so with
/// `MEJL_TOKEN` set the keyring is never read.
#[derive(Debug)]
pub enum LoginSource {
    /// `MEJL_TOKEN` is set.
    Env,
    /// The account has a `token_command`.
    Command,
    /// A stored OAuth login.
    OAuth(Box<OAuthRecord>),
    /// A stored API token.
    Store(Secret),
}

/// Finds which source has the login for an account, without running
/// `token_command` and without registering anything. `Ok(None)` when no
/// source has one. `mejl doctor` uses this; [`resolve_login`] builds on it.
pub fn login_source(
    account: &AccountConfig,
    env: &Env,
    store: &dyn SecretStore,
) -> Result<Option<LoginSource>, SecretError> {
    if env.non_empty(var::MEJL_TOKEN).is_some() {
        return Ok(Some(LoginSource::Env));
    }
    if account.token_command.is_some() {
        return Ok(Some(LoginSource::Command));
    }
    if let Some(record) = store.get_oauth(&account.name)? {
        return Ok(Some(LoginSource::OAuth(Box::new(record))));
    }
    Ok(store.get(&account.name)?.map(LoginSource::Store))
}

/// A login, ready to use.
#[derive(Debug)]
pub enum Login {
    /// An API token and where it came from.
    Token(Secret, TokenSource),
    /// A stored OAuth login.
    OAuth(Box<OAuthRecord>),
}

/// Finds the login for an account, in the order of [`login_source`], and
/// runs `token_command` when that wins. Returns `Ok(None)` when no source
/// has one. Every token found is registered with the log redactor.
pub fn resolve_login(
    account: &AccountConfig,
    env: &Env,
    store: &dyn SecretStore,
) -> Result<Option<Login>, SecretError> {
    let found = match login_source(account, env, store)? {
        None => return Ok(None),
        Some(LoginSource::Env) => {
            let raw = env.get(var::MEJL_TOKEN).unwrap_or_default();
            let token =
                check_token(raw).map_err(|e| SecretError::Corrupt(format!("MEJL_TOKEN: {e}")))?;
            Login::Token(token, TokenSource::Env)
        }
        Some(LoginSource::Command) => {
            let command = account.token_command.as_deref().unwrap_or_default();
            Login::Token(run_token_command(command)?, TokenSource::Command)
        }
        Some(LoginSource::OAuth(record)) => {
            record.register_secrets();
            return Ok(Some(Login::OAuth(record)));
        }
        Some(LoginSource::Store(token)) => Login::Token(token, TokenSource::Store),
    };
    if let Login::Token(token, _) = &found {
        crate::logging::register_secret(token);
    }
    Ok(Some(found))
}

/// Runs a `token_command` through the shell and returns the first non-empty
/// line it prints. Its stdin and stderr stay on the terminal, so a password
/// manager can prompt.
pub fn run_token_command(command: &str) -> Result<Secret, SecretError> {
    let mut cmd = shell_command(command);
    let output = cmd
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| SecretError::Command(format!("cannot run it: {e}")))?;
    if !output.status.success() {
        return Err(SecretError::Command(format!(
            "it exited with {}",
            output.status
        )));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| SecretError::Command("its output is not UTF-8".into()))?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    check_token(line).map_err(|e| SecretError::Command(e.to_string()))
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        assert!(store.get("a").unwrap().is_none());
        store.set("a", &Secret::new("tok-1")).unwrap();
        store.set("b", &Secret::new("tok-2")).unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().expose(), "tok-1");
        store.set("a", &Secret::new("tok-3")).unwrap();
        assert_eq!(store.get("a").unwrap().unwrap().expose(), "tok-3");
        assert!(store.delete("a").unwrap());
        assert!(!store.delete("a").unwrap());
        assert_eq!(store.get("b").unwrap().unwrap().expose(), "tok-2");
    }

    #[test]
    fn memory_store_can_fail_like_a_locked_keyring() {
        let store = MemoryStore::new();
        store.fail_with(Some("locked"));
        assert!(matches!(store.get("a"), Err(SecretError::Unavailable(_))));
        assert!(store.set("a", &Secret::new("x")).is_err());
        store.fail_with(None);
        assert!(store.get("a").unwrap().is_none());
    }

    #[test]
    fn token_shape_checks() {
        assert_eq!(check_token("  fmu1-abc \n").unwrap().expose(), "fmu1-abc");
        assert_eq!(check_token("   ").unwrap_err(), TokenProblem::Empty);
        assert_eq!(
            check_token("ab cd").unwrap_err(),
            TokenProblem::BadCharacters
        );
        assert_eq!(
            check_token("ab\u{7}cd").unwrap_err(),
            TokenProblem::BadCharacters
        );
        assert_eq!(
            check_token(&"x".repeat(MAX_TOKEN_LEN + 1)).unwrap_err(),
            TokenProblem::TooLong
        );
        // The problem never echoes the token.
        let shown = check_token("secret value").unwrap_err().to_string();
        assert!(!shown.contains("secret"));
    }

    fn token(login: Option<Login>) -> (String, TokenSource) {
        match login {
            Some(Login::Token(t, src)) => (t.expose().to_owned(), src),
            other => panic!("{other:?}"),
        }
    }

    fn record() -> OAuthRecord {
        OAuthRecord {
            client_id: "client-1".into(),
            client_source: crate::oauth::ClientSource::Registered,
            issuer: "https://api.example.com".into(),
            token_endpoint: "https://api.example.com/oauth/refresh".into(),
            revocation_endpoint: None,
            tokens: mejl_jmap::TokenSet {
                access: Secret::new("oauth-access-0001"),
                refresh: Secret::new("oauth-refresh-0001"),
                expires_at: jiff::Timestamp::UNIX_EPOCH,
                scope: Vec::new(),
            },
        }
    }

    #[test]
    fn env_wins_over_command_and_store() {
        let store = MemoryStore::new();
        store.set("default", &Secret::new("from-store")).unwrap();
        store.set_oauth("default", &record()).unwrap();
        let mut account = AccountConfig::named("default");
        let env = Env::default().with(var::MEJL_TOKEN, "from-env");
        let found = resolve_login(&account, &env, &store).unwrap();
        assert_eq!(token(found), ("from-env".into(), TokenSource::Env));

        account.token_command = Some("exit 3".into());
        let found = resolve_login(&account, &env, &store).unwrap();
        assert_eq!(token(found), ("from-env".into(), TokenSource::Env));
    }

    #[test]
    fn blank_env_falls_through_to_store() {
        let store = MemoryStore::new();
        store.set("default", &Secret::new("from-store")).unwrap();
        let account = AccountConfig::named("default");
        let env = Env::default().with(var::MEJL_TOKEN, "  ");
        let found = resolve_login(&account, &env, &store).unwrap();
        assert_eq!(token(found), ("from-store".into(), TokenSource::Store));
        let other = AccountConfig::named("other");
        assert!(resolve_login(&other, &env, &store).unwrap().is_none());
    }

    #[test]
    fn an_oauth_login_wins_over_a_stored_token() {
        let store = MemoryStore::new();
        store.set("default", &Secret::new("from-store")).unwrap();
        store.set_oauth("default", &record()).unwrap();
        let account = AccountConfig::named("default");
        match resolve_login(&account, &Env::default(), &store).unwrap() {
            Some(Login::OAuth(r)) => assert_eq!(*r, record()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            login_source(&account, &Env::default(), &store).unwrap(),
            Some(LoginSource::OAuth(_))
        ));
        // The source check never runs token_command.
        let mut with_command = account.clone();
        with_command.token_command = Some("exit 9".into());
        assert!(matches!(
            login_source(&with_command, &Env::default(), &store).unwrap(),
            Some(LoginSource::Command)
        ));
        // With MEJL_TOKEN set, the store is not read at all.
        store.fail_with(Some("locked"));
        let env = Env::default().with(var::MEJL_TOKEN, "from-env");
        assert!(matches!(
            login_source(&account, &env, &store).unwrap(),
            Some(LoginSource::Env)
        ));
        assert!(login_source(&account, &Env::default(), &store).is_err());
    }

    #[test]
    fn memory_store_keeps_oauth_logins_apart_from_tokens() {
        let store = MemoryStore::new();
        assert!(store.get_oauth("a").unwrap().is_none());
        store.set_oauth("a", &record()).unwrap();
        store.set("a", &Secret::new("tok-1")).unwrap();
        assert_eq!(store.get_oauth("a").unwrap().unwrap(), record());
        assert!(store.delete("a").unwrap());
        assert!(store.get_oauth("a").unwrap().is_some());
        assert!(store.delete_oauth("a").unwrap());
        assert!(!store.delete_oauth("a").unwrap());
        assert_eq!(oauth_entry("work"), "work:oauth");
        // A record over the size limit is refused without echoing it.
        let mut big = record();
        big.tokens.access = Secret::new("x".repeat(MAX_RECORD_LEN));
        store.set_oauth("a", &big).unwrap();
        let e = store.get_oauth("a").unwrap_err().to_string();
        assert!(e.contains("longer than"), "{e}");
        assert!(!e.contains("xxxx"), "{e}");
    }

    #[test]
    fn malformed_env_token_is_an_error() {
        let store = MemoryStore::new();
        let account = AccountConfig::named("default");
        let env = Env::default().with(var::MEJL_TOKEN, "two words");
        let err = resolve_login(&account, &env, &store)
            .unwrap_err()
            .to_string();
        assert!(err.contains("MEJL_TOKEN"), "{err}");
        assert!(!err.contains("words"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn token_command_first_line_is_used() {
        let store = MemoryStore::new();
        store.set("default", &Secret::new("from-store")).unwrap();
        let mut account = AccountConfig::named("default");
        account.token_command = Some("printf '\\n  tok-from-cmd  \\nsecond\\n'".into());
        let found = resolve_login(&account, &Env::default(), &store).unwrap();
        assert_eq!(token(found), ("tok-from-cmd".into(), TokenSource::Command));
    }

    #[cfg(unix)]
    #[test]
    fn token_command_failures_are_reported() {
        let err = run_token_command("exit 4").unwrap_err().to_string();
        assert!(err.contains("exited"), "{err}");
        let err = run_token_command("true").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
        let err = run_token_command("printf '\\377\\376'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("UTF-8"), "{err}");
    }

    #[test]
    fn token_sources_have_words() {
        assert!(TokenSource::Env.describe().contains("MEJL_TOKEN"));
        assert_eq!(TokenSource::Store.describe(), "the keyring");
    }
}
