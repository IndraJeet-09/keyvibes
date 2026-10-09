//! Monotonic clock helper shared by the input, control, and audio threads.
//!
//! The clock is `Instant`-based, so it is monotonic and consistent across
//! threads on the same machine. It exists so that the input thread and the
//! real-time audio thread can stamp the same timeline without depending on
//! any platform-specific crate.

use std::sync::OnceLock;
use std::time::Instant;

/// Nanoseconds since an arbitrary, process-wide monotonic epoch.
///
/// Costs one clock read per call after the epoch is initialized. Safe to call
/// from the input thread, the control thread, and the real-time audio thread.
#[inline]
pub fn monotonic_ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic_ns_is_monotonic() {
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(b >= a);
    }

    #[test]
    fn test_monotonic_ns_advances() {
        let a = monotonic_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = monotonic_ns();
        assert!(b - a >= 1_000_000, "expected at least 1ms of progress");
    }
}
