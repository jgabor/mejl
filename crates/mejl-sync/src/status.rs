//! Getting events to the caller: the sink, the status snapshot and the
//! data change coalescer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::event::{DataChanges, SyncEvent, SyncStatus};

/// The caller's event sink.
pub(crate) type Sink = Arc<dyn Fn(SyncEvent) + Send + Sync>;

/// Sends events, and data changes through the coalescer. Cheap to clone and
/// usable from any thread.
#[derive(Clone)]
pub(crate) struct Events {
    sink: Sink,
    data: mpsc::UnboundedSender<DataChanges>,
}

impl Events {
    pub(crate) fn new(sink: Sink) -> (Self, mpsc::UnboundedReceiver<DataChanges>) {
        let (data, rx) = mpsc::unbounded_channel();
        (Events { sink, data }, rx)
    }

    pub(crate) fn emit(&self, event: SyncEvent) {
        (self.sink)(event);
    }

    /// Queues a data change for the next coalesced
    /// [`SyncEvent::DataChanged`].
    pub(crate) fn data(&self, changes: DataChanges) {
        if !changes.is_empty() {
            // The receiver only goes away at shutdown, when nobody is
            // listening any more.
            let _ = self.data.send(changes);
        }
    }

    pub(crate) fn sink(&self) -> &Sink {
        &self.sink
    }
}

/// The status snapshot. Every change that alters it emits one
/// [`SyncEvent::Status`].
pub(crate) struct StatusCell {
    status: Mutex<SyncStatus>,
    sink: Sink,
}

impl StatusCell {
    pub(crate) fn new(sink: Sink, initial: SyncStatus) -> Self {
        StatusCell {
            status: Mutex::new(initial),
            sink,
        }
    }

    /// Changes the status and emits it if anything changed. The event is
    /// sent under the lock so snapshots never arrive out of order.
    pub(crate) fn update(&self, f: impl FnOnce(&mut SyncStatus)) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        let before = status.clone();
        f(&mut status);
        if *status != before {
            (self.sink)(SyncEvent::Status(status.clone()));
        }
    }

    #[cfg(test)]
    pub(crate) fn get(&self) -> SyncStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Merges data changes and sends at most one event per `interval`.
pub(crate) async fn coalesce(
    mut rx: mpsc::UnboundedReceiver<DataChanges>,
    sink: Sink,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut last_emit: Option<Instant> = None;
    loop {
        let first = tokio::select! {
            _ = cancel.cancelled() => return,
            next = rx.recv() => match next {
                Some(c) => c,
                None => return,
            },
        };
        let mut pending = first;
        let deadline = match last_emit {
            Some(at) => at + interval,
            None => Instant::now(),
        };
        loop {
            // Take everything already queued before deciding to wait.
            while let Ok(more) = rx.try_recv() {
                pending.merge(more);
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep_until(deadline) => {}
                more = rx.recv() => match more {
                    Some(c) => pending.merge(c),
                    None => break,
                },
            }
        }
        last_emit = Some(Instant::now());
        sink(SyncEvent::DataChanged(pending));
    }
}

/// A wake-up with a dirty flag, so a burst of triggers runs the work once
/// and a trigger during the work runs it once more.
#[derive(Default)]
pub(crate) struct Trigger {
    dirty: AtomicBool,
    notify: Notify,
}

impl Trigger {
    pub(crate) fn fire(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Waits until fired. Returns at once if fired since the last
    /// [`Trigger::take`].
    pub(crate) async fn wait(&self) {
        while !self.dirty.load(Ordering::SeqCst) {
            self.notify.notified().await;
        }
    }

    /// Clears the flag before the work starts.
    pub(crate) fn take(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn is_set(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_jmap::Id;

    #[test]
    fn status_emits_only_on_change() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let sink: Sink = Arc::new(move |e| s2.lock().unwrap().push(e));
        let cell = StatusCell::new(sink, SyncStatus::default());
        cell.update(|s| s.online = true);
        cell.update(|s| s.online = true);
        cell.update(|s| s.pending = 3);
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], SyncEvent::Status(s) if s.pending == 3 && s.online));
        assert_eq!(cell.get().pending, 3);
    }

    #[test]
    fn coalescer_merges_bursts() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let s2 = seen.clone();
            let sink: Sink = Arc::new(move |e| s2.lock().unwrap().push(e));
            let (events, rx) = Events::new(sink.clone());
            let cancel = CancellationToken::new();
            let task = tokio::spawn(coalesce(
                rx,
                sink,
                Duration::from_millis(30),
                cancel.clone(),
            ));
            // The first change goes out at once; the burst after it is
            // merged into one more event.
            events.data(DataChanges {
                mailboxes: true,
                ..Default::default()
            });
            tokio::time::sleep(Duration::from_millis(5)).await;
            for i in 0..10 {
                let mut c = DataChanges::default();
                c.emails.insert(Id::from(format!("e{i}")));
                events.data(c);
            }
            events.data(DataChanges::default());
            tokio::time::sleep(Duration::from_millis(80)).await;
            cancel.cancel();
            task.await.unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            let SyncEvent::DataChanged(second) = &seen[1] else {
                panic!("expected data");
            };
            assert_eq!(second.emails.len(), 10);
        });
    }

    #[test]
    fn trigger_absorbs_bursts() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let t = Trigger::default();
            assert!(!t.is_set());
            t.fire();
            t.fire();
            t.wait().await;
            assert!(t.take());
            assert!(!t.take());
            let waited = tokio::time::timeout(Duration::from_millis(20), t.wait()).await;
            assert!(waited.is_err(), "no fire, so wait must block");
        });
    }
}
