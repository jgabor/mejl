//! Frame timing statistics for the performance overlay.
//!
//! The shell measures each frame from input read to `Backend::flush` and
//! sends it as `Msg::Perf`. The UI keeps the last 1000 samples and reports
//! percentiles and the budget buckets from architecture.md: under 8 ms,
//! 16 ms, 50 ms, 100 ms, and worse. The smallest bucket shows regressions
//! first.

use std::collections::VecDeque;
use std::time::Duration;

use crate::event::FrameSample;

/// Samples kept.
pub const PERF_WINDOW: usize = 1000;

/// Upper bounds of the buckets, in milliseconds.
pub const BUCKETS_MS: [u64; 4] = [8, 16, 50, 100];

/// A summary of recent frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PerfSummary {
    /// Samples in the window.
    pub count: usize,
    /// Frames recorded since start.
    pub total: u64,
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// 99th percentile.
    pub p99: Duration,
    /// Slowest frame in the window.
    pub max: Duration,
    /// Frames under 8, 16, 50 and 100 ms, and slower, in the window.
    pub buckets: [usize; 5],
    /// The slowest recent frames, slowest first, at most five.
    pub slowest: Vec<Duration>,
}

/// A ring buffer of frame timings.
#[derive(Clone, Debug, Default)]
pub struct PerfStats {
    samples: VecDeque<Duration>,
    total: u64,
}

impl PerfStats {
    /// Records one frame.
    pub fn record(&mut self, sample: FrameSample) {
        if self.samples.len() == PERF_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(sample.input_to_flush);
        self.total += 1;
    }

    /// Percentiles, buckets and the slowest frames.
    pub fn summary(&self) -> PerfSummary {
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let pct = |p: f64| -> Duration {
            if sorted.is_empty() {
                return Duration::ZERO;
            }
            // Nearest rank: the smallest value with at least p of the
            // samples at or below it.
            let rank = (p * sorted.len() as f64).ceil() as usize;
            sorted[rank.clamp(1, sorted.len()) - 1]
        };
        let mut buckets = [0usize; 5];
        for d in &sorted {
            let ms = d.as_micros();
            let i = BUCKETS_MS
                .iter()
                .position(|&b| ms < u128::from(b) * 1000)
                .unwrap_or(BUCKETS_MS.len());
            buckets[i] += 1;
        }
        PerfSummary {
            count: sorted.len(),
            total: self.total,
            p50: pct(0.50),
            p95: pct(0.95),
            p99: pct(0.99),
            max: sorted.last().copied().unwrap_or_default(),
            buckets,
            slowest: sorted.iter().rev().take(5).copied().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> FrameSample {
        FrameSample {
            input_to_flush: Duration::from_millis(v),
        }
    }

    #[test]
    fn empty_summary() {
        let s = PerfStats::default().summary();
        assert_eq!(s.count, 0);
        assert_eq!(s.p99, Duration::ZERO);
    }

    #[test]
    fn percentiles_and_buckets() {
        let mut p = PerfStats::default();
        for v in 1..=100 {
            p.record(ms(v));
        }
        let s = p.summary();
        assert_eq!(s.count, 100);
        assert_eq!(s.p50, Duration::from_millis(50));
        assert_eq!(s.p95, Duration::from_millis(95));
        assert_eq!(s.p99, Duration::from_millis(99));
        assert_eq!(s.max, Duration::from_millis(100));
        assert_eq!(s.buckets, [7, 8, 34, 50, 1]);
        assert_eq!(s.slowest.len(), 5);
        assert_eq!(s.slowest[0], Duration::from_millis(100));
    }

    #[test]
    fn window_is_bounded() {
        let mut p = PerfStats::default();
        for _ in 0..(PERF_WINDOW + 10) {
            p.record(ms(1));
        }
        let s = p.summary();
        assert_eq!(s.count, PERF_WINDOW);
        assert_eq!(s.total, (PERF_WINDOW + 10) as u64);
    }
}
