//! The EventSource push loop: connect, read, reconnect with backoff, and
//! fall back to polling when push keeps failing.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt;
use mejl_jmap::push::PushEvent;
use mejl_jmap::{PushOptions, PushStream};
use mejl_store::type_name;
use tokio::time::Instant;

use crate::backoff::delay_for;
use crate::error::{Class, SyncError};
use crate::event::PushState;
use crate::shared::Shared;

/// The data types mejl syncs, for the `types` parameter.
const PUSH_TYPES: &[&str] = &[
    type_name::MAILBOX,
    type_name::EMAIL,
    type_name::THREAD,
    type_name::IDENTITY,
    type_name::CONTACT_CARD,
    type_name::MASKED_EMAIL,
];

/// How a connection ended.
struct Ended {
    /// At least one event arrived, so the server and the path work.
    healthy: bool,
    /// The server's `retry:` hint.
    retry: Option<Duration>,
    last_event_id: Option<String>,
}

pub(crate) async fn push_loop(sh: Arc<Shared>) {
    let mut failures: u32 = 0;
    let mut last_event_id: Option<String> = None;
    loop {
        if sh.cancel.is_cancelled() {
            return;
        }
        if sh.is_auth_failed() && !sh.wait_auth().await {
            return;
        }
        // Push needs a session; the sync loop fetches it.
        if sh.loaded_session().is_none()
            && let Err(e) = sh.session().await
        {
            handle_error(&sh, &e, &mut failures);
            if !wait_before_retry(&sh, failures, None).await {
                return;
            }
            continue;
        }
        sh.status.update(|s| {
            if s.online || failures == 0 {
                s.push = if failures == 0 {
                    PushState::Connecting
                } else if sh.polling.load(Ordering::SeqCst) {
                    PushState::Polling
                } else {
                    PushState::Reconnecting { attempt: failures }
                };
            }
        });
        let options = PushOptions {
            types: Some(PUSH_TYPES.iter().map(|t| (*t).to_owned()).collect()),
            close_after_state: false,
            ping: sh.cfg.push_ping_seconds,
            last_event_id: last_event_id.clone(),
            ..PushOptions::default()
        };
        let retry_hint;
        let connected = tokio::select! {
            _ = sh.cancel.cancelled() => return,
            r = sh.client.push(&options) => r,
        };
        match connected {
            Ok(stream) => {
                sh.set_online(true);
                tracing::debug!("push connected");
                sh.status.update(|s| s.push = PushState::Connected);
                sh.polling.store(false, Ordering::SeqCst);
                // Events may have been missed while disconnected.
                sh.sync.fire();
                let Some(ended) = read(&sh, stream).await else {
                    return;
                };
                if ended.last_event_id.is_some() {
                    last_event_id = ended.last_event_id;
                }
                retry_hint = ended.retry;
                failures = if ended.healthy { 1 } else { failures + 1 };
                tracing::debug!(failures, "push connection ended");
            }
            Err(e) => {
                let e = SyncError::from(e);
                retry_hint = match e.class() {
                    Class::Backoff(d) => d,
                    _ => None,
                };
                handle_error(&sh, &e, &mut failures);
            }
        }
        if failures >= sh.cfg.push_failures_before_poll.max(1)
            && !sh.polling.swap(true, Ordering::SeqCst)
        {
            tracing::info!("push keeps failing; polling as well");
            sh.status.update(|s| {
                if s.push != PushState::Offline {
                    s.push = PushState::Polling;
                }
            });
            // The sync loop reads the flag when it next waits.
            sh.sync.fire();
        } else if failures > 0 {
            sh.status.update(|s| {
                if s.push != PushState::Offline && s.push != PushState::Polling {
                    s.push = PushState::Reconnecting { attempt: failures };
                }
            });
        }
        if !wait_before_retry(&sh, failures, retry_hint).await {
            return;
        }
    }
}

fn handle_error(sh: &Shared, e: &SyncError, failures: &mut u32) {
    let class = sh.note(e);
    tracing::debug!(error = %e, ?class, "push connect failed");
    *failures = failures.saturating_add(1);
}

/// Waits the backoff for `failures`, at least the server's hint. Wakes
/// early when the engine comes back online. Returns false on shutdown.
async fn wait_before_retry(sh: &Shared, failures: u32, hint: Option<Duration>) -> bool {
    let mut delay = delay_for(
        sh.cfg.backoff_min,
        sh.cfg.backoff_max,
        failures.saturating_sub(1),
    );
    if let Some(hint) = hint {
        delay = delay.max(hint);
    }
    if failures == 0 {
        delay = Duration::ZERO;
    }
    if sh.is_online() {
        sh.sleep(delay).await
    } else {
        sh.sleep_or_online(delay).await
    }
}

/// Reads events until the stream ends, fails or goes quiet for longer than
/// the idle timeout. Returns `None` on shutdown.
async fn read(sh: &Shared, mut stream: PushStream) -> Option<Ended> {
    let idle = sh.cfg.push_idle_timeout();
    let account = sh.account().ok();
    let mut healthy = false;
    let opened = Instant::now();
    loop {
        let next = tokio::select! {
            _ = sh.cancel.cancelled() => return None,
            n = tokio::time::timeout(idle, stream.next()) => n,
        };
        match next {
            Err(_) => {
                tracing::info!(?idle, "push went quiet; reconnecting");
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                tracing::debug!(error = %e, "push stream failed");
                break;
            }
            Ok(Some(Ok(PushEvent::Ping { .. }))) => healthy = true,
            Ok(Some(Ok(PushEvent::StateChange { changed }))) => {
                healthy = true;
                let mine = account.as_ref().and_then(|a| changed.get(a));
                if mine.is_some_and(|types| types.contains_key(type_name::MASKED_EMAIL)) {
                    sh.masked.fire();
                }
                // Even an empty or unknown change is worth one sync.
                sh.sync.fire();
            }
        }
    }
    // A connection that lasted longer than the idle timeout was fine too.
    healthy |= opened.elapsed() >= idle;
    Some(Ended {
        healthy,
        retry: stream.retry(),
        last_event_id: stream.last_event_id().map(str::to_owned),
    })
}
