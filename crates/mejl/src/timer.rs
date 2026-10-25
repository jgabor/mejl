//! Clock ticks and wake-ups for the main loop.
//!
//! mejl has no fixed render timer: an idle mejl does no work. The UI asks
//! for exactly the wake-ups it needs:
//!
//! - ticks once a second while the status clock shows seconds
//!   ([`Timer::set_ticking`]). Ticks land just after each wall-clock second
//!   changes, so the clock never shows a stale second;
//! - one-off wake-ups at given instants, for toast expiry, undo-send
//!   countdowns and mark-read delays ([`Timer::wake_at`]).
//!
//! A single background thread sleeps until the earliest deadline.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::events::{LoopEvent, LoopSender};

/// How ticks are spaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMode {
    /// Just after each wall-clock second boundary.
    WallSecond,
    /// A fixed period from when ticking starts. Used by tests.
    Every(Duration),
}

#[derive(Debug)]
struct State {
    ticking: bool,
    next_tick: Option<Instant>,
    wakes: BTreeSet<Instant>,
    stop: bool,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// The timer thread. Dropping it stops the thread.
#[derive(Debug)]
pub struct Timer {
    shared: Arc<Shared>,
    mode: TickMode,
    thread: Option<JoinHandle<()>>,
}

/// Small slack after a second boundary, so a clock read at the tick is
/// already in the new second.
const TICK_SLACK: Duration = Duration::from_millis(5);

/// The delay from `now` to just after the next wall-clock second.
pub fn until_next_second(now: SystemTime) -> Duration {
    let into = now
        .duration_since(UNIX_EPOCH)
        .map(|d| Duration::from_nanos(u64::from(d.subsec_nanos())))
        .unwrap_or(Duration::ZERO);
    Duration::from_secs(1) - into + TICK_SLACK
}

impl Timer {
    /// Starts a timer that sends [`LoopEvent::Tick`] and
    /// [`LoopEvent::Wake`] to `tx`. Ticking starts off.
    pub fn spawn<M: Send + 'static>(tx: LoopSender<M>) -> std::io::Result<Timer> {
        Self::spawn_with(tx, TickMode::WallSecond)
    }

    /// [`Timer::spawn`] with another tick spacing.
    pub fn spawn_with<M: Send + 'static>(
        tx: LoopSender<M>,
        mode: TickMode,
    ) -> std::io::Result<Timer> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                ticking: false,
                next_tick: None,
                wakes: BTreeSet::new(),
                stop: false,
            }),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("mejl-timer".into())
            .spawn(move || run(&worker, &tx, mode))?;
        Ok(Timer {
            shared,
            mode,
            thread: Some(thread),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Turns ticking on or off. Turning it on when it is on changes
    /// nothing, so the UI can call this after every update.
    pub fn set_ticking(&self, on: bool) {
        let mut state = self.lock();
        if state.ticking == on {
            return;
        }
        state.ticking = on;
        state.next_tick = on.then(|| next_tick_after(Instant::now(), self.mode));
        drop(state);
        self.shared.changed.notify_one();
    }

    /// Whether ticks are on.
    pub fn is_ticking(&self) -> bool {
        self.lock().ticking
    }

    /// Asks for one [`LoopEvent::Wake`] at `deadline`. Asking twice for the
    /// same instant gives one wake-up. A deadline in the past fires at once.
    pub fn wake_at(&self, deadline: Instant) {
        self.lock().wakes.insert(deadline);
        self.shared.changed.notify_one();
    }

    /// Converts a wall-clock time, as the UI's `Cmd::WakeAt` carries, to an
    /// instant and asks for a wake-up then.
    pub fn wake_at_timestamp(&self, at: jiff::Timestamp) {
        let now = jiff::Timestamp::now();
        let delay = if at > now {
            Duration::try_from(at.duration_since(now)).unwrap_or(Duration::ZERO)
        } else {
            Duration::ZERO
        };
        self.wake_at(Instant::now() + delay);
    }

    /// Drops every pending wake-up.
    pub fn cancel_wakes(&self) {
        self.lock().wakes.clear();
        self.shared.changed.notify_one();
    }

    /// Pending wake-ups.
    pub fn pending_wakes(&self) -> usize {
        self.lock().wakes.len()
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.lock().stop = true;
        self.shared.changed.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn next_tick_after(now: Instant, mode: TickMode) -> Instant {
    match mode {
        TickMode::WallSecond => now + until_next_second(SystemTime::now()),
        TickMode::Every(period) => now + period,
    }
}

fn run<M>(shared: &Shared, tx: &LoopSender<M>, mode: TickMode) {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if state.stop {
            return;
        }
        let now = Instant::now();
        let mut due = Vec::new();
        while let Some(&first) = state.wakes.first() {
            if first > now {
                break;
            }
            state.wakes.pop_first();
            due.push(LoopEvent::Wake(first));
        }
        if let Some(tick) = state.next_tick
            && tick <= now
        {
            due.push(LoopEvent::Tick);
            // Skip missed ticks after a stall or a suspended laptop.
            let mut next = next_tick_after(tick, mode);
            if next <= now {
                next = next_tick_after(now, mode);
            }
            state.next_tick = Some(next);
        }
        if !due.is_empty() {
            drop(state);
            for event in due {
                if tx.send(event).is_err() {
                    return;
                }
            }
            state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let next = [state.next_tick, state.wakes.first().copied()]
            .into_iter()
            .flatten()
            .min();
        state = match next {
            Some(at) => {
                let wait = at.saturating_duration_since(Instant::now());
                shared
                    .changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            // Nothing to wait for: sleep until something changes.
            None => shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{LoopReceiver, channel};

    fn next(rx: &LoopReceiver<()>, within: Duration) -> Option<LoopEvent<()>> {
        rx.recv_batch_until(Instant::now() + within)
            .into_iter()
            .next()
    }

    #[test]
    fn next_second_delay_is_within_a_second() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_250);
        assert_eq!(
            until_next_second(t),
            Duration::from_millis(750) + TICK_SLACK
        );
        let t = UNIX_EPOCH + Duration::from_secs(5);
        assert_eq!(until_next_second(t), Duration::from_secs(1) + TICK_SLACK);
    }

    #[test]
    fn wake_ups_fire_in_order_and_once() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn(tx).unwrap();
        let now = Instant::now();
        let a = now + Duration::from_millis(40);
        let b = now + Duration::from_millis(20);
        timer.wake_at(a);
        timer.wake_at(b);
        timer.wake_at(b);
        assert_eq!(timer.pending_wakes(), 2);
        match next(&rx, Duration::from_secs(5)) {
            Some(LoopEvent::Wake(at)) => assert_eq!(at, b),
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() >= b);
        match next(&rx, Duration::from_secs(5)) {
            Some(LoopEvent::Wake(at)) => assert_eq!(at, a),
            other => panic!("{other:?}"),
        }
        assert!(next(&rx, Duration::from_millis(60)).is_none());
    }

    #[test]
    fn past_deadline_fires_at_once() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn(tx).unwrap();
        let past = Instant::now();
        timer.wake_at(past);
        assert!(matches!(
            next(&rx, Duration::from_secs(5)),
            Some(LoopEvent::Wake(_))
        ));
    }

    #[test]
    fn ticks_start_and_stop() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn_with(tx, TickMode::Every(Duration::from_millis(15))).unwrap();
        assert!(
            next(&rx, Duration::from_millis(40)).is_none(),
            "off by default"
        );
        timer.set_ticking(true);
        timer.set_ticking(true);
        assert!(timer.is_ticking());
        for _ in 0..3 {
            assert!(matches!(
                next(&rx, Duration::from_secs(5)),
                Some(LoopEvent::Tick)
            ));
        }
        timer.set_ticking(false);
        // At most one tick could already be in the channel.
        let _ = rx.try_recv();
        assert!(next(&rx, Duration::from_millis(60)).is_none());
    }

    #[test]
    fn cancel_drops_pending_wakes() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn(tx).unwrap();
        timer.wake_at(Instant::now() + Duration::from_millis(30));
        timer.cancel_wakes();
        assert_eq!(timer.pending_wakes(), 0);
        assert!(next(&rx, Duration::from_millis(80)).is_none());
    }

    #[test]
    fn timestamp_wake_up_converts() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn(tx).unwrap();
        let at = jiff::Timestamp::now() + jiff::SignedDuration::from_millis(20);
        timer.wake_at_timestamp(at);
        assert!(matches!(
            next(&rx, Duration::from_secs(5)),
            Some(LoopEvent::Wake(_))
        ));
        timer.wake_at_timestamp(jiff::Timestamp::UNIX_EPOCH);
        assert!(matches!(
            next(&rx, Duration::from_secs(5)),
            Some(LoopEvent::Wake(_))
        ));
    }

    #[test]
    fn timer_stops_when_dropped_or_the_loop_hangs_up() {
        let (tx, rx) = channel::<()>();
        let timer = Timer::spawn(tx).unwrap();
        drop(rx);
        timer.wake_at(Instant::now());
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        drop(timer);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
