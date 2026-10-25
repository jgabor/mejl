//! The body render workers.
//!
//! A few std threads turn message bodies into styled lines with
//! mejl-render, off the UI thread, and send each result to the main loop as
//! [`LoopEvent::Rendered`].
//!
//! The queue is bounded and knows that only the newest request per email
//! matters. A resize sends a new width for the open message many times a
//! second; a newer request replaces a waiting one for the same email in
//! place, and a result whose request was replaced while it ran is dropped
//! instead of delivered. When the queue is full, the oldest request is
//! dropped, since the newest is what the user is looking at.
//!
//! Workers keep a small cache of prepared bodies ([`PreparedBody`]), so a
//! rewrap at a new width skips parsing and cleaning the HTML.
//!
//! mejl-render has no `render_body` function; the default renderer is
//! `PreparedBody::auto` followed by `render(width)`.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mejl_render::{PreparedBody, RenderOptions, RenderedBody};

use crate::events::{LoopEvent, LoopSender};

/// A body's text parts. Shared, so requeueing a body at a new width copies
/// nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyText {
    /// The `text/plain` part.
    pub plain: Option<Arc<str>>,
    /// The `text/html` part.
    pub html: Option<Arc<str>>,
}

/// One render job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    /// The email the body belongs to. Requests are deduplicated by this.
    pub email_id: String,
    /// Target width in columns.
    pub width: usize,
    /// Show HTML when both parts exist.
    pub prefer_html: bool,
    /// The body.
    pub body: BodyText,
}

/// A finished render.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderDone {
    /// The email.
    pub email_id: String,
    /// The width it was laid out for.
    pub width: usize,
    /// Whether HTML was preferred.
    pub prefer_html: bool,
    /// The lines, or why rendering failed.
    pub result: Result<RenderedBody, String>,
    /// Time spent rendering.
    pub elapsed: Duration,
}

/// What [`RenderQueue::submit`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Added at the back.
    Queued,
    /// Replaced a waiting request for the same email in place.
    Replaced,
    /// Added, and the oldest waiting request was dropped to make room.
    DroppedOldest,
}

/// A queued request and its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Higher is newer. Unique per submit.
    pub generation: u64,
    /// The request.
    pub request: RenderRequest,
}

/// The queue and staleness rules, without threads.
#[derive(Debug)]
pub struct RenderQueue {
    jobs: VecDeque<Job>,
    capacity: usize,
    next_generation: u64,
    latest: HashMap<String, u64>,
}

impl RenderQueue {
    /// A queue holding at most `capacity` waiting requests (at least 1).
    pub fn new(capacity: usize) -> Self {
        RenderQueue {
            jobs: VecDeque::new(),
            capacity: capacity.max(1),
            next_generation: 1,
            latest: HashMap::new(),
        }
    }

    /// Adds a request; it supersedes every earlier one for that email.
    pub fn submit(&mut self, request: RenderRequest) -> SubmitOutcome {
        let generation = self.next_generation;
        self.next_generation += 1;
        self.latest.insert(request.email_id.clone(), generation);
        if let Some(waiting) = self
            .jobs
            .iter_mut()
            .find(|j| j.request.email_id == request.email_id)
        {
            *waiting = Job {
                generation,
                request,
            };
            return SubmitOutcome::Replaced;
        }
        let mut outcome = SubmitOutcome::Queued;
        if self.jobs.len() >= self.capacity
            && let Some(dropped) = self.jobs.pop_front()
        {
            self.forget_if_latest(&dropped);
            outcome = SubmitOutcome::DroppedOldest;
        }
        self.jobs.push_back(Job {
            generation,
            request,
        });
        outcome
    }

    /// The next request to work on.
    pub fn take(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    /// Whether a job is still the newest request for its email.
    pub fn is_current(&self, job: &Job) -> bool {
        self.latest.get(&job.request.email_id) == Some(&job.generation)
    }

    /// Records that a job finished. Returns whether its result should be
    /// delivered.
    pub fn finish(&mut self, job: &Job) -> bool {
        let current = self.is_current(job);
        if current {
            self.latest.remove(&job.request.email_id);
        }
        current
    }

    /// Forgets every request for an email: a waiting one is removed and a
    /// running one's result will be dropped.
    pub fn cancel(&mut self, email_id: &str) {
        self.jobs.retain(|j| j.request.email_id != email_id);
        self.latest.remove(email_id);
    }

    /// Requests waiting.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    fn forget_if_latest(&mut self, job: &Job) {
        if self.is_current(job) {
            self.latest.remove(&job.request.email_id);
        }
    }
}

/// The function workers run. Tests inject their own.
pub type RenderFn = Arc<dyn Fn(&RenderRequest) -> RenderedBody + Send + Sync>;

/// Prepared bodies by email and part choice. `PreparedBody::auto` picks
/// the part from `prefer_html`, so both are in the key.
#[derive(Default)]
struct PreparedCache {
    entries: VecDeque<(PreparedKey, SharedPrepared)>,
}

/// Email id and `prefer_html`.
type PreparedKey = (String, bool);
type SharedPrepared = Arc<Mutex<PreparedBody>>;

/// Prepared bodies kept for rewrapping.
pub const PREPARED_CACHE_SIZE: usize = 16;

impl PreparedCache {
    /// A cached body, moved to the most recently used end.
    fn lookup(&mut self, key: &PreparedKey) -> Option<SharedPrepared> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(pos)?;
        let body = entry.1.clone();
        self.entries.push_back(entry);
        Some(body)
    }

    /// Adds a body, unless another worker added one for the key first, in
    /// which case that one is returned and `prepared` is dropped.
    fn insert(&mut self, key: PreparedKey, prepared: SharedPrepared) -> SharedPrepared {
        if let Some(existing) = self.lookup(&key) {
            return existing;
        }
        if self.entries.len() >= PREPARED_CACHE_SIZE {
            self.entries.pop_front();
        }
        self.entries.push_back((key, prepared.clone()));
        prepared
    }
}

/// The prepared body for a request, from the cache or freshly prepared.
/// The cache lock is held only to look up and insert, never while
/// preparing, so a slow HTML parse on one worker does not stall the others.
fn get_or_prepare(
    cache: &Mutex<PreparedCache>,
    request: &RenderRequest,
    options: &RenderOptions,
) -> SharedPrepared {
    let key = (request.email_id.clone(), request.prefer_html);
    let lock = || cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = lock().lookup(&key) {
        return hit;
    }
    let options = RenderOptions {
        width: request.width,
        prefer_html: request.prefer_html,
        ..*options
    };
    let prepared = Arc::new(Mutex::new(PreparedBody::auto(
        request.body.plain.as_deref(),
        request.body.html.as_deref(),
        &options,
    )));
    lock().insert(key, prepared)
}

/// The default renderer: mejl-render with a rewrap cache. `options` gives
/// everything but width and part choice, which come with each request.
pub fn default_renderer(options: RenderOptions) -> RenderFn {
    let cache = Arc::new(Mutex::new(PreparedCache::default()));
    Arc::new(move |request: &RenderRequest| {
        let prepared = get_or_prepare(&cache, request, &options);
        // The layout runs under the entry's own lock, so only renders of
        // the same body wait for each other.
        let body = prepared.lock().unwrap_or_else(PoisonError::into_inner);
        body.render(request.width)
    })
}

#[derive(Debug)]
struct PoolState {
    queue: RenderQueue,
    stop: bool,
}

struct Shared {
    state: Mutex<PoolState>,
    work: Condvar,
}

/// The worker pool. Dropping it stops the workers once their current job
/// is done; waiting jobs are dropped.
pub struct RenderPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for RenderPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderPool")
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

/// Waiting requests the default pool holds.
pub const DEFAULT_QUEUE: usize = 32;

/// Worker threads for this machine: two, or one on a single core.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(2))
}

impl RenderPool {
    /// Starts `threads` workers running mejl-render.
    pub fn spawn<M: Send + 'static>(
        threads: usize,
        capacity: usize,
        options: RenderOptions,
        tx: LoopSender<M>,
    ) -> std::io::Result<RenderPool> {
        Self::spawn_with(threads, capacity, default_renderer(options), tx)
    }

    /// Starts workers running `render`.
    pub fn spawn_with<M: Send + 'static>(
        threads: usize,
        capacity: usize,
        render: RenderFn,
        tx: LoopSender<M>,
    ) -> std::io::Result<RenderPool> {
        let shared = Arc::new(Shared {
            state: Mutex::new(PoolState {
                queue: RenderQueue::new(capacity),
                stop: false,
            }),
            work: Condvar::new(),
        });
        let mut handles = Vec::new();
        for i in 0..threads.max(1) {
            let worker_shared = shared.clone();
            let render = render.clone();
            let tx = tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("mejl-render-{i}"))
                .spawn(move || worker(&worker_shared, &render, &tx));
            match handle {
                Ok(h) => handles.push(h),
                Err(e) => {
                    let pool = RenderPool {
                        shared,
                        threads: handles,
                    };
                    drop(pool);
                    return Err(e);
                }
            }
        }
        Ok(RenderPool {
            shared,
            threads: handles,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a request.
    pub fn submit(&self, request: RenderRequest) -> SubmitOutcome {
        let outcome = self.lock().queue.submit(request);
        self.shared.work.notify_one();
        outcome
    }

    /// Forgets every request for an email.
    pub fn cancel(&self, email_id: &str) {
        self.lock().queue.cancel(email_id);
    }

    /// Requests waiting for a worker.
    pub fn waiting(&self) -> usize {
        self.lock().queue.len()
    }
}

impl Drop for RenderPool {
    fn drop(&mut self) {
        self.lock().stop = true;
        self.shared.work.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker<M>(shared: &Shared, render: &RenderFn, tx: &LoopSender<M>) {
    crate::terminal::mark_thread_catches_panics();
    loop {
        let job = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if state.stop {
                    return;
                }
                if let Some(job) = state.queue.take() {
                    break job;
                }
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let started = Instant::now();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| render(&job.request)))
            .map_err(|_| "the renderer failed on this message".to_owned());
        let elapsed = started.elapsed();
        let deliver = shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queue
            .finish(&job);
        if deliver {
            let done = RenderDone {
                email_id: job.request.email_id,
                width: job.request.width,
                prefer_html: job.request.prefer_html,
                result,
                elapsed,
            };
            if tx.send(LoopEvent::Rendered(done)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::channel;
    use std::sync::Barrier;

    fn req(id: &str, width: usize) -> RenderRequest {
        RenderRequest {
            email_id: id.into(),
            width,
            prefer_html: false,
            body: BodyText {
                plain: Some(Arc::from("Hello there, this is a test body.")),
                html: None,
            },
        }
    }

    #[test]
    fn newer_request_replaces_waiting_one() {
        let mut q = RenderQueue::new(8);
        assert_eq!(q.submit(req("a", 80)), SubmitOutcome::Queued);
        assert_eq!(q.submit(req("b", 80)), SubmitOutcome::Queued);
        assert_eq!(q.submit(req("a", 100)), SubmitOutcome::Replaced);
        assert_eq!(q.len(), 2);
        let first = q.take().unwrap();
        assert_eq!(
            (first.request.email_id.as_str(), first.request.width),
            ("a", 100)
        );
        assert!(q.finish(&first));
        let second = q.take().unwrap();
        assert_eq!(second.request.email_id, "b");
        assert!(q.take().is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn running_job_goes_stale_when_a_newer_one_arrives() {
        let mut q = RenderQueue::new(8);
        q.submit(req("a", 80));
        let running = q.take().unwrap();
        q.submit(req("a", 120));
        assert!(!q.is_current(&running));
        assert!(!q.finish(&running), "stale result must be dropped");
        let newer = q.take().unwrap();
        assert!(q.finish(&newer));
        // Finished entries are forgotten, so memory stays bounded.
        assert!(q.latest.is_empty());
    }

    #[test]
    fn full_queue_drops_the_oldest() {
        let mut q = RenderQueue::new(2);
        q.submit(req("a", 80));
        q.submit(req("b", 80));
        assert_eq!(q.submit(req("c", 80)), SubmitOutcome::DroppedOldest);
        let ids: Vec<String> = std::iter::from_fn(|| q.take())
            .map(|j| j.request.email_id)
            .collect();
        assert_eq!(ids, ["b", "c"]);
        assert!(!q.latest.contains_key("a"));
        assert_eq!(RenderQueue::new(0).capacity, 1);
    }

    #[test]
    fn cancel_forgets_waiting_and_running() {
        let mut q = RenderQueue::new(4);
        q.submit(req("a", 80));
        let running = q.take().unwrap();
        q.submit(req("a", 90));
        q.submit(req("b", 90));
        q.cancel("a");
        assert_eq!(q.len(), 1);
        assert!(!q.finish(&running));
    }

    fn recv_rendered(rx: &crate::events::LoopReceiver<()>) -> Option<RenderDone> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for ev in rx.recv_batch_until(deadline) {
                if let LoopEvent::Rendered(done) = ev {
                    return Some(done);
                }
            }
        }
        None
    }

    #[test]
    fn stale_result_is_not_delivered() {
        // The worker blocks inside the first render until the test has
        // queued a newer width, so the order is fixed.
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let render: RenderFn = {
            let (started, release, calls) = (started.clone(), release.clone(), calls.clone());
            Arc::new(move |r: &RenderRequest| {
                calls.lock().unwrap().push(r.width);
                if r.width == 80 {
                    started.wait();
                    release.wait();
                }
                RenderedBody {
                    width: r.width,
                    ..RenderedBody::default()
                }
            })
        };
        let (tx, rx) = channel::<()>();
        let pool = RenderPool::spawn_with(1, 8, render, tx).unwrap();
        pool.submit(req("a", 80));
        started.wait();
        pool.submit(req("a", 100));
        pool.submit(req("a", 120));
        assert_eq!(pool.waiting(), 1);
        release.wait();
        let done = recv_rendered(&rx).unwrap();
        assert_eq!(done.width, 120);
        assert_eq!(done.result.unwrap().width, 120);
        drop(pool);
        assert_eq!(*calls.lock().unwrap(), vec![80, 120]);
        assert!(rx.try_recv().is_none(), "the width 80 result was dropped");
    }

    #[test]
    fn real_renderer_produces_lines_and_rewraps() {
        let (tx, rx) = channel::<()>();
        let pool = RenderPool::spawn(2, 8, RenderOptions::default(), tx).unwrap();
        pool.submit(req("a", 40));
        let done = recv_rendered(&rx).unwrap();
        let body = done.result.unwrap();
        assert_eq!(body.width, 40);
        assert!(body.lines.iter().any(|l| l.text().contains("Hello there")));
        pool.submit(req("a", 12));
        let narrow = recv_rendered(&rx).unwrap().result.unwrap();
        assert!(narrow.lines.iter().all(|l| l.width() <= 12));
        assert!(narrow.lines.len() > body.lines.len());
    }

    #[test]
    fn html_body_renders() {
        let (tx, rx) = channel::<()>();
        let pool = RenderPool::spawn(1, 8, RenderOptions::default(), tx).unwrap();
        pool.submit(RenderRequest {
            email_id: "h".into(),
            width: 60,
            prefer_html: true,
            body: BodyText {
                plain: Some(Arc::from("plain")),
                html: Some(Arc::from(
                    "<p>Hello <b>HTML</b> <a href=\"https://example.com\">link</a></p>",
                )),
            },
        });
        let done = recv_rendered(&rx).unwrap();
        assert!(done.prefer_html);
        let body = done.result.unwrap();
        assert_eq!(body.notices.source, mejl_render::BodySource::Html);
        assert_eq!(body.links.len(), 1);
    }

    #[test]
    fn a_panicking_renderer_reports_and_keeps_working() {
        let render: RenderFn = Arc::new(|r: &RenderRequest| {
            if r.email_id == "boom" {
                panic!("renderer bug");
            }
            RenderedBody::default()
        });
        let (tx, rx) = channel::<()>();
        let pool = RenderPool::spawn_with(1, 8, render, tx).unwrap();
        pool.submit(req("boom", 80));
        let done = recv_rendered(&rx).unwrap();
        assert!(done.result.is_err());
        pool.submit(req("fine", 80));
        assert!(recv_rendered(&rx).unwrap().result.is_ok());
    }

    #[test]
    fn prepared_cache_is_bounded_and_keyed_by_part_choice() {
        let cache = Mutex::new(PreparedCache::default());
        let options = RenderOptions::default();
        let a = get_or_prepare(&cache, &req("a", 80), &options);
        let a_again = get_or_prepare(&cache, &req("a", 40), &options);
        assert!(Arc::ptr_eq(&a, &a_again));
        let mut html = req("a", 80);
        html.prefer_html = true;
        let a_html = get_or_prepare(&cache, &html, &options);
        assert!(!Arc::ptr_eq(&a, &a_html));
        for i in 0..PREPARED_CACHE_SIZE + 5 {
            get_or_prepare(&cache, &req(&format!("m{i}"), 80), &options);
        }
        assert_eq!(cache.lock().unwrap().entries.len(), PREPARED_CACHE_SIZE);
    }

    #[test]
    fn racing_insert_keeps_the_first_body() {
        let mut cache = PreparedCache::default();
        let options = RenderOptions::default();
        let make = || Arc::new(Mutex::new(PreparedBody::plain("x", &options)));
        let key = ("a".to_owned(), false);
        let first = cache.insert(key.clone(), make());
        let second = cache.insert(key, make());
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.entries.len(), 1);
    }
}
