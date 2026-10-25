//! Engine tuning and the clock.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// The source of "now" for holds, backoff deadlines and stored times.
///
/// Tests can swap in their own clock. The engine never compares this clock
/// with server timestamps.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Tuning for the sync engine. Every wait is configurable so tests can run
/// in milliseconds; the defaults suit a real Fastmail account.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// How many conversations the first query window per mailbox holds.
    pub window: u64,
    /// How many more conversations one load-more fetches.
    pub load_more: u64,
    /// The most ids one `/changes` call may return. Capped further by the
    /// session's `maxObjectsInGet`.
    pub max_changes: u64,
    /// The most bytes of each body value the server should send.
    pub max_body_value_bytes: u64,
    /// The most body fetches in flight at once.
    pub body_concurrency: usize,
    /// How many prefetched bodies go in one request.
    pub prefetch_batch: usize,
    /// Whether to open an EventSource push connection. Without push the
    /// engine polls at [`EngineConfig::poll_interval`].
    pub push: bool,
    /// The ping interval asked of the server, in seconds.
    pub push_ping_seconds: u32,
    /// Added to twice the ping interval to get the idle timeout after which
    /// a silent push connection is dropped and reopened.
    pub push_idle_margin: Duration,
    /// Push state changes that arrive within this window start one sync.
    pub push_debounce: Duration,
    /// After this many push failures in a row the engine also polls.
    pub push_failures_before_poll: u32,
    /// How often to poll when push is off or failing.
    pub poll_interval: Duration,
    /// First wait after a failure. Doubles up to
    /// [`EngineConfig::backoff_max`], with jitter.
    pub backoff_min: Duration,
    /// Longest wait between retries.
    pub backoff_max: Duration,
    /// How often to refetch the full Masked Email list, which has no
    /// reliable change feed.
    pub masked_refresh: Duration,
    /// Data change events are merged and sent at most once per this.
    pub data_coalesce: Duration,
    /// How long shutdown waits for the engine and store threads.
    pub shutdown_timeout: Duration,
    /// The time zone for dates in server search (`before:2026-10-01`).
    pub time_zone: TimeZone,
    /// The clock.
    pub clock: Arc<dyn Clock>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            window: 100,
            load_more: 100,
            max_changes: 500,
            max_body_value_bytes: 1024 * 1024,
            body_concurrency: 2,
            prefetch_batch: 4,
            push: true,
            push_ping_seconds: 60,
            push_idle_margin: Duration::from_secs(15),
            push_debounce: Duration::from_millis(100),
            push_failures_before_poll: 3,
            poll_interval: Duration::from_secs(60),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(300),
            masked_refresh: Duration::from_secs(15 * 60),
            data_coalesce: Duration::from_millis(16),
            shutdown_timeout: Duration::from_secs(3),
            time_zone: TimeZone::system(),
            clock: Arc::new(SystemClock),
        }
    }
}

impl EngineConfig {
    /// The idle timeout for a push connection: twice the ping interval
    /// plus the margin.
    pub fn push_idle_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.push_ping_seconds.max(1)) * 2) + self.push_idle_margin
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }
}
