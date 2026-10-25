//! Engine errors and how each kind of failure is handled.

use std::time::Duration;

use mejl_jmap::{Error as JmapError, MethodErrorType, RequestErrorType, SetError, SetErrorType};

/// Why [`Engine::start`](crate::Engine::start) failed.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The store could not be read before it was handed to its thread.
    #[error("cannot read the store: {0}")]
    Store(#[from] mejl_store::Error),
    /// A thread or the async runtime could not be started.
    #[error("cannot start the sync engine: {0}")]
    Spawn(#[from] std::io::Error),
}

/// A failure inside the engine.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SyncError {
    #[error(transparent)]
    Jmap(#[from] JmapError),
    #[error("store: {0}")]
    Store(#[from] mejl_store::Error),
    /// The store thread is gone, which only happens at shutdown or after a
    /// crash of that thread.
    #[error("the store thread has stopped")]
    StoreGone,
    /// A store job panicked before it answered. The store thread lives on.
    #[error("a store job failed")]
    JobFailed,
    /// The response was requested before a flush result that was already
    /// recorded, so applying it could confirm a change too early.
    #[error("the server data is older than a recorded change")]
    Stale,
    /// The session names no mail account.
    #[error("the session has no mail account")]
    NoAccount,
    #[error("{0}")]
    Invalid(String),
}

/// What to do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// The credentials were rejected. Stop and ask for new ones.
    Auth,
    /// The network is down or the server cannot be reached.
    Network,
    /// The server asked us to slow down, maybe saying for how long.
    Backoff(Option<Duration>),
    /// A server-side problem that may go away.
    Transient,
    /// Retrying the same thing will not help.
    Permanent,
}

impl Class {
    /// The wait before retrying: the backoff step, or longer when the
    /// server asked for more.
    pub(crate) fn retry_delay(self, backoff: Duration) -> Duration {
        match self {
            Class::Backoff(Some(asked)) => backoff.max(asked),
            _ => backoff,
        }
    }

    /// Whether retrying the same request later may succeed.
    pub(crate) fn is_retryable(self) -> bool {
        !matches!(self, Class::Permanent)
    }
}

/// Whether an auth failure is an OAuth refresh refused with
/// `temporary: true`, such as a locked account.
pub(crate) fn is_temporary_auth(err: &JmapError) -> bool {
    err.oauth_error().is_some_and(|e| e.temporary)
}

impl SyncError {
    /// [`is_temporary_auth`] for an engine error.
    pub(crate) fn is_temporary_auth(&self) -> bool {
        match self {
            SyncError::Jmap(e) => is_temporary_auth(e),
            _ => false,
        }
    }
}

pub(crate) fn classify_jmap(err: &JmapError) -> Class {
    match err {
        // A 401 or 403, or an OAuth refresh that was refused or lost. A
        // refresh that only hit a passing server problem is transient.
        e if e.is_unauthorized() => Class::Auth,
        JmapError::InvalidOAuth(_) => Class::Auth,
        JmapError::OAuth(_) => Class::Transient,
        JmapError::Http { status: 403, .. } => Class::Auth,
        JmapError::Transport(_) => Class::Network,
        JmapError::RateLimited { retry_after } | JmapError::ServiceUnavailable { retry_after } => {
            Class::Backoff(*retry_after)
        }
        JmapError::Http { status, .. } if *status >= 500 => Class::Transient,
        JmapError::Request(problem) if problem.kind == RequestErrorType::Limit => {
            Class::Backoff(None)
        }
        JmapError::Request(problem) if problem.status.is_some_and(|s| s >= 500) => Class::Transient,
        JmapError::Method { error, .. } => classify_method(&error.kind),
        JmapError::NoSession | JmapError::MissingResponse { .. } => Class::Transient,
        _ => Class::Permanent,
    }
}

pub(crate) fn classify_method(kind: &MethodErrorType) -> Class {
    match kind {
        MethodErrorType::ServerUnavailable
        | MethodErrorType::ServerFail
        | MethodErrorType::ServerPartialFail => Class::Transient,
        MethodErrorType::Other(other) if other == "rateLimit" => Class::Backoff(None),
        _ => Class::Permanent,
    }
}

impl SyncError {
    pub(crate) fn class(&self) -> Class {
        match self {
            SyncError::Jmap(e) => classify_jmap(e),
            SyncError::Stale => Class::Transient,
            SyncError::StoreGone | SyncError::JobFailed => Class::Permanent,
            // A session without a mail account is most likely one fetched
            // mid-change; never drop queued work over it.
            SyncError::NoAccount => Class::Transient,
            SyncError::Store(_) | SyncError::Invalid(_) => Class::Permanent,
        }
    }
}

/// Whether a per-object `/set` failure may go away on retry.
pub(crate) fn set_error_is_transient(err: &SetError) -> bool {
    matches!(err.kind, SetErrorType::RateLimit)
}

/// A short, human-readable form of a set error for toasts.
pub(crate) fn describe_set_error(err: &SetError) -> String {
    match &err.description {
        Some(d) if !d.is_empty() => format!("{}: {d}", err.kind),
        _ => err.kind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_jmap::MethodError;
    use mejl_jmap::transport::TransportError;

    fn method(kind: MethodErrorType) -> JmapError {
        JmapError::Method {
            call_id: "c0".into(),
            name: "Email/set".into(),
            error: Box::new(MethodError::new(kind)),
        }
    }

    #[test]
    fn classes() {
        assert_eq!(
            classify_jmap(&JmapError::Unauthorized {
                status: 401,
                www_authenticate: None
            }),
            Class::Auth
        );
        let oauth = |kind: mejl_jmap::OAuthErrorKind| {
            JmapError::OAuth(Box::new(mejl_jmap::OAuthError::new(kind)))
        };
        assert_eq!(
            classify_jmap(&oauth(mejl_jmap::OAuthErrorKind::InvalidGrant)),
            Class::Auth
        );
        assert_eq!(
            classify_jmap(&oauth(mejl_jmap::OAuthErrorKind::TemporarilyUnavailable)),
            Class::Transient
        );
        assert_eq!(classify_jmap(&JmapError::RefreshLost), Class::Auth);
        assert_eq!(
            classify_jmap(&JmapError::InvalidOAuth("bad".into())),
            Class::Auth
        );
        assert_eq!(
            classify_jmap(&JmapError::Http {
                status: 403,
                body: String::new()
            }),
            Class::Auth
        );
        assert_eq!(
            classify_jmap(&JmapError::Transport(TransportError::Timeout)),
            Class::Network
        );
        assert_eq!(
            classify_jmap(&JmapError::RateLimited {
                retry_after: Some(Duration::from_secs(3))
            }),
            Class::Backoff(Some(Duration::from_secs(3)))
        );
        assert_eq!(
            classify_jmap(&JmapError::ServiceUnavailable { retry_after: None }),
            Class::Backoff(None)
        );
        assert_eq!(
            classify_jmap(&JmapError::Http {
                status: 502,
                body: String::new()
            }),
            Class::Transient
        );
        assert_eq!(
            classify_jmap(&JmapError::Http {
                status: 404,
                body: String::new()
            }),
            Class::Permanent
        );
        assert_eq!(
            classify_jmap(&method(MethodErrorType::ServerFail)),
            Class::Transient
        );
        assert_eq!(
            classify_jmap(&method(MethodErrorType::ServerUnavailable)),
            Class::Transient
        );
        assert_eq!(
            classify_jmap(&method(MethodErrorType::Other("rateLimit".into()))),
            Class::Backoff(None)
        );
        assert_eq!(
            classify_jmap(&method(MethodErrorType::InvalidArguments)),
            Class::Permanent
        );
        let limit = mejl_jmap::RequestError::from_body(
            br#"{"type":"urn:ietf:params:jmap:error:limit","limit":"maxCallsInRequest"}"#,
        )
        .unwrap();
        assert_eq!(
            classify_jmap(&JmapError::Request(Box::new(limit))),
            Class::Backoff(None)
        );
        assert!(Class::Transient.is_retryable());
        assert!(!Class::Permanent.is_retryable());
        assert_eq!(SyncError::Stale.class(), Class::Transient);
    }

    #[test]
    fn set_errors() {
        let mut e = SetError::new(SetErrorType::RateLimit);
        assert!(set_error_is_transient(&e));
        assert_eq!(describe_set_error(&e), "rateLimit");
        e.description = Some("slow down".into());
        assert_eq!(describe_set_error(&e), "rateLimit: slow down");
        assert!(!set_error_is_transient(&SetError::new(
            SetErrorType::NotFound
        )));
    }
}
