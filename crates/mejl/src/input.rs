//! The input thread.
//!
//! A std thread reads terminal events, stamps each with the time it was
//! read, folds runs of mouse motion, drag and resize into their last event
//! ([`coalesce`]), and forwards them to the main loop.
//!
//! It waits on crossterm's `EventStream` and a control channel at once, so
//! an idle mejl makes no timer wake-ups, and [`InputThread::pause`] can stop
//! reading at once. That matters for `$EDITOR`: a reader still running
//! would eat the editor's keys.
//!
//! Dropping the stream is not enough on its own. crossterm reads the
//! terminal on a thread of its own, which it does not let anyone join, and
//! dropping the stream only asks that thread to stop. A pause therefore
//! also waits until that thread has let go of the stream's waker, which it
//! does right after it stops polling the terminal ([`Tracked`]).
//!
//! crossterm also does not parse OSC or DCS strings, so a terminal reply
//! that arrives after the startup probe would become a run of keys.
//! [`drop_leaked_replies`] removes those runs from each batch.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use futures::channel::mpsc;
use futures::future::{Either, select};
use futures::{Stream, StreamExt};

use crate::events::{LoopEvent, LoopSender, TimedEvent};

/// At most this many already waiting events are read in one go, so one
/// huge paste of keys cannot starve the main loop.
pub const MAX_BATCH: usize = 1024;

/// How long [`InputThread::pause`] waits for the thread to let go of the
/// terminal.
pub const PAUSE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a pause waits for a reader the stream left behind, such as
/// crossterm's event thread, to stop reading. Shorter than
/// [`PAUSE_TIMEOUT`], so a stuck reader costs a warning, not the pause.
pub const QUIESCE_TIMEOUT: Duration = Duration::from_millis(500);

/// The longest terminal reply [`drop_leaked_replies`] looks for, in keys.
pub const MAX_LEAKED_REPLY: usize = 512;

/// Folds runs of events that only the last one matters for: mouse motion
/// with the same modifiers, drag with the same button and modifiers, and
/// resizes. The merged event keeps the first event's timestamp, since the
/// user's action started then. Everything else, scroll wheel included,
/// passes through in order.
pub fn coalesce(events: Vec<TimedEvent>) -> Vec<TimedEvent> {
    let mut out: Vec<TimedEvent> = Vec::with_capacity(events.len());
    for ev in events {
        if let Some(last) = out.last_mut()
            && mergeable(&last.event, &ev.event)
        {
            last.event = ev.event;
            continue;
        }
        out.push(ev);
    }
    out
}

fn mergeable(a: &Event, b: &Event) -> bool {
    match (a, b) {
        (Event::Resize(..), Event::Resize(..)) => true,
        (Event::Mouse(a), Event::Mouse(b)) => same_motion(a, b),
        _ => false,
    }
}

fn same_motion(a: &MouseEvent, b: &MouseEvent) -> bool {
    a.modifiers == b.modifiers
        && match (a.kind, b.kind) {
            (MouseEventKind::Moved, MouseEventKind::Moved) => true,
            (MouseEventKind::Drag(x), MouseEventKind::Drag(y)) => x == y,
            _ => false,
        }
}

/// Removes terminal replies that crossterm turned into keys.
///
/// crossterm parses `ESC x` as Alt+x and does not know OSC (`ESC ]`) or DCS
/// (`ESC P`) strings. A late OSC 11 reply such as `ESC ] 11 ; rgb:1e/1e/2e
/// ESC \` therefore arrives as Alt+`]`, one key per character, then
/// Alt+`\`, or Ctrl+G when BEL ends it. DCS starts with Alt+Shift+P.
///
/// A reply arrives in one read, so only a run that starts and ends inside
/// one batch is removed: an introducer, printable characters without
/// modifiers, and a terminator. A user's own Alt+`]` arrives in a batch of
/// its own and passes, and so does a run with no terminator.
pub fn drop_leaked_replies(events: Vec<TimedEvent>) -> Vec<TimedEvent> {
    if !events.iter().any(|e| is_string_start(&e.event)) {
        return events;
    }
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        if is_string_start(&events[i].event)
            && let Some(len) = leaked_reply_len(&events[i + 1..])
        {
            tracing::debug!(keys = len + 1, "dropped a terminal reply read as keys");
            i += len + 1;
            continue;
        }
        out.push(events[i].clone());
        i += 1;
    }
    out
}

/// The keys after an introducer up to and including the terminator, if the
/// run is a whole reply.
fn leaked_reply_len(rest: &[TimedEvent]) -> Option<usize> {
    for (n, ev) in rest.iter().take(MAX_LEAKED_REPLY).enumerate() {
        let Event::Key(key) = &ev.event else {
            return None;
        };
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if is_string_end(key) {
            return Some(n + 1);
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        match key.code {
            KeyCode::Char(c) if plain && (' '..='~').contains(&c) => {}
            _ => return None,
        }
    }
    None
}

/// Alt+`]` (OSC) or Alt+Shift+P (DCS), as crossterm reports `ESC ]` and
/// `ESC P`.
fn is_string_start(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    key.kind == KeyEventKind::Press
        && match key.code {
            KeyCode::Char(']') => key.modifiers == KeyModifiers::ALT,
            KeyCode::Char('P') => key.modifiers == KeyModifiers::ALT | KeyModifiers::SHIFT,
            _ => false,
        }
}

/// Alt+`\` (ST, `ESC \`) or Ctrl+G (BEL).
fn is_string_end(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('\\') => key.modifiers == KeyModifiers::ALT,
        KeyCode::Char('g') => key.modifiers == KeyModifiers::CONTROL,
        _ => false,
    }
}

/// Where the input thread reads events from. The real one is the terminal;
/// tests use a fake.
pub trait EventSource: Send + 'static {
    /// A stream of events. Dropping it must stop all reading.
    type Stream: Stream<Item = io::Result<Event>> + Unpin;
    /// Starts reading.
    fn open(&mut self) -> Self::Stream;
    /// Events that are already waiting, without blocking. Called right
    /// after the stream yields one, while no reader is running.
    fn drain_ready(&mut self, max: usize) -> io::Result<Vec<Event>>;
}

/// The terminal, through crossterm.
#[derive(Debug, Default)]
pub struct TerminalSource;

impl EventSource for TerminalSource {
    type Stream = crossterm::event::EventStream;

    fn open(&mut self) -> Self::Stream {
        crossterm::event::EventStream::new()
    }

    fn drain_ready(&mut self, max: usize) -> io::Result<Vec<Event>> {
        let mut out = Vec::new();
        while out.len() < max && crossterm::event::poll(Duration::ZERO)? {
            out.push(crossterm::event::read()?);
        }
        Ok(out)
    }
}

/// A stream wrapper that tells when nobody else holds its waker any more.
///
/// crossterm's `EventStream` hands a clone of the waker to its reader
/// thread whenever it returns `Pending`. That thread polls the terminal
/// until a key arrives or it sees the stream was dropped, then wakes the
/// waker, which uses up the clone. So while a clone is out, the thread may
/// still read keys. Each poll here passes the inner stream a waker that is
/// an [`Arc`] of one shared [`WakerTrack`], so counting the `Arc` counts the
/// clones. The wrapper forwards every wake to the task's real waker.
pub struct Tracked<St> {
    inner: St,
    track: Arc<WakerTrack>,
}

/// The shared side of [`Tracked`].
#[derive(Debug, Default)]
pub struct WakerTrack {
    /// The task's own waker, from the latest poll.
    outer: Mutex<Option<Waker>>,
    /// Signalled on every wake, so a waiter looks again.
    woken: Condvar,
}

impl<St> Tracked<St> {
    /// Wraps a stream.
    pub fn new(inner: St) -> Self {
        Tracked {
            inner,
            track: Arc::new(WakerTrack::default()),
        }
    }

    /// A handle that outlives the stream, for [`wait_released`].
    pub fn track(&self) -> Arc<WakerTrack> {
        Arc::clone(&self.track)
    }
}

impl<St: Stream + Unpin> Stream for Tracked<St> {
    type Item = St::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<St::Item>> {
        {
            let mut outer = self
                .track
                .outer
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !outer.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                *outer = Some(cx.waker().clone());
            }
        }
        let waker = Waker::from(Arc::clone(&self.track));
        let mut inner_cx = Context::from_waker(&waker);
        Pin::new(&mut self.inner).poll_next(&mut inner_cx)
    }
}

impl Wake for WakerTrack {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let outer = self
            .outer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.woken.notify_all();
        if let Some(waker) = outer {
            waker.wake();
        }
    }
}

/// Waits until `track` is the last handle to its [`WakerTrack`]: the
/// stream is dropped and no reader it started still holds a waker. Returns
/// `false` if that did not happen within `timeout`.
pub fn wait_released(track: &Arc<WakerTrack>, timeout: Duration) -> bool {
    // A wake runs before the waker clone is dropped, so a notification can
    // come a moment early. The short step covers that.
    const STEP: Duration = Duration::from_millis(2);
    let deadline = Instant::now() + timeout;
    let mut guard = track.outer.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if Arc::strong_count(track) <= 1 {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        guard = track
            .woken
            .wait_timeout(guard, left.min(STEP))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

enum Control {
    Pause(std_mpsc::Sender<()>),
    Resume,
    Stop,
}

/// Why a pause failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PauseError {
    /// The thread has stopped.
    #[error("the input thread has stopped")]
    Stopped,
    /// The thread did not answer in time.
    #[error("the input thread did not pause in time")]
    Timeout,
}

/// A running input thread. Dropping it stops the thread.
pub struct InputThread {
    ctrl: mpsc::UnboundedSender<Control>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for InputThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputThread")
            .field("running", &self.thread.is_some())
            .finish()
    }
}

impl InputThread {
    /// Reads the terminal on a new thread and sends to `tx`.
    pub fn spawn<M: Send + 'static>(tx: LoopSender<M>) -> io::Result<InputThread> {
        Self::spawn_with(TerminalSource, tx)
    }

    /// Reads from any [`EventSource`].
    pub fn spawn_with<S: EventSource, M: Send + 'static>(
        source: S,
        tx: LoopSender<M>,
    ) -> io::Result<InputThread> {
        let (ctrl, rx) = mpsc::unbounded();
        let thread = std::thread::Builder::new()
            .name("mejl-input".into())
            .spawn(move || futures::executor::block_on(run(source, rx, tx)))?;
        Ok(InputThread {
            ctrl,
            thread: Some(thread),
        })
    }

    /// Stops reading and returns once the thread no longer reads the
    /// terminal. Events already read are delivered first.
    pub fn pause(&self) -> Result<(), PauseError> {
        let (ack, done) = std_mpsc::channel();
        self.ctrl
            .unbounded_send(Control::Pause(ack))
            .map_err(|_| PauseError::Stopped)?;
        done.recv_timeout(PAUSE_TIMEOUT).map_err(|e| match e {
            std_mpsc::RecvTimeoutError::Timeout => PauseError::Timeout,
            std_mpsc::RecvTimeoutError::Disconnected => PauseError::Stopped,
        })
    }

    /// Starts reading again after [`InputThread::pause`].
    pub fn resume(&self) {
        let _ = self.ctrl.unbounded_send(Control::Resume);
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        let _ = self.ctrl.unbounded_send(Control::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum Step {
    Event(Option<io::Result<Event>>),
    Control(Option<Control>),
}

async fn run<S: EventSource, M>(
    mut source: S,
    mut ctrl: mpsc::UnboundedReceiver<Control>,
    tx: LoopSender<M>,
) {
    loop {
        let mut stream = Tracked::new(source.open());
        let track = stream.track();
        let step = loop {
            let step = match select(stream.next(), ctrl.next()).await {
                Either::Left((event, _)) => Step::Event(event),
                Either::Right((control, _)) => Step::Control(control),
            };
            match step {
                Step::Event(Some(Ok(first))) => {
                    let mut batch = vec![TimedEvent {
                        event: first,
                        at: Instant::now(),
                    }];
                    let (more, failed) = match source.drain_ready(MAX_BATCH) {
                        Ok(more) => (more, None),
                        Err(e) => (Vec::new(), Some(e)),
                    };
                    batch.extend(more.into_iter().map(|event| TimedEvent {
                        event,
                        at: Instant::now(),
                    }));
                    // Events read before a failure still count.
                    for ev in coalesce(drop_leaked_replies(batch)) {
                        if tx.send(LoopEvent::Input(ev)).is_err() {
                            return;
                        }
                    }
                    if let Some(e) = failed {
                        break Step::Event(Some(Err(e)));
                    }
                }
                other => break other,
            }
        };
        // Reading stops here, before anyone is told about it.
        drop(stream);
        match step {
            Step::Event(Some(Err(e))) => {
                let _ = tx.send(LoopEvent::InputClosed(e.to_string()));
                return;
            }
            Step::Event(_) => {
                let _ = tx.send(LoopEvent::InputClosed("the terminal closed".into()));
                return;
            }
            Step::Control(Some(Control::Pause(ack))) => {
                // The editor starts once the ack arrives, so no reader may
                // be left polling the terminal by then.
                if !wait_released(&track, QUIESCE_TIMEOUT) {
                    tracing::warn!(
                        "the terminal reader did not stop in time; the editor may miss keys"
                    );
                }
                let _ = ack.send(());
                if !wait_for_resume(&mut ctrl).await {
                    return;
                }
            }
            // Resume while running is a no-op: open a fresh stream.
            Step::Control(Some(Control::Resume)) => {}
            Step::Control(Some(Control::Stop) | None) => return,
        }
    }
}

/// Waits while paused. Returns `false` when the thread should stop.
async fn wait_for_resume(ctrl: &mut mpsc::UnboundedReceiver<Control>) -> bool {
    loop {
        match ctrl.next().await {
            Some(Control::Resume) => return true,
            Some(Control::Pause(ack)) => {
                let _ = ack.send(());
            }
            Some(Control::Stop) | None => return false,
        }
    }
}

/// A fake [`EventSource`] for tests: events pushed through its handle come
/// out of the stream, and the handle can tell whether a stream is open.
#[derive(Debug, Clone, Default)]
pub struct FakeSource {
    shared: std::sync::Arc<std::sync::Mutex<FakeShared>>,
}

#[derive(Debug, Default)]
struct FakeShared {
    queue: VecDeque<io::Result<Event>>,
    waker: Option<std::task::Waker>,
    open_streams: usize,
    opened: usize,
}

impl FakeSource {
    /// A source with nothing queued.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeShared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queues events as if they arrived together.
    pub fn push_all(&self, events: impl IntoIterator<Item = io::Result<Event>>) {
        let mut s = self.lock();
        s.queue.extend(events);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    /// Streams open right now.
    pub fn open_streams(&self) -> usize {
        self.lock().open_streams
    }

    /// Streams opened so far.
    pub fn opened(&self) -> usize {
        self.lock().opened
    }
}

/// The stream a [`FakeSource`] hands out.
pub struct FakeStream {
    source: FakeSource,
}

impl Stream for FakeStream {
    type Item = io::Result<Event>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut s = self.source.lock();
        match s.queue.pop_front() {
            Some(item) => std::task::Poll::Ready(Some(item)),
            None => {
                s.waker = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        let mut s = self.source.lock();
        s.open_streams -= 1;
        s.waker = None;
    }
}

impl EventSource for FakeSource {
    type Stream = FakeStream;

    fn open(&mut self) -> FakeStream {
        let mut s = self.lock();
        s.open_streams += 1;
        s.opened += 1;
        drop(s);
        FakeStream {
            source: self.clone(),
        }
    }

    fn drain_ready(&mut self, max: usize) -> io::Result<Vec<Event>> {
        let mut s = self.lock();
        let mut out = Vec::new();
        while out.len() < max {
            match s.queue.pop_front() {
                Some(Ok(e)) => out.push(e),
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::channel;
    use crossterm::event::MouseButton;

    fn mouse(kind: MouseEventKind, col: u16, mods: KeyModifiers) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: col,
            row: 1,
            modifiers: mods,
        })
    }

    fn moved(col: u16) -> Event {
        mouse(MouseEventKind::Moved, col, KeyModifiers::NONE)
    }

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn timed(events: Vec<Event>) -> (Vec<TimedEvent>, Vec<Instant>) {
        let base = Instant::now();
        let stamped: Vec<TimedEvent> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| TimedEvent {
                event,
                at: base + Duration::from_millis(i as u64),
            })
            .collect();
        let times = stamped.iter().map(|e| e.at).collect();
        (stamped, times)
    }

    #[test]
    fn motion_runs_fold_into_the_last_position() {
        let (events, times) = timed(vec![
            moved(1),
            moved(2),
            moved(3),
            key('j'),
            moved(4),
            moved(5),
        ]);
        let out = coalesce(events);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].event, moved(3));
        assert_eq!(out[0].at, times[0]);
        assert_eq!(out[1].event, key('j'));
        assert_eq!(out[2].event, moved(5));
        assert_eq!(out[2].at, times[4]);
    }

    #[test]
    fn drag_folds_per_button() {
        let left = |c| {
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                c,
                KeyModifiers::NONE,
            )
        };
        let right = |c| {
            mouse(
                MouseEventKind::Drag(MouseButton::Right),
                c,
                KeyModifiers::NONE,
            )
        };
        let (events, _) = timed(vec![left(1), left(2), right(3), right(4), left(5)]);
        let out: Vec<Event> = coalesce(events).into_iter().map(|e| e.event).collect();
        assert_eq!(out, vec![left(2), right(4), left(5)]);
    }

    #[test]
    fn modifier_change_breaks_a_run() {
        let (events, _) = timed(vec![
            moved(1),
            mouse(MouseEventKind::Moved, 2, KeyModifiers::CONTROL),
        ]);
        assert_eq!(coalesce(events).len(), 2);
    }

    #[test]
    fn resizes_fold_and_others_pass() {
        let wheel = mouse(MouseEventKind::ScrollDown, 1, KeyModifiers::NONE);
        let (events, _) = timed(vec![
            Event::Resize(80, 24),
            Event::Resize(100, 30),
            Event::Resize(120, 40),
            wheel.clone(),
            wheel.clone(),
            Event::Paste("x".into()),
            Event::FocusGained,
            Event::Resize(90, 20),
        ]);
        let out: Vec<Event> = coalesce(events).into_iter().map(|e| e.event).collect();
        assert_eq!(
            out,
            vec![
                Event::Resize(120, 40),
                wheel.clone(),
                wheel,
                Event::Paste("x".into()),
                Event::FocusGained,
                Event::Resize(90, 20),
            ]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }

    #[test]
    fn press_between_drags_is_kept() {
        let down = mouse(
            MouseEventKind::Down(MouseButton::Left),
            1,
            KeyModifiers::NONE,
        );
        let drag = |c| {
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                c,
                KeyModifiers::NONE,
            )
        };
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 9, KeyModifiers::NONE);
        let (events, _) = timed(vec![down.clone(), drag(2), drag(3), up.clone()]);
        let out: Vec<Event> = coalesce(events).into_iter().map(|e| e.event).collect();
        assert_eq!(out, vec![down, drag(3), up]);
    }

    fn next_event(rx: &crate::events::LoopReceiver<()>) -> Option<LoopEvent<()>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(ev) = rx.try_recv() {
                return Some(ev);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    fn inputs(rx: &crate::events::LoopReceiver<()>, n: usize) -> Vec<Event> {
        let mut out = Vec::new();
        while out.len() < n {
            match next_event(rx) {
                Some(LoopEvent::Input(e)) => out.push(e.event),
                Some(other) => panic!("unexpected {other:?}"),
                None => panic!("timed out after {out:?}"),
            }
        }
        out
    }

    #[test]
    fn thread_forwards_coalesced_batches() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        source.push_all([Ok(moved(1)), Ok(moved(2)), Ok(moved(3)), Ok(key('a'))]);
        let got = inputs(&rx, 2);
        assert_eq!(got, vec![moved(3), key('a')]);
        drop(thread);
        assert_eq!(source.open_streams(), 0);
    }

    #[test]
    fn pause_closes_the_stream_before_acknowledging() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        source.push_all([Ok(key('a'))]);
        assert_eq!(inputs(&rx, 1), vec![key('a')]);

        thread.pause().unwrap();
        assert_eq!(source.open_streams(), 0);
        // Keys typed while paused belong to someone else and stay unread.
        source.push_all([Ok(key('e'))]);
        std::thread::sleep(Duration::from_millis(30));
        assert!(rx.try_recv().is_none());
        // A second pause while paused is acknowledged too.
        thread.pause().unwrap();

        thread.resume();
        assert_eq!(inputs(&rx, 1), vec![key('e')]);
        assert_eq!(source.opened(), 2);
        drop(thread);
    }

    #[test]
    fn read_error_closes_input() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        source.push_all([Err(io::Error::other("tty gone"))]);
        match next_event(&rx) {
            Some(LoopEvent::InputClosed(msg)) => assert!(msg.contains("tty gone")),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(thread.pause(), Err(PauseError::Stopped));
    }

    #[test]
    fn error_while_draining_closes_input_after_nothing_is_lost() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let _thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        source.push_all([Ok(key('a')), Err(io::Error::other("bad read"))]);
        assert_eq!(inputs(&rx, 1), vec![key('a')]);
        match next_event(&rx) {
            Some(LoopEvent::InputClosed(msg)) => assert!(msg.contains("bad read")),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Mimics crossterm's `EventStream`: on `Pending` a reader thread takes
    /// a clone of the waker and moves keys from the "terminal" into the
    /// stream's queue. Told to stop, it goes on reading for [`LAG`] before
    /// it notices, as crossterm's thread does between the drop and its next
    /// look at the stop flag.
    #[derive(Clone, Default)]
    struct LaggingSource {
        shared: Arc<Mutex<Lagging>>,
    }

    #[derive(Default)]
    struct Lagging {
        /// Keys typed but not read by anyone yet.
        tty: VecDeque<Event>,
        /// Keys the reader took from the terminal for the stream.
        read: VecDeque<Event>,
        readers: usize,
    }

    const LAG: Duration = Duration::from_millis(60);

    impl LaggingSource {
        fn lock(&self) -> std::sync::MutexGuard<'_, Lagging> {
            self.shared.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn type_key(&self, e: Event) {
            self.lock().tty.push_back(e);
        }
    }

    struct LaggingStream {
        source: LaggingSource,
        stop: Arc<std::sync::atomic::AtomicBool>,
        armed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Stream for LaggingStream {
        type Item = io::Result<Event>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            use std::sync::atomic::Ordering::SeqCst;
            if let Some(e) = self.source.lock().read.pop_front() {
                return Poll::Ready(Some(Ok(e)));
            }
            if !self.armed.swap(true, SeqCst) {
                let waker = cx.waker().clone();
                let source = self.source.clone();
                let stop = Arc::clone(&self.stop);
                let armed = Arc::clone(&self.armed);
                source.lock().readers += 1;
                std::thread::spawn(move || {
                    let mut stopped_at: Option<Instant> = None;
                    loop {
                        {
                            let mut s = source.lock();
                            if let Some(e) = s.tty.pop_front() {
                                s.read.push_back(e);
                                break;
                            }
                        }
                        if stop.load(SeqCst) {
                            let at = *stopped_at.get_or_insert_with(Instant::now);
                            if at.elapsed() >= LAG {
                                break;
                            }
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    source.lock().readers -= 1;
                    armed.store(false, SeqCst);
                    waker.wake();
                });
            }
            Poll::Pending
        }
    }

    impl Drop for LaggingStream {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl EventSource for LaggingSource {
        type Stream = LaggingStream;

        fn open(&mut self) -> LaggingStream {
            LaggingStream {
                source: self.clone(),
                stop: Arc::default(),
                armed: Arc::default(),
            }
        }

        fn drain_ready(&mut self, max: usize) -> io::Result<Vec<Event>> {
            let mut s = self.lock();
            let n = s.read.len().min(max);
            Ok(s.read.drain(..n).collect())
        }
    }

    #[test]
    fn pause_waits_for_a_reader_the_stream_left_behind() {
        let source = LaggingSource::default();
        let (tx, rx) = channel::<()>();
        let thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        source.type_key(key('a'));
        assert_eq!(inputs(&rx, 1), vec![key('a')]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while source.lock().readers == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(source.lock().readers, 1, "a reader is polling");

        thread.pause().unwrap();
        // The editor runs now. Its keys must stay in the terminal.
        assert_eq!(source.lock().readers, 0, "the reader stopped first");
        source.type_key(key('e'));
        std::thread::sleep(LAG + Duration::from_millis(40));
        assert_eq!(source.lock().tty.len(), 1, "nobody read the editor's key");
        assert!(rx.try_recv().is_none());

        thread.resume();
        assert_eq!(inputs(&rx, 1), vec![key('e')]);
        drop(thread);
    }

    #[test]
    fn tracked_counts_waker_clones() {
        let mut stream = Tracked::new(futures::stream::pending::<()>());
        let track = stream.track();
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut stream).poll_next(&mut cx).is_pending());
        drop(stream);
        assert!(wait_released(&track, Duration::ZERO));

        // A clone held elsewhere keeps it busy until that clone is used up.
        let stream = Tracked::new(futures::stream::pending::<()>());
        let track = stream.track();
        let held = Waker::from(Arc::clone(&track));
        drop(stream);
        assert!(!wait_released(&track, Duration::from_millis(10)));
        // The waiter takes the only other handle, so it counts alone.
        let waiter = std::thread::spawn(move || wait_released(&track, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        held.wake();
        assert!(waiter.join().unwrap());
    }

    /// The keys crossterm 0.29 makes of `bytes`, as seen in a pty: `ESC x`
    /// is Alt+x, BEL is Ctrl+G, uppercase letters carry Shift.
    fn as_crossterm_keys(bytes: &str) -> Vec<Event> {
        let mut out = Vec::new();
        let mut chars = bytes.chars();
        while let Some(c) = chars.next() {
            let (code, mut mods) = match c {
                '\x1b' => {
                    let Some(next) = chars.next() else { break };
                    (KeyCode::Char(next), KeyModifiers::ALT)
                }
                '\x07' => (KeyCode::Char('g'), KeyModifiers::CONTROL),
                c => (KeyCode::Char(c), KeyModifiers::NONE),
            };
            if let KeyCode::Char(c) = code
                && c.is_uppercase()
            {
                mods |= KeyModifiers::SHIFT;
            }
            out.push(Event::Key(KeyEvent::new(code, mods)));
        }
        out
    }

    fn filtered(events: Vec<Event>) -> Vec<Event> {
        let (stamped, _) = timed(events);
        drop_leaked_replies(stamped)
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn leaked_osc_and_dcs_replies_are_dropped() {
        let mut events = as_crossterm_keys("\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\");
        events.push(key('j'));
        assert_eq!(filtered(events), vec![key('j')]);

        let mut events = vec![key('k')];
        events.extend(as_crossterm_keys("\x1bP>|kitty(0.49.2)\x1b\\"));
        events.extend(as_crossterm_keys("\x1b]11;rgb:ffff/ffff/ffff\x07"));
        assert_eq!(filtered(events), vec![key('k')]);
    }

    #[test]
    fn user_alt_keys_are_kept() {
        let alt = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        // Alone, with no terminator, or broken by a non-character event.
        for events in [
            vec![alt(']')],
            vec![alt(']'), key('j'), key('k')],
            vec![alt(']'), Event::FocusGained, alt('\\')],
            vec![
                alt(']'),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                alt('\\'),
            ],
        ] {
            assert_eq!(filtered(events.clone()), events);
        }
    }

    #[test]
    fn thread_drops_a_reply_that_arrives_as_keys() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let _thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        let mut events = as_crossterm_keys("\x1b]11;rgb:0000/0000/0000\x1b\\");
        events.push(key('j'));
        source.push_all(events.into_iter().map(Ok));
        assert_eq!(inputs(&rx, 1), vec![key('j')]);
        std::thread::sleep(Duration::from_millis(30));
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn thread_stops_when_the_loop_hangs_up() {
        let source = FakeSource::new();
        let (tx, rx) = channel::<()>();
        let thread = InputThread::spawn_with(source.clone(), tx).unwrap();
        drop(rx);
        source.push_all([Ok(key('a'))]);
        // The thread notices on its next send and ends; drop must not hang.
        let start = Instant::now();
        drop(thread);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
