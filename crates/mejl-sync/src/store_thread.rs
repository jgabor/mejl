//! The store thread: it owns the [`Store`] and runs every write.
//!
//! Network tasks and commands send it jobs over a channel. A job is a
//! closure that runs with the store; async callers get its result back over
//! a oneshot channel. Jobs run one at a time in the order they were sent, so
//! a mutation queued before an undo is always applied first.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use jiff::Timestamp;
use mejl_jmap::Id;
use mejl_store::{Reader, Store};
use tokio::sync::{Notify, oneshot};

use crate::config::Clock;
use crate::error::SyncError;
use crate::event::DataChanges;
use crate::status::{Events, StatusCell};

/// Above this many emails a data change is reported as "everything"
/// instead of by id, so bulk actions and full resyncs stay cheap.
pub(crate) const SCOPE_LIMIT: usize = 1000;

/// A unit of work for the store thread.
pub(crate) type Job = Box<dyn FnOnce(&mut StoreCtx) + Send + 'static>;

enum Msg {
    Job(Job),
    Stop,
}

/// Which kinds of server objects a response carries, for the staleness
/// check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Kinds {
    pub email: bool,
    pub mailbox: bool,
    pub masked: bool,
    pub contact: bool,
}

/// Flush generations: each counts recorded flush results that changed
/// server objects of one kind. A request remembers them when it is sent;
/// if one moved by the time its response is applied, the response may
/// predate that change and is dropped.
#[derive(Debug, Default)]
pub(crate) struct Gens {
    email: AtomicU64,
    mailbox: AtomicU64,
    masked: AtomicU64,
    contact: AtomicU64,
}

/// The flush generations when a request was sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Stamp {
    email: u64,
    mailbox: u64,
    masked: u64,
    contact: u64,
}

impl Gens {
    pub(crate) fn stamp(&self) -> Stamp {
        Stamp {
            email: self.email.load(Ordering::SeqCst),
            mailbox: self.mailbox.load(Ordering::SeqCst),
            masked: self.masked.load(Ordering::SeqCst),
            contact: self.contact.load(Ordering::SeqCst),
        }
    }

    /// Records that a flush result for these kinds was passed to the store.
    pub(crate) fn bump(&self, kinds: Kinds) {
        for (on, counter) in [
            (kinds.email, &self.email),
            (kinds.mailbox, &self.mailbox),
            (kinds.masked, &self.masked),
            (kinds.contact, &self.contact),
        ] {
            if on {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    pub(crate) fn is_stale(&self, stamp: Stamp, kinds: Kinds) -> bool {
        let now = self.stamp();
        (kinds.email && now.email != stamp.email)
            || (kinds.mailbox && now.mailbox != stamp.mailbox)
            || (kinds.masked && now.masked != stamp.masked)
            || (kinds.contact && now.contact != stamp.contact)
    }
}

/// What a store job can use.
pub(crate) struct StoreCtx {
    pub store: Store,
    reader: Option<Reader>,
    pub gens: Arc<Gens>,
    pub events: Events,
    pub status: Arc<StatusCell>,
    pub flush_wake: Arc<Notify>,
    pub draft_wake: Arc<Notify>,
    clock: Arc<dyn Clock>,
}

impl StoreCtx {
    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// A read connection for queries the writer API does not offer.
    pub(crate) fn reader(&mut self) -> Result<&Reader, SyncError> {
        if self.reader.is_none() {
            self.reader = Some(self.store.reader()?);
        }
        self.reader
            .as_ref()
            .ok_or_else(|| SyncError::Invalid("no reader".into()))
    }

    /// Copies the outbox counts into the status line.
    pub(crate) fn refresh_outbox_status(&mut self) {
        let now = self.now();
        let summary = self.reader().and_then(|r| Ok(r.outbox_summary(now)?));
        match summary {
            Ok(s) => self.status.update(|st| {
                st.pending = usize::try_from(s.pending).unwrap_or(usize::MAX);
                st.held_sends = usize::try_from(s.held).unwrap_or(usize::MAX);
                st.failed = usize::try_from(s.failed).unwrap_or(usize::MAX);
            }),
            Err(e) => tracing::warn!(error = %e, "cannot read outbox counts"),
        }
    }

    /// The data change scope for emails whose membership may have moved:
    /// the emails, their threads and every mailbox they were or are in.
    /// Past [`SCOPE_LIMIT`] ids it just says "everything", which is cheaper
    /// for the caller to act on than a huge id set.
    pub(crate) fn email_scope(
        &self,
        ids: &[Id],
        before: &std::collections::BTreeMap<Id, Vec<Id>>,
    ) -> DataChanges {
        let mut changes = DataChanges::default();
        if ids.is_empty() {
            return changes;
        }
        changes.mailboxes = true;
        if ids.len() > SCOPE_LIMIT {
            changes.everything = true;
            return changes;
        }
        changes.emails.extend(ids.iter().cloned());
        for mailboxes in before.values() {
            changes.mailbox_lists.extend(mailboxes.iter().cloned());
        }
        match self.store.email_mailboxes(ids) {
            Ok(after) => {
                for mailboxes in after.values() {
                    changes.mailbox_lists.extend(mailboxes.iter().cloned());
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot read mailbox membership");
                changes.everything = true;
            }
        }
        changes
    }

    /// The current membership of `ids`, for [`StoreCtx::email_scope`].
    pub(crate) fn memberships(&self, ids: &[Id]) -> std::collections::BTreeMap<Id, Vec<Id>> {
        if ids.len() > SCOPE_LIMIT {
            return std::collections::BTreeMap::new();
        }
        self.store.email_mailboxes(ids).unwrap_or_default()
    }
}

#[cfg(test)]
impl StoreCtx {
    /// A context without a thread, for tests that drive jobs by hand.
    pub(crate) fn for_test(store: Store) -> Self {
        let sink: crate::status::Sink = Arc::new(|_| {});
        let (events, _rx) = Events::new(sink.clone());
        StoreCtx {
            store,
            reader: None,
            gens: Arc::new(Gens::default()),
            events,
            status: Arc::new(StatusCell::new(sink, crate::event::SyncStatus::default())),
            flush_wake: Arc::new(Notify::new()),
            draft_wake: Arc::new(Notify::new()),
            clock: Arc::new(crate::config::SystemClock),
        }
    }
}

/// Sends jobs to the store thread. Cheap to clone; never blocks.
#[derive(Clone)]
pub(crate) struct StoreHandle {
    tx: mpsc::Sender<Msg>,
    gens: Arc<Gens>,
}

impl StoreHandle {
    /// Runs `f` on the store thread without waiting. Returns false when the
    /// thread is gone.
    pub(crate) fn post(&self, f: impl FnOnce(&mut StoreCtx) + Send + 'static) -> bool {
        self.tx.send(Msg::Job(Box::new(f))).is_ok()
    }

    /// Runs `f` on the store thread and waits for its result.
    pub(crate) async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut StoreCtx) -> R + Send + 'static,
    ) -> Result<R, SyncError> {
        let (tx, rx) = oneshot::channel();
        let job = move |ctx: &mut StoreCtx| {
            let _ = tx.send(f(ctx));
        };
        if !self.post(job) {
            return Err(SyncError::StoreGone);
        }
        // The thread drops the reply only when the job panicked; it keeps
        // running, so this is not the same as the thread being gone.
        rx.await.map_err(|_| SyncError::JobFailed)
    }

    /// The flush generations now, to send with a data request.
    pub(crate) fn stamp(&self) -> Stamp {
        self.gens.stamp()
    }

    /// Asks the thread to stop after the jobs already queued.
    pub(crate) fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

/// What the store thread needs besides the store.
pub(crate) struct StoreParts {
    pub events: Events,
    pub status: Arc<StatusCell>,
    pub flush_wake: Arc<Notify>,
    pub draft_wake: Arc<Notify>,
    pub clock: Arc<dyn Clock>,
}

/// Starts the store thread. The returned receiver gets a message when the
/// thread has finished.
pub(crate) fn spawn(
    store: Store,
    parts: StoreParts,
) -> std::io::Result<(StoreHandle, JoinHandle<()>, mpsc::Receiver<()>)> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (done_tx, done_rx) = mpsc::channel();
    let gens = Arc::new(Gens::default());
    let mut ctx = StoreCtx {
        store,
        reader: None,
        gens: gens.clone(),
        events: parts.events,
        status: parts.status,
        flush_wake: parts.flush_wake,
        draft_wake: parts.draft_wake,
        clock: parts.clock,
    };
    let thread = std::thread::Builder::new()
        .name("mejl-store".into())
        .spawn(move || {
            run(&rx, &mut ctx);
            // Close the store before reporting, so a caller that waits for
            // this can reopen the directory at once.
            drop(ctx);
            let _ = done_tx.send(());
        })?;
    Ok((StoreHandle { tx, gens }, thread, done_rx))
}

fn run(rx: &mpsc::Receiver<Msg>, ctx: &mut StoreCtx) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Job(job) => {
                // A panicking job must not take the store down with it; its
                // transaction rolls back when dropped.
                if catch_unwind(AssertUnwindSafe(|| job(ctx))).is_err() {
                    tracing::error!("a store job panicked");
                }
            }
            Msg::Stop => break,
        }
    }
    tracing::debug!("store thread stopping");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_go_stale_per_kind() {
        let gens = Gens::default();
        let stamp = gens.stamp();
        let email = Kinds {
            email: true,
            ..Default::default()
        };
        let masked = Kinds {
            masked: true,
            ..Default::default()
        };
        assert!(!gens.is_stale(stamp, email));
        gens.bump(masked);
        assert!(!gens.is_stale(stamp, email));
        assert!(gens.is_stale(stamp, masked));
        gens.bump(email);
        assert!(gens.is_stale(stamp, email));
        assert!(!gens.is_stale(gens.stamp(), email));
        assert!(!gens.is_stale(stamp, Kinds::default()));
    }
}
