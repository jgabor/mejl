//! SIGTERM, SIGINT, SIGHUP and SIGCONT.
//!
//! In raw mode Ctrl+C is a key, so these signals come from `kill`, a closed
//! terminal window or a resumed job. The first stop signal is passed to the
//! main loop, which quits cleanly: the outbox and drafts are on disk and the
//! terminal is restored on the way out. If the loop does not get there and
//! a second stop signal arrives, the terminal is restored at once and the
//! process exits with `128 + signal`, the shell convention.
//!
//! SIGCONT is passed on too: after a stop the shell may have reset the
//! terminal, so the UI should redraw everything.

/// A signal mejl reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// SIGTERM: asked to stop.
    Terminate,
    /// SIGINT: interrupted, from `kill -INT`.
    Interrupt,
    /// SIGHUP: the terminal went away.
    Hangup,
    /// SIGCONT: resumed after a stop.
    Continue,
}

impl SignalKind {
    /// Whether this signal asks mejl to quit.
    pub fn is_stop(self) -> bool {
        !matches!(self, SignalKind::Continue)
    }

    /// The signal number on this platform.
    #[cfg(unix)]
    pub fn number(self) -> i32 {
        match self {
            SignalKind::Terminate => libc::SIGTERM,
            SignalKind::Interrupt => libc::SIGINT,
            SignalKind::Hangup => libc::SIGHUP,
            SignalKind::Continue => libc::SIGCONT,
        }
    }

    #[cfg(unix)]
    fn from_number(n: i32) -> Option<SignalKind> {
        [
            SignalKind::Terminate,
            SignalKind::Interrupt,
            SignalKind::Hangup,
            SignalKind::Continue,
        ]
        .into_iter()
        .find(|k| k.number() == n)
    }
}

/// What to do with a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// Tell the main loop.
    Forward,
    /// Restore the terminal and exit with this code now.
    ExitNow(i32),
}

/// Decides what to do with a signal, given how many stop signals came
/// before it.
pub fn signal_action(kind: SignalKind, earlier_stops: u32, signal_number: i32) -> SignalAction {
    if kind.is_stop() && earlier_stops >= 1 {
        SignalAction::ExitNow(128 + signal_number)
    } else {
        SignalAction::Forward
    }
}

/// A running signal watcher. Dropping it stops watching.
#[derive(Debug)]
pub struct SignalWatcher {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SignalWatcher {
    /// Watches SIGTERM, SIGINT, SIGHUP and SIGCONT on a background thread
    /// and calls `forward` for each signal the main loop should see.
    pub fn spawn(forward: impl Fn(SignalKind) + Send + 'static) -> std::io::Result<SignalWatcher> {
        Self::spawn_for(
            &[
                SignalKind::Terminate,
                SignalKind::Interrupt,
                SignalKind::Hangup,
                SignalKind::Continue,
            ],
            forward,
        )
    }

    /// [`SignalWatcher::spawn`] for a chosen set of signals.
    #[cfg(unix)]
    pub fn spawn_for(
        kinds: &[SignalKind],
        forward: impl Fn(SignalKind) + Send + 'static,
    ) -> std::io::Result<SignalWatcher> {
        let numbers: Vec<i32> = kinds.iter().map(|k| k.number()).collect();
        let mut signals = signal_hook::iterator::Signals::new(&numbers)?;
        let handle = signals.handle();
        let thread = std::thread::Builder::new()
            .name("mejl-signals".into())
            .spawn(move || {
                let mut stops = 0;
                for number in signals.forever() {
                    let Some(kind) = SignalKind::from_number(number) else {
                        continue;
                    };
                    match signal_action(kind, stops, number) {
                        SignalAction::Forward => forward(kind),
                        SignalAction::ExitNow(code) => {
                            let _ = super::restore();
                            std::process::exit(code);
                        }
                    }
                    if kind.is_stop() {
                        stops += 1;
                    }
                }
            })?;
        Ok(SignalWatcher {
            handle,
            thread: Some(thread),
        })
    }

    /// Signals are a Unix idea; elsewhere nothing is watched.
    #[cfg(not(unix))]
    pub fn spawn_for(
        _kinds: &[SignalKind],
        _forward: impl Fn(SignalKind) + Send + 'static,
    ) -> std::io::Result<SignalWatcher> {
        Ok(SignalWatcher { thread: None })
    }
}

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_is_forwarded_second_exits() {
        assert_eq!(
            signal_action(SignalKind::Terminate, 0, 15),
            SignalAction::Forward
        );
        assert_eq!(
            signal_action(SignalKind::Hangup, 1, 1),
            SignalAction::ExitNow(129)
        );
        assert_eq!(
            signal_action(SignalKind::Interrupt, 3, 2),
            SignalAction::ExitNow(130)
        );
        // Continue never exits.
        assert_eq!(
            signal_action(SignalKind::Continue, 5, 18),
            SignalAction::Forward
        );
    }

    #[cfg(unix)]
    #[test]
    fn sigcont_is_forwarded() {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = SignalWatcher::spawn_for(&[SignalKind::Continue], move |k| {
            let _ = tx.send(k);
        })
        .unwrap();
        // SAFETY: SIGCONT to a running process does nothing but run the
        // handler signal-hook installed.
        unsafe {
            libc::raise(libc::SIGCONT);
        }
        let got = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(got, SignalKind::Continue);
        drop(watcher);
    }

    #[cfg(unix)]
    #[test]
    fn numbers_round_trip() {
        for k in [
            SignalKind::Terminate,
            SignalKind::Interrupt,
            SignalKind::Hangup,
            SignalKind::Continue,
        ] {
            assert_eq!(SignalKind::from_number(k.number()), Some(k));
        }
        assert_eq!(SignalKind::from_number(libc::SIGUSR1), None);
    }
}
