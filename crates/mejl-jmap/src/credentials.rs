//! Where a [`Client`](crate::Client) gets its bearer token: a fixed token,
//! or an OAuth login that refreshes itself.
//!
//! The OAuth rules, because Fastmail rotates the refresh token on every use
//! and revokes the whole login when an old one comes back:
//!
//! - A token with less than a minute left is refreshed before it is used.
//! - A 401, and only a 401, triggers one refresh and one resend.
//! - Refreshes are single-flight. A caller whose rejected token is no
//!   longer current just takes the current one.
//! - Before a refresh the [`TokenStore`] is read. A newer login saved by
//!   another mejl process is adopted, and used without a refresh while its
//!   access token is still good.
//! - A refresh token that worked is never sent again.
//! - The new tokens are saved before the new access token is used. If the
//!   save fails, they stay in memory and the next refresh saves again.
//! - A refresh that gets no response is retried once with the same refresh
//!   token. After the second one the login counts as failed. A connection
//!   that never opened is not counted: the request was not sent.
//! - Once a refresh is refused or lost, every later caller gets the same
//!   error without a request, until the store offers a newer login or the
//!   app installs new credentials.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use futures::lock::Mutex;
use jiff::{SignedDuration, Timestamp};
use sha2::{Digest, Sha256};

use crate::error::{Error, OAuthError};
use crate::oauth::{self, TokenReply, TokenSet};
use crate::secret::Secret;
use crate::transport::{BoxFuture, Transport, TransportError};

/// How long before expiry an access token is refreshed.
pub const REFRESH_MARGIN: SignedDuration = SignedDuration::from_secs(60);

/// Where an OAuth login is kept between runs, such as the OS keyring.
///
/// Both methods are async and runtime-free; an implementation that blocks
/// should move the work off the async thread itself.
pub trait TokenStore: Send + Sync + 'static {
    /// The login as last saved, by this process or another. `None` when
    /// nothing is saved or it cannot be read.
    fn load(&self) -> BoxFuture<'_, Option<TokenSet>>;

    /// Save a refreshed login. It is called before the new access token is
    /// used, so this is also the place to register new secrets with a log
    /// redactor.
    fn save<'a>(&'a self, tokens: &'a TokenSet) -> BoxFuture<'a, Result<(), String>>;
}

/// What a [`Client`](crate::Client) needs for an OAuth login.
#[derive(Clone)]
pub struct OAuthCredentials {
    /// The tokens to start from, usually just loaded from the store.
    pub tokens: TokenSet,
    /// The token endpoint, from discovery. Fastmail's is `/oauth/refresh`.
    pub token_endpoint: String,
    pub client_id: String,
    pub store: Arc<dyn TokenStore>,
}

impl fmt::Debug for OAuthCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredentials")
            .field("tokens", &self.tokens)
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) enum Credentials {
    Static(Secret),
    OAuth(Arc<OAuthProvider>),
}

pub(crate) struct OAuthProvider {
    transport: Arc<dyn Transport>,
    token_endpoint: String,
    client_id: String,
    store: Arc<dyn TokenStore>,
    current: RwLock<TokenSet>,
    refresh: Mutex<RefreshState>,
}

#[derive(Default)]
struct RefreshState {
    /// SHA-256 of every refresh token that must not be sent again.
    spent: HashSet<[u8; 32]>,
    /// Refreshes of the current refresh token that got no response.
    lost: u8,
    /// Why the current refresh token cannot be used, once it cannot.
    failed: Option<Failure>,
    /// A save failure was already logged.
    save_warned: bool,
}

enum Failure {
    Refused(OAuthError),
    Lost,
    Invalid(String),
}

impl Failure {
    fn error(&self) -> Error {
        match self {
            Failure::Refused(e) => Error::OAuth(Box::new(e.clone())),
            Failure::Lost => Error::RefreshLost,
            Failure::Invalid(why) => Error::InvalidOAuth(why.clone()),
        }
    }
}

fn digest(secret: &Secret) -> [u8; 32] {
    Sha256::digest(secret.expose().as_bytes()).into()
}

fn is_fresh(tokens: &TokenSet) -> bool {
    !tokens.expires_within(REFRESH_MARGIN, Timestamp::now())
}

/// A replacement for a rejected or expiring token is taken while it has
/// not expired, even inside the margin, so callers waiting on one refresh
/// do not each start another when the server's lifetime is short.
fn is_usable(tokens: &TokenSet) -> bool {
    tokens.expires_at > Timestamp::now()
}

impl RefreshState {
    fn is_spent(&self, refresh: &Secret) -> bool {
        self.spent.contains(&digest(refresh))
    }

    fn spend(&mut self, refresh: &Secret) {
        self.spent.insert(digest(refresh));
    }
}

impl OAuthProvider {
    pub(crate) fn new(transport: Arc<dyn Transport>, creds: OAuthCredentials) -> Self {
        Self {
            transport,
            token_endpoint: creds.token_endpoint,
            client_id: creds.client_id,
            store: creds.store,
            current: RwLock::new(creds.tokens),
            refresh: Mutex::new(RefreshState::default()),
        }
    }

    pub(crate) fn current(&self) -> TokenSet {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_current(&self, tokens: TokenSet) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = tokens;
    }

    /// The access token to send now, refreshed first when it is about to
    /// expire.
    pub(crate) async fn access_token(&self) -> Result<Secret, Error> {
        let current = self.current();
        if is_fresh(&current) {
            return Ok(current.access);
        }
        self.renew(&current.access).await
    }

    /// A working access token to replace `seen`, which was rejected with a
    /// 401 or is about to expire.
    pub(crate) async fn renew(&self, seen: &Secret) -> Result<Secret, Error> {
        let mut state = self.refresh.lock().await;
        let mut current = self.current();
        if current.access != *seen && is_usable(&current) {
            return Ok(current.access);
        }

        // Another mejl process may have refreshed already, which used up
        // the refresh token this one holds.
        if let Some(stored) = self.store.load().await
            && stored.refresh != current.refresh
            && !state.is_spent(&stored.refresh)
        {
            tracing::debug!("adopting an OAuth login saved by another process");
            state.spend(&current.refresh);
            state.lost = 0;
            state.failed = None;
            self.set_current(stored.clone());
            current = stored;
            if current.access != *seen && is_usable(&current) {
                return Ok(current.access);
            }
        }

        if let Some(failure) = &state.failed {
            return Err(failure.error());
        }
        if state.is_spent(&current.refresh) {
            let failure = Failure::Invalid("the refresh token was already used".into());
            let err = failure.error();
            state.failed = Some(failure);
            return Err(err);
        }

        loop {
            let reply = oauth::refresh_reply(
                &*self.transport,
                &self.token_endpoint,
                &self.client_id,
                &current,
            )
            .await;
            let lost = match reply {
                Ok(TokenReply::Tokens(new)) => {
                    return Ok(self.install(&mut state, &current, new).await);
                }
                Ok(TokenReply::Unusable(why)) => why,
                // The connection never opened, so the token was not sent.
                Err(e @ Error::Transport(TransportError::Connect(_))) => return Err(e),
                Err(Error::Transport(e)) => e.to_string(),
                Err(e) if e.is_transient() => return Err(e),
                Err(e) => {
                    state.failed = Some(match &e {
                        Error::OAuth(refused) => Failure::Refused((**refused).clone()),
                        other => Failure::Invalid(other.to_string()),
                    });
                    return Err(e);
                }
            };
            state.lost += 1;
            if state.lost >= 2 {
                tracing::warn!(reason = %lost, "OAuth refresh got no response twice; the login needs renewing");
                state.failed = Some(Failure::Lost);
                return Err(Error::RefreshLost);
            }
            tracing::warn!(reason = %lost, "OAuth refresh got no response; retrying once with the same refresh token");
        }
    }

    /// Take a refresh result: mark the old refresh token spent, save, then
    /// make the new tokens current.
    async fn install(&self, state: &mut RefreshState, old: &TokenSet, new: TokenSet) -> Secret {
        if new.refresh != old.refresh {
            state.spend(&old.refresh);
        }
        state.lost = 0;
        match self.store.save(&new).await {
            Ok(()) => state.save_warned = false,
            Err(why) => {
                if !state.save_warned {
                    tracing::warn!(
                        error = %why,
                        "cannot save the refreshed OAuth login; it is kept in memory and saved again at the next refresh, but will not survive a restart"
                    );
                    state.save_warned = true;
                }
            }
        }
        let access = new.access.clone();
        self.set_current(new);
        access
    }
}
