//! Lock-free latency/interval histograms shared by the input and audio paths.
//!
//! Recording a sample is a single atomic increment on a fixed bucket array:
//! no allocation, no locks, no I/O. Snapshots and percentiles are only taken
//! from the control plane.

use std::sync::atomic::{AtomicU64, Ordering};

/// Total number of buckets. The last one is the overflow counter.
pub const BUCKET_COUNT: usize = 4096;

/// Buckets below [`FINE_WIDTH_NS`] resolution: 0 .. [`FINE_END_NS`].
pub const FINE_BUCKETS: usize = 2048;

/// Buckets above [`FINE_WIDTH_NS`] resolution: [`FINE_END_NS`] .. [`MAX_NS`].
pub const COARSE_BUCKETS: usize = BUCKET_COUNT - 1 - FINE_BUCKETS;

/// Nanoseconds per fine bucket (1 µs).
pub const FINE_WIDTH_NS: u64 = 1_000;

/// Nanoseconds per coarse bucket (32 µs).
pub const COARSE_WIDTH_NS: u64 = 32_000;

/// Upper bound of the fine region.
pub const FINE_END_NS: u64 = FINE_BUCKETS as u64 * FINE_WIDTH_NS;

/// Largest value the histogram can represent; anything higher is overflow.
pub const MAX_NS: u64 = FINE_END_NS + COARSE_BUCKETS as u64 * COARSE_WIDTH_NS;

/// A fixed-resolution histogram of durations in nanoseconds.
///
/// Two regions, chosen around what the engine actually measures:
///
/// * `0 .. 2.048 ms` at **1 µs** resolution - covers callback duration and
///   input-to-command latency.
/// * `2.048 ms .. 67.552 ms` at **32 µs** resolution - covers queue latency
///   and scheduling jitter against a 1024-frame / 21.3 ms quantum.
/// * beyond that: one overflow bucket.
///
/// Recording is a single relaxed atomic increment, so it is safe to call from
/// the real-time callback.
pub struct AtomicHistogram {
    buckets: [AtomicU64; BUCKET_COUNT],
    overflows: AtomicU64,
}

impl AtomicHistogram {
    /// Creates an empty histogram.
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            overflows: AtomicU64::new(0),
        }
    }

    /// Bucket index a sample falls into.
    #[inline]
    fn index_for(ns: u64) -> usize {
        if ns < FINE_END_NS {
            (ns / FINE_WIDTH_NS) as usize
        } else if ns < MAX_NS {
            FINE_BUCKETS + ((ns - FINE_END_NS) / COARSE_WIDTH_NS) as usize
        } else {
            BUCKET_COUNT - 1
        }
    }

    /// Lower bound, in nanoseconds, of bucket `index`.
    #[inline]
    fn bucket_start_ns(index: usize) -> u64 {
        if index < FINE_BUCKETS {
            index as u64 * FINE_WIDTH_NS
        } else if index >= BUCKET_COUNT - 1 {
            MAX_NS
        } else {
            FINE_END_NS + (index - FINE_BUCKETS) as u64 * COARSE_WIDTH_NS
        }
    }

    /// Records one sample in nanoseconds.
    ///
    /// Real-time safe: one relaxed atomic increment, no allocation.
    #[inline]
    pub fn record_ns(&self, ns: u64) {
        self.buckets[Self::index_for(ns)].fetch_add(1, Ordering::Relaxed);
        if ns >= MAX_NS {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Returns the total number of recorded samples.
    pub fn count(&self) -> u64 {
        self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).sum()
    }

    /// Samples that exceeded [`MAX_NS`] and were clamped to the last bucket.
    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    /// Copies the raw bucket counts (control plane only; allocates).
    pub fn buckets(&self) -> Vec<u64> {
        self.buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect()
    }

    /// Returns the sample at `p` (0.0..=1.0) expressed in nanoseconds.
    ///
    /// Returns `0` when nothing has been recorded. Reported values are the
    /// lower bound of the bucket the percentile lands in, so they are at most
    /// one bucket-width below the true value.
    pub fn percentile_ns(&self, p: f64) -> u64 {
        let total = self.count();
        if total == 0 {
            return 0;
        }
        let target = ((p.clamp(0.0, 1.0)) * total as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (index, bucket) in self.buckets.iter().enumerate() {
            seen += bucket.load(Ordering::Relaxed);
            if seen >= target {
                return Self::bucket_start_ns(index);
            }
        }
        MAX_NS
    }
}

impl Default for AtomicHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AtomicHistogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicHistogram")
            .field("count", &self.count())
            .field("overflows", &self.overflows())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_reports_zero() {
        let hist = AtomicHistogram::new();
        assert_eq!(hist.count(), 0);
        assert_eq!(hist.percentile_ns(0.5), 0);
    }

    #[test]
    fn fine_region_resolves_single_microseconds() {
        let hist = AtomicHistogram::new();
        hist.record_ns(700_000);
        hist.record_ns(700_400);
        assert_eq!(hist.percentile_ns(1.0), 700 * FINE_WIDTH_NS);
        assert_eq!(hist.percentile_ns(0.5), 700 * FINE_WIDTH_NS);
    }

    #[test]
    fn coarse_region_covers_a_full_quantum() {
        let hist = AtomicHistogram::new();
        // A 1024-frame / 48 kHz quantum is 21.33 ms.
        let quantum = 21_333_333;
        assert!(quantum < MAX_NS, "quantum must fit in the histogram");
        for _ in 0..100 {
            hist.record_ns(quantum);
        }
        let p50 = hist.percentile_ns(0.5);
        assert!(
            p50 <= quantum && quantum - p50 < COARSE_WIDTH_NS,
            "p50 {p50} should be within one bucket of {quantum}"
        );
    }

    #[test]
    fn samples_above_the_range_land_in_overflow() {
        let hist = AtomicHistogram::new();
        hist.record_ns(10_000_000_000);
        assert_eq!(hist.overflows(), 1);
        assert_eq!(hist.count(), 1);
        assert_eq!(hist.percentile_ns(1.0), MAX_NS);
    }

    #[test]
    fn bucket_layout_is_contiguous() {
        assert_eq!(FINE_BUCKETS + COARSE_BUCKETS + 1, BUCKET_COUNT);
        assert_eq!(FINE_END_NS, 2_048_000);
        assert_eq!(MAX_NS, 67_552_000);
        // Every index below overflow must map back to a valid range.
        for index in 0..BUCKET_COUNT - 1 {
            let start = AtomicHistogram::bucket_start_ns(index);
            assert_eq!(AtomicHistogram::index_for(start), index);
        }
    }
}
