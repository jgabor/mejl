//! State every engine task shares, and the one path all JMAP calls take.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;
use mejl_jmap::method::{GetResponse, Method};
use mejl_jmap::{Client, CoreLimits, Id, Request, RequestBuilder, Response, Session, capability};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::apply::{self, Apply};
use crate::bodies::BodyMsg;
use crate::config::EngineConfig;
use crate::error::{Class, SyncError};
use crate::event::{PushState, SyncEvent};
use crate::status::{Events, StatusCell, Trigger};
use crate::store_thread::StoreHandle;

pub(crate) struct Shared {
    pub cfg: EngineConfig,
    pub client: Client,
    pub store: StoreHandle,
    pub events: Events,
    pub status: Arc<StatusCell>,
    pub cancel: CancellationToken,
    /// Run a sync pass.
    pub sync: Trigger,
    /// Refetch the Masked Email list.
    pub masked: Trigger,
    /// Look for outbox work.
    pub flush_wake: Arc<Notify>,
    /// Look for draft copies to upload or destroy.
    pub draft_wake: Arc<Notify>,
    /// Told when the engine comes back online.
    pub online_wake: Notify,
    /// Body fetch requests.
    pub bodies: mpsc::UnboundedSender<BodyMsg>,
    /// Mailboxes with a load-more in flight.
    pub loading_more: Mutex<HashSet<Id>>,
    /// Push failed often enough that the sync loop polls.
    pub polling: AtomicBool,
    session_stale: AtomicBool,
    online: AtomicBool,
    auth_failed: AtomicBool,
    auth_wake: Notify,
}

pub(crate) struct SharedParts {
    pub cfg: EngineConfig,
    pub client: Client,
    pub store: StoreHandle,
    pub events: Events,
    pub status: Arc<StatusCell>,
    pub flush_wake: Arc<Notify>,
    pub draft_wake: Arc<Notify>,
    pub bodies: mpsc::UnboundedSender<BodyMsg>,
}

impl Shared {
    pub(crate) fn new(parts: SharedParts) -> Self {
        Shared {
            cfg: parts.cfg,
            client: parts.client,
            store: parts.store,
            events: parts.events,
            status: parts.status,
            cancel: CancellationToken::new(),
            sync: Trigger::default(),
            masked: Trigger::default(),
            flush_wake: parts.flush_wake,
            draft_wake: parts.draft_wake,
            online_wake: Notify::new(),
            bodies: parts.bodies,
            loading_more: Mutex::new(HashSet::new()),
            polling: AtomicBool::new(false),
            // The cached session is only good for the first frame; the
            // live one is fetched before the first sync.
            session_stale: AtomicBool::new(true),
            online: AtomicBool::new(false),
            auth_failed: AtomicBool::new(false),
            auth_wake: Notify::new(),
        }
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.cfg.now()
    }

    pub(crate) fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst)
    }

    /// Records whether the server is reachable. Coming back online wakes
    /// the flusher, the draft uploader and the push loop.
    pub(crate) fn set_online(&self, online: bool) {
        let was = self.online.swap(online, Ordering::SeqCst);
        if was == online {
            return;
        }
        if online {
            tracing::info!("online");
            self.status.update(|s| {
                s.online = true;
                if s.push == PushState::Offline {
                    s.push = if self.cfg.push {
                        PushState::Reconnecting { attempt: 0 }
                    } else {
                        PushState::Disabled
                    };
                }
            });
            self.flush_wake.notify_one();
            self.draft_wake.notify_one();
            self.online_wake.notify_waiters();
        } else {
            tracing::info!("offline");
            self.status.update(|s| {
                s.online = false;
                if s.push != PushState::Disabled {
                    s.push = PushState::Offline;
                }
            });
            // The sync loop probes with backoff until the server answers,
            // whichever task noticed the outage.
            self.sync.fire();
        }
    }

    pub(crate) fn is_auth_failed(&self) -> bool {
        self.auth_failed.load(Ordering::SeqCst)
    }

    /// Stops network work until new credentials arrive. Emits the event
    /// once per failure, not once per task that hits it.
    pub(crate) fn auth_failed(&self, error: &str) {
        if !self.auth_failed.swap(true, Ordering::SeqCst) {
            tracing::warn!("the server rejected the credentials");
            self.status.update(|s| {
                s.auth_failed = true;
                s.last_error = Some(error.to_owned());
            });
            self.events.emit(SyncEvent::AuthFailed {
                error: error.to_owned(),
            });
        }
    }

    /// New credentials: resume everything.
    pub(crate) fn resume_auth(&self) {
        self.auth_failed.store(false, Ordering::SeqCst);
        self.session_stale.store(true, Ordering::SeqCst);
        self.status.update(|s| {
            s.auth_failed = false;
            s.last_error = None;
        });
        self.auth_wake.notify_waiters();
        self.sync.fire();
        self.masked.fire();
        self.flush_wake.notify_one();
        self.draft_wake.notify_one();
        self.online_wake.notify_waiters();
    }

    /// Waits while the credentials are rejected. Returns false on shutdown.
    pub(crate) async fn wait_auth(&self) -> bool {
        while self.is_auth_failed() {
            let woken = self.auth_wake.notified();
            if !self.is_auth_failed() {
                break;
            }
            tokio::select! {
                _ = self.cancel.cancelled() => return false,
                _ = woken => {}
            }
        }
        !self.cancel.is_cancelled()
    }

    /// Sleeps, or returns false at once on shutdown.
    pub(crate) async fn sleep(&self, d: Duration) -> bool {
        if d.is_zero() {
            return !self.cancel.is_cancelled();
        }
        tokio::select! {
            _ = self.cancel.cancelled() => false,
            _ = tokio::time::sleep(d) => true,
        }
    }

    /// Sleeps, but wakes early when the engine comes back online. Returns
    /// false on shutdown.
    pub(crate) async fn sleep_or_online(&self, d: Duration) -> bool {
        let woken = self.online_wake.notified();
        tokio::select! {
            _ = self.cancel.cancelled() => false,
            _ = tokio::time::sleep(d) => true,
            _ = woken => true,
        }
    }

    /// Updates online state, auth state and the last error after a
    /// failure, and says how to handle it.
    pub(crate) fn note(&self, err: &SyncError) -> Class {
        let class = err.class();
        match class {
            Class::Auth => self.auth_failed(&err.to_string()),
            Class::Network => self.set_online(false),
            _ => {}
        }
        if class != Class::Auth {
            let text = err.to_string();
            self.status.update(|s| s.last_error = Some(text));
        }
        class
    }

    /// Records a request result: success means the server is reachable.
    fn observe<T>(&self, result: &Result<T, mejl_jmap::Error>) {
        match result {
            Ok(_) => self.set_online(true),
            Err(e) => {
                let class = crate::error::classify_jmap(e);
                match class {
                    Class::Network => self.set_online(false),
                    Class::Auth => self.auth_failed(&e.to_string()),
                    // Any HTTP answer means the network works.
                    _ => self.set_online(true),
                }
            }
        }
    }

    /// The live session, fetched when missing or when a response said it
    /// changed. A fresh session is stored in the cache.
    pub(crate) async fn session(&self) -> Result<Arc<Session>, SyncError> {
        if let Some(session) = self.client.session()
            && !self.session_stale.load(Ordering::SeqCst)
        {
            return Ok(session);
        }
        let fetched = self.client.fetch_session().await;
        self.observe(&fetched);
        let session = fetched?;
        self.session_stale.store(false, Ordering::SeqCst);
        let mut a = Apply::new(self.store.stamp());
        a.session = Some((*session).clone());
        self.store.call(move |ctx| apply::run(ctx, a)).await??;
        Ok(session)
    }

    /// The session if one is loaded, without fetching.
    pub(crate) fn loaded_session(&self) -> Option<Arc<Session>> {
        self.client.session()
    }

    /// The mail account id.
    pub(crate) fn account(&self) -> Result<Id, SyncError> {
        let session = self.client.session().ok_or(SyncError::NoAccount)?;
        session
            .primary_account(capability::MAIL)
            .cloned()
            .or_else(|| session.accounts.keys().next().cloned())
            .ok_or(SyncError::NoAccount)
    }

    pub(crate) fn limits(&self) -> CoreLimits {
        self.client
            .session()
            .map(|s| s.core_limits())
            .unwrap_or_default()
    }

    /// Whether the session and account offer a capability.
    pub(crate) fn has_capability(&self, uri: &str) -> bool {
        let Some(session) = self.client.session() else {
            return false;
        };
        if !session.has_capability(uri) {
            return false;
        }
        match self.account() {
            // Some servers list account capabilities, some do not; only a
            // list that leaves it out counts as a no.
            Ok(account) => session.account(account.as_str()).is_none_or(|a| {
                a.account_capabilities.is_empty() || a.account_capabilities.contains_key(uri)
            }),
            Err(_) => false,
        }
    }

    /// Sends one request, loading the session first if needed.
    pub(crate) async fn call(&self, request: Request) -> Result<Response, SyncError> {
        if self.client.session().is_none() {
            self.session().await?;
        }
        let result = self.client.call(&request).await;
        self.observe(&result);
        let response = result?;
        if response.session_changed {
            tracing::debug!("the session changed; fetching it again");
            self.session_stale.store(true, Ordering::SeqCst);
            self.sync.fire();
        }
        Ok(response)
    }

    /// Builds and sends a request.
    pub(crate) async fn send(&self, builder: RequestBuilder) -> Result<Response, SyncError> {
        let request = builder.build()?;
        self.call(request).await
    }

    /// `/get` of many ids, split at `maxObjectsInGet` and packed up to
    /// `maxCallsInRequest` calls per request.
    pub(crate) async fn get_chunked<M, T>(
        &self,
        ids: &[Id],
        args: impl Fn(Vec<Id>) -> M::Args,
    ) -> Result<Vec<GetResponse<T>>, SyncError>
    where
        M: Method<Response = GetResponse<T>>,
    {
        let limits = self.limits();
        let per_get = usize::try_from(limits.max_objects_in_get)
            .unwrap_or(usize::MAX)
            .max(1);
        let per_request = usize::try_from(limits.max_calls_in_request)
            .unwrap_or(usize::MAX)
            .max(1);
        let mut out = Vec::new();
        if ids.is_empty() {
            return Ok(out);
        }
        for request_ids in ids.chunks(per_get.saturating_mul(per_request)) {
            let mut req = RequestBuilder::new();
            let handles: Vec<_> = request_ids
                .chunks(per_get)
                .map(|chunk| req.call::<M>(args(chunk.to_vec())))
                .collect();
            let resp = self.send(req).await?;
            for h in &handles {
                out.push(resp.get(h)?);
            }
        }
        Ok(out)
    }
}
