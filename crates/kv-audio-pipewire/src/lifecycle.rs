//! Audio stream lifecycle management.
//!
//! Reconnection policy and state tracking for the PipeWire stream. All
//! lifecycle work happens on a control thread: the real-time callback never
//! reconnects, sleeps, or performs I/O.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// Bounded exponential backoff for stream reconnection.
///
/// The policy is deliberately conservative: audio failures are usually
/// transient (session manager restart, device reconfiguration), and a tight
/// retry loop would fight the session manager while it recovers.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectPolicy {
    /// Attempts before giving up. `0` means "never retry".
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub initial_delay: Duration,
    /// Upper bound for any single delay.
    pub max_delay: Duration,
    /// Multiplier applied after each failed attempt.
    pub factor: f32,
}

impl ReconnectPolicy {
    /// Creates a policy with explicit bounds.
    pub fn new(max_attempts: u32, initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts,
            initial_delay,
            max_delay,
            factor: 2.0,
        }
    }

    /// Delay to sleep before retry number `attempt` (0-based).
    ///
    /// Computed in integer nanoseconds with rounding so that repeated
    /// multiplication never drifts (for example `0.8f32` seconds would
    /// otherwise round-trip to 800_000_011 ns instead of 800_000_000 ns).
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let initial = self.initial_delay.as_nanos() as u64;
        let max = self.max_delay.as_nanos() as u64;
        let mut delay = initial;
        for _ in 0..attempt {
            delay = (delay as f64 * self.factor as f64).round() as u64;
            if delay >= max {
                return Duration::from_nanos(max);
            }
        }
        Duration::from_nanos(delay.min(max))
    }

    /// Whether another attempt is allowed after `attempt` failures.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_attempts
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new(5, Duration::from_millis(200), Duration::from_secs(5))
    }
}

/// The supervisor's reconnect decision logic, extracted so it can be driven
/// deterministically without a PipeWire connection.
///
/// This **is** the state machine [`crate::engine`]'s supervisor runs: the
/// supervisor only supplies "the attempt failed" / "the attempt succeeded"
/// and obeys the returned delay. Keeping it free of I/O is what lets
/// `keyvibes audio-recovery-test` exercise a PipeWire restart lifecycle
/// without restarting anything.
///
/// Invariants it guarantees:
///
/// * the very first attempt is never delayed,
/// * every later attempt waits the bounded exponential backoff from
///   [`ReconnectPolicy`] - never a busy loop,
/// * a success resets the failure count, so recovery starts fresh,
/// * at most `policy.max_attempts` consecutive failures are tolerated.
#[derive(Debug)]
pub struct ReconnectStateMachine {
    policy: ReconnectPolicy,
    /// Total attempts made so far (successful or not).
    attempts: u32,
    /// Consecutive failures since the last success.
    failures: u32,
    /// Whether a connection has ever been established.
    established: bool,
    /// Whether retries are permanently stopped.
    exhausted: bool,
}

impl ReconnectStateMachine {
    /// Creates a machine for `policy` that has not attempted anything yet.
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            attempts: 0,
            failures: 0,
            established: false,
            exhausted: false,
        }
    }

    /// The policy this machine applies.
    pub fn policy(&self) -> ReconnectPolicy {
        self.policy
    }

    /// Delay to wait before the next connection attempt.
    ///
    /// Zero only for the very first attempt; every later attempt waits at
    /// least `policy.initial_delay`, so a dead session manager can never be
    /// hammered.
    pub fn backoff(&self) -> Duration {
        if self.attempts == 0 {
            return Duration::ZERO;
        }
        self.policy.delay_for(self.failures.saturating_sub(1))
    }

    /// Records a failed connection attempt.
    ///
    /// Returns `true` when another attempt is allowed, `false` once the
    /// budget is spent (the machine then reports itself exhausted forever).
    pub fn record_failure(&mut self) -> bool {
        self.attempts = self.attempts.saturating_add(1);
        self.failures = self.failures.saturating_add(1);
        if self.exhausted || !self.policy.should_retry(self.failures) {
            self.exhausted = true;
            return false;
        }
        true
    }

    /// Records a connection that came up (or came back).
    ///
    /// Resets the failure budget so a later outage starts over at the short
    /// delay instead of jumping straight to the cap.
    pub fn record_success(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        self.failures = 0;
        self.established = true;
        self.exhausted = false;
    }

    /// Total attempts made so far.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Consecutive failures since the last success.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Whether a connection has ever succeeded.
    pub fn established(&self) -> bool {
        self.established
    }

    /// Whether the machine has given up.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;

    fn fast() -> ReconnectPolicy {
        ReconnectPolicy::new(3, Duration::from_millis(10), Duration::from_millis(80))
    }

    #[test]
    fn first_attempt_is_never_delayed() {
        let machine = ReconnectStateMachine::new(fast());
        assert_eq!(machine.backoff(), Duration::ZERO);
        assert_eq!(machine.attempts(), 0);
    }

    #[test]
    fn backoff_grows_with_consecutive_failures() {
        let policy = ReconnectPolicy::new(6, Duration::from_millis(10), Duration::from_millis(80));
        let mut machine = ReconnectStateMachine::new(policy);
        machine.record_success();
        assert_eq!(machine.backoff(), Duration::from_millis(10));

        assert!(machine.record_failure());
        assert_eq!(machine.backoff(), Duration::from_millis(10));
        assert!(machine.record_failure());
        assert_eq!(machine.backoff(), Duration::from_millis(20));
        assert!(machine.record_failure());
        assert_eq!(machine.backoff(), Duration::from_millis(40));
        assert!(machine.record_failure());
        assert_eq!(
            machine.backoff(),
            Duration::from_millis(80),
            "the cap is never exceeded"
        );
    }

    #[test]
    fn backoff_is_always_bounded() {
        let policy = ReconnectPolicy::new(
            u32::MAX,
            Duration::from_millis(10),
            Duration::from_millis(50),
        );
        let mut machine = ReconnectStateMachine::new(policy);
        machine.record_success();
        for _ in 0..50 {
            assert!(machine.record_failure());
            assert!(machine.backoff() <= Duration::from_millis(50));
        }
    }

    #[test]
    fn retries_stop_after_the_budget_is_spent() {
        let mut machine = ReconnectStateMachine::new(fast());
        machine.record_success();

        assert!(machine.record_failure());
        assert!(machine.record_failure());
        assert!(
            !machine.record_failure(),
            "third failure exhausts max_attempts=3"
        );
        assert!(machine.is_exhausted());
        assert!(
            !machine.record_failure(),
            "an exhausted machine stays exhausted"
        );
    }

    #[test]
    fn success_resets_the_failure_budget() {
        let mut machine = ReconnectStateMachine::new(fast());
        machine.record_success();
        assert!(machine.record_failure());
        assert!(machine.record_failure());
        assert!(!machine.record_failure());
        assert!(machine.is_exhausted());

        // A session manager that comes back on its own (or a forced
        // reconnect) must be allowed to run again.
        machine.record_success();
        assert!(!machine.is_exhausted());
        assert_eq!(machine.failures(), 0);
        assert!(machine.record_failure());
    }

    #[test]
    fn never_busy_loops() {
        let mut machine = ReconnectStateMachine::new(fast());
        machine.record_success();
        let mut waited = Duration::ZERO;
        for _ in 0..20 {
            let delay = machine.backoff();
            assert!(delay >= Duration::from_millis(10) || machine.attempts() == 0);
            waited += delay;
            if !machine.record_failure() {
                break;
            }
        }
        assert!(
            waited >= Duration::from_millis(40),
            "backoff must actually wait"
        );
    }

    #[test]
    fn machine_tracks_established_state() {
        let machine = ReconnectStateMachine::new(fast());
        assert!(!machine.established());
        let mut machine = machine;
        machine.record_success();
        assert!(machine.established());
    }
}

/// Observable lifecycle phase of the audio runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPhase {
    /// No connection attempt has been made yet.
    Starting,
    /// A connection exists and the stream is producing audio.
    Running,
    /// The connection dropped and a bounded retry is pending.
    Reconnecting,
    /// Retries were exhausted; the runtime is inert until restarted.
    Failed,
    /// Clean shutdown was requested.
    Stopped,
}

/// Lock-free lifecycle state shared with the CLI and diagnostics.
#[derive(Debug, Default)]
pub struct LifecycleState {
    running: AtomicBool,
    failed: AtomicBool,
    attempts: AtomicU32,
}

impl LifecycleState {
    /// Creates a lifecycle in the `Starting` phase.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the stream as connected.
    pub fn mark_running(&self) {
        self.running.store(true, Ordering::Relaxed);
        self.failed.store(false, Ordering::Relaxed);
        self.attempts.store(0, Ordering::Relaxed);
    }

    /// Marks a failed attempt.
    pub fn mark_failed(&self, attempt: u32) {
        self.running.store(false, Ordering::Relaxed);
        self.attempts.store(attempt, Ordering::Relaxed);
    }

    /// Marks permanent failure.
    pub fn mark_exhausted(&self) {
        self.running.store(false, Ordering::Relaxed);
        self.failed.store(true, Ordering::Relaxed);
    }

    /// Whether audio is currently flowing.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Whether the engine gave up.
    pub fn is_exhausted(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Consecutive failed attempts since the last success.
    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_grows_and_caps() {
        let policy = ReconnectPolicy::new(10, Duration::from_millis(100), Duration::from_secs(1));
        assert_eq!(policy.delay_for(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for(3), Duration::from_millis(800));
        assert_eq!(policy.delay_for(4), Duration::from_secs(1));
        assert_eq!(policy.delay_for(9), Duration::from_secs(1));
    }

    #[test]
    fn test_backoff_never_exceeds_max() {
        let policy = ReconnectPolicy::new(3, Duration::from_millis(50), Duration::from_millis(120));
        for attempt in 0..100 {
            assert!(policy.delay_for(attempt) <= Duration::from_millis(120));
        }
    }

    #[test]
    fn test_should_retry_respects_max_attempts() {
        let policy = ReconnectPolicy::new(3, Duration::from_millis(1), Duration::from_millis(2));
        assert!(policy.should_retry(0));
        assert!(policy.should_retry(2));
        assert!(!policy.should_retry(3));
        assert!(!policy.should_retry(4));
    }

    #[test]
    fn test_lifecycle_state_transitions() {
        let state = LifecycleState::new();
        assert!(!state.is_running());
        state.mark_running();
        assert!(state.is_running());
        assert_eq!(state.attempts(), 0);
        state.mark_failed(2);
        assert!(!state.is_running());
        assert_eq!(state.attempts(), 2);
        state.mark_running();
        assert!(state.is_running());
        assert_eq!(state.attempts(), 0);
        state.mark_exhausted();
        assert!(state.is_exhausted());
    }
}
