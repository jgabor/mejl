//! Exponential backoff with jitter, and the small amount of randomness the
//! engine needs.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A random 64-bit value. Each call mixes a per-process random key, the
/// time and a counter, which is plenty for jitter and for Message-IDs. It
/// is not for secrets.
pub(crate) fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(nanos);
    h.finish()
}

/// Exponential backoff between `min` and `max`, with full jitter on the
/// upper half so many clients do not retry in step.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    min: Duration,
    max: Duration,
    failures: u32,
}

impl Backoff {
    pub(crate) fn new(min: Duration, max: Duration) -> Self {
        Backoff {
            min,
            max: max.max(min),
            failures: 0,
        }
    }

    /// The wait before the next attempt, counting one more failure.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = delay_for(self.min, self.max, self.failures);
        self.failures = self.failures.saturating_add(1);
        delay
    }

    pub(crate) fn reset(&mut self) {
        self.failures = 0;
    }
}

/// The jittered delay after `failures` earlier failures: a value between
/// half and all of `min * 2^failures`, capped at `max`.
pub(crate) fn delay_for(min: Duration, max: Duration, failures: u32) -> Duration {
    let base = min
        .checked_mul(1u32.checked_shl(failures.min(31)).unwrap_or(u32::MAX))
        .unwrap_or(max)
        .min(max);
    let half = base / 2;
    let span = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    let jitter = if span == 0 {
        0
    } else {
        random_u64() % span.saturating_add(1)
    };
    half + Duration::from_nanos(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_and_caps() {
        let min = Duration::from_millis(100);
        let max = Duration::from_secs(1);
        let mut b = Backoff::new(min, max);
        let first = b.next_delay();
        assert!(
            first >= Duration::from_millis(50) && first <= min,
            "{first:?}"
        );
        let second = b.next_delay();
        assert!(second >= Duration::from_millis(100) && second <= Duration::from_millis(200));
        for _ in 0..40 {
            assert!(b.next_delay() <= max);
        }
        assert!(b.failures > 30);
        b.reset();
        assert_eq!(b.failures, 0);
        assert!(b.next_delay() <= min);
    }

    #[test]
    fn zero_minimum_is_fine() {
        assert_eq!(delay_for(Duration::ZERO, Duration::ZERO, 5), Duration::ZERO);
    }

    #[test]
    fn random_values_differ() {
        let a = random_u64();
        let b = random_u64();
        assert_ne!(a, b);
    }
}
