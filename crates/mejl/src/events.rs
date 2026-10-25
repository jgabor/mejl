//! The main loop's channel.
//!
//! The UI thread blocks on one channel that carries everything it reacts
//! to: input from the input thread, finished body renders, clock ticks and
//! wake-ups, signals, and messages from the sync engine. The sync payload
//! is the type parameter `M`, so this module does not depend on the sync
//! or UI crates; the binary picks `M` when it wires them together.
//!
//! The channel is an unbounded `std::sync::mpsc` channel. Sending never
//! blocks, so tokio tasks can send from inside the runtime, and the UI
//! drains everything pending before it renders once.

use std::sync::mpsc;
use std::time::Instant;

use crate::render_pool::RenderDone;
use crate::terminal::signals::SignalKind;

/// One terminal event with the time the input thread read it. That time
/// starts the input-to-flush latency measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedEvent {
    /// The event.
    pub event: crossterm::event::Event,
    /// When it was read. For a coalesced run of motion or resize events,
    /// this is when the first of the run was read.
    pub at: Instant,
}

/// A report the terminal sends on its own, read from its input bytes.
///
/// Not a [`LoopEvent`] yet: nothing produces one until mejl reads the
/// terminal's bytes before crossterm does (see
/// [`crate::terminal::theme`]). [`crate::terminal::theme::parse_reports`]
/// makes these from bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReport {
    /// Mode 2031: the terminal's colour scheme is now dark
    /// ([`Background::Dark`](crate::terminal::probe::Background::Dark)) or
    /// light. Never `Unknown`.
    Theme(crate::terminal::probe::Background),
}

/// Everything the main loop receives.
#[derive(Debug)]
pub enum LoopEvent<M> {
    /// A terminal event.
    Input(TimedEvent),
    /// The input thread stopped because reading the terminal failed. The
    /// loop should restore the terminal and quit.
    InputClosed(String),
    /// A body render finished.
    Rendered(RenderDone),
    /// A clock tick, once a second while ticking is on.
    Tick,
    /// A wake-up the UI asked for, at this deadline.
    Wake(Instant),
    /// A signal: quit cleanly on a stop signal, redraw after SIGCONT.
    Signal(SignalKind),
    /// A message from the sync engine or another backend.
    Msg(M),
}

/// The sending side. Cheap to clone; every producer gets one.
#[derive(Debug)]
pub struct LoopSender<M> {
    tx: mpsc::Sender<LoopEvent<M>>,
}

// Derive would require `M: Clone`, which the channel does not need.
impl<M> Clone for LoopSender<M> {
    fn clone(&self) -> Self {
        LoopSender {
            tx: self.tx.clone(),
        }
    }
}

/// The main loop hung up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the main loop has stopped")]
pub struct Disconnected;

impl<M> LoopSender<M> {
    /// Sends one event. Fails only when the receiver is gone, which means
    /// the program is shutting down.
    pub fn send(&self, event: LoopEvent<M>) -> Result<(), Disconnected> {
        self.tx.send(event).map_err(|_| Disconnected)
    }

    /// Sends a backend message.
    pub fn msg(&self, msg: M) -> Result<(), Disconnected> {
        self.send(LoopEvent::Msg(msg))
    }
}

/// The receiving side, owned by the UI thread.
#[derive(Debug)]
pub struct LoopReceiver<M> {
    rx: mpsc::Receiver<LoopEvent<M>>,
}

impl<M> LoopReceiver<M> {
    /// Blocks for the next event. `None` once every sender is gone.
    pub fn recv(&self) -> Option<LoopEvent<M>> {
        self.rx.recv().ok()
    }

    /// The next event if one is waiting.
    pub fn try_recv(&self) -> Option<LoopEvent<M>> {
        self.rx.try_recv().ok()
    }

    /// Blocks for one event, then takes everything else already waiting,
    /// in order. This is one turn of the main loop: apply them all, then
    /// render once. Empty once every sender is gone.
    pub fn recv_batch(&self) -> Vec<LoopEvent<M>> {
        let mut batch = Vec::new();
        if let Some(first) = self.recv() {
            batch.push(first);
            batch.extend(self.rx.try_iter());
        }
        batch
    }

    /// Like [`LoopReceiver::recv_batch`] but gives up at `deadline`.
    pub fn recv_batch_until(&self, deadline: Instant) -> Vec<LoopEvent<M>> {
        let mut batch = Vec::new();
        let wait = deadline.saturating_duration_since(Instant::now());
        if let Ok(first) = self.rx.recv_timeout(wait) {
            batch.push(first);
            batch.extend(self.rx.try_iter());
        }
        batch
    }
}

/// A new main loop channel.
pub fn channel<M>() -> (LoopSender<M>, LoopReceiver<M>) {
    let (tx, rx) = mpsc::channel();
    (LoopSender { tx }, LoopReceiver { rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct NotClone(u32);

    #[test]
    fn batch_takes_everything_waiting_in_order() {
        let (tx, rx) = channel::<NotClone>();
        let tx2 = tx.clone();
        tx.msg(NotClone(1)).unwrap();
        tx2.send(LoopEvent::Tick).unwrap();
        tx.msg(NotClone(2)).unwrap();
        let batch = rx.recv_batch();
        assert_eq!(batch.len(), 3);
        assert!(matches!(batch[0], LoopEvent::Msg(NotClone(1))));
        assert!(matches!(batch[1], LoopEvent::Tick));
        assert!(matches!(batch[2], LoopEvent::Msg(NotClone(2))));
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn closed_channel_gives_empty_batch_and_send_errors() {
        let (tx, rx) = channel::<()>();
        drop(tx);
        assert!(rx.recv_batch().is_empty());
        let (tx, rx) = channel::<()>();
        drop(rx);
        assert_eq!(tx.send(LoopEvent::Tick), Err(Disconnected));
    }

    #[test]
    fn batch_until_times_out() {
        let (_tx, rx) = channel::<()>();
        let start = Instant::now();
        let batch = rx.recv_batch_until(start + std::time::Duration::from_millis(20));
        assert!(batch.is_empty());
        assert!(start.elapsed() >= std::time::Duration::from_millis(20));
    }

    #[test]
    fn senders_work_from_other_threads() {
        let (tx, rx) = channel::<u32>();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let tx = tx.clone();
                std::thread::spawn(move || tx.msg(i).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        drop(tx);
        let mut got: Vec<u32> = std::iter::from_fn(|| rx.recv())
            .filter_map(|e| match e {
                LoopEvent::Msg(m) => Some(m),
                _ => None,
            })
            .collect();
        got.sort_unstable();
        assert_eq!(got, [0, 1, 2, 3]);
    }
}
