//! Idle power management for the output stream.
//!
//! # Why this looks the way it does
//!
//! Two failure modes have to be avoided at once:
//!
//! * **wasting work** - a stream that keeps the session manager scheduling
//!   audio callbacks while nothing is being pressed,
//! * **a lazy first key** - a stream that has to be torn down and rebuilt, or
//!   polled back to life, before the next keypress can be heard.
//!
//! The Linux/PipeWire answer is neither of the extremes. KeyVibes keeps the
//! client connection **open** the whole time (reconnecting would be the slow
//! wake-up the brief forbids) and only asks PipeWire to stop *processing* the
//! stream once it has been quiet for [`IdleConfig::idle_after`]. Resuming is
//! a single `pw_stream_set_active(true)` sent from the input thread on the
//! very same keypress, so there is no poll interval in the wake-up path.
//!
//! Everything here runs on a control thread. The real-time callback never
//! reads this state and is never asked to suspend or resume anything.

use crate::engine::AudioControl;
use crate::stream::RtStats;
use kv_core::{monotonic_ns, PlayCommand};
use kv_ring::SpscRing;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long the stream must be completely quiet before it is paused.
pub const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(5);

/// How often the monitor re-evaluates. Only matters for commands that did
/// not come from an input thread (which wakes the stream itself).
pub const DEFAULT_IDLE_POLL: Duration = Duration::from_millis(25);

/// Knobs for [`spawn_idle_monitor`].
#[derive(Debug, Clone, Copy)]
pub struct IdleConfig {
    /// Whether the stream is ever paused for being idle.
    pub enabled: bool,
    /// Quiet time required before the stream is paused.
    pub idle_after: Duration,
    /// Observation interval of the monitor thread.
    pub poll: Duration,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_after: DEFAULT_IDLE_AFTER,
            poll: DEFAULT_IDLE_POLL,
        }
    }
}

/// Where the stream is in its idle lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum IdlePhase {
    /// Producing audio (voices, commands, or both).
    #[default]
    Active = 0,
    /// Quiet, but the stream is still being processed.
    Idle = 1,
    /// Quiet long enough that PipeWire was told to stop processing us.
    Paused = 2,
}

impl IdlePhase {
    /// Wraps a raw discriminant.
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Active,
            1 => Self::Idle,
            2 => Self::Paused,
            _ => Self::Active,
        }
    }

    /// Human-readable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Paused => "paused",
        }
    }
}

/// Lock-free idle counters shared with the control plane and tests.
#[derive(Debug, Default)]
pub struct IdleState {
    phase: AtomicU8,
    pauses: AtomicU64,
    resumes: AtomicU64,
    evaluations: AtomicU64,
    /// Nanoseconds of the current continuous quiet stretch; 0 when active.
    quiet_ns: AtomicU64,
    longest_quiet_ns: AtomicU64,
    /// `monotonic_ns()` at which the current quiet stretch began; 0 if active.
    quiet_since: AtomicU64,
}

/// Plain snapshot of [`IdleState`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdleStats {
    /// Current phase.
    pub phase: IdlePhase,
    /// Times the stream was paused for idleness.
    pub pauses: u64,
    /// Times it was resumed (key press or new work).
    pub resumes: u64,
    /// Monitor evaluations since start.
    pub evaluations: u64,
    /// Length of the current quiet stretch (`0` when active).
    pub quiet: Duration,
    /// Longest quiet stretch observed.
    pub longest_quiet: Duration,
}

impl IdleState {
    /// Creates an idle state that starts out active.
    pub fn new() -> Self {
        Self {
            phase: AtomicU8::new(IdlePhase::Active as u8),
            ..Default::default()
        }
    }

    /// Current phase.
    pub fn phase(&self) -> IdlePhase {
        IdlePhase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    /// Whether the stream is currently paused for idleness.
    pub fn is_paused(&self) -> bool {
        self.phase() == IdlePhase::Paused
    }

    /// Whether the monitor has been quiet for at least `duration`.
    pub fn quiet_for(&self) -> Duration {
        Duration::from_nanos(self.quiet_ns.load(Ordering::Relaxed))
    }

    /// Current continuous quiet stretch.
    pub fn snapshot(&self) -> IdleStats {
        IdleStats {
            phase: self.phase(),
            pauses: self.pauses.load(Ordering::Relaxed),
            resumes: self.resumes.load(Ordering::Relaxed),
            evaluations: self.evaluations.load(Ordering::Relaxed),
            quiet: self.quiet_for(),
            longest_quiet: Duration::from_nanos(self.longest_quiet_ns.load(Ordering::Relaxed)),
        }
    }

    fn set_phase(&self, phase: IdlePhase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    fn note_pause(&self) {
        self.pauses.fetch_add(1, Ordering::Relaxed);
    }

    fn note_resume(&self) {
        self.resumes.fetch_add(1, Ordering::Relaxed);
    }

    /// Marks the stream active and ends the quiet stretch.
    fn mark_active(&self, resumed: bool) {
        if self.quiet_since.swap(0, Ordering::Relaxed) != 0 && resumed {
            self.note_resume();
        }
        self.quiet_ns.store(0, Ordering::Relaxed);
        self.set_phase(IdlePhase::Active);
    }

    /// Records a quiet observation of length `quiet`.
    fn mark_quiet(&self, quiet: Duration) {
        self.quiet_since.store(monotonic_ns(), Ordering::Relaxed);
        self.quiet_ns
            .store(quiet.as_nanos() as u64, Ordering::Relaxed);
        self.longest_quiet_ns
            .fetch_max(quiet.as_nanos() as u64, Ordering::Relaxed);
        self.evaluations.fetch_add(1, Ordering::Relaxed);
    }

    /// Marks the stream paused; the quiet stretch keeps counting.
    fn mark_paused(&self) {
        self.set_phase(IdlePhase::Paused);
        self.note_pause();
    }
}

/// Spawns the control-plane idle monitor.
///
/// The thread sleeps for `config.poll` between evaluations, so an idle
/// system costs one wakeup per interval and never spins. It stops as soon as
/// `stop` is set (checked before every sleep), so shutdown joins promptly.
pub fn spawn_idle_monitor(
    control: AudioControl,
    stats: Arc<RtStats>,
    queue: Arc<SpscRing<PlayCommand>>,
    state: Arc<IdleState>,
    config: IdleConfig,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("keyvibes-idle".to_string())
        .spawn(move || {
            let mut quiet_since: Option<Instant> = None;

            while !stop.load(Ordering::Relaxed) {
                thread::sleep(config.poll);
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                let snapshot = stats.snapshot();
                let silent = snapshot.active_voices == 0 && queue.is_empty();
                let stream_up = control.is_active();

                // A resume can come from the input thread, which brings the
                // stream up itself and may have finished playing before this
                // poll. Notice the disagreement between what we last
                // recorded and what the engine wants, so `resumes` and the
                // phase stay truthful instead of waiting for new work.
                if state.is_paused() && stream_up {
                    state.mark_active(true);
                    quiet_since = None;
                }

                if !silent {
                    quiet_since = None;
                    if !stream_up {
                        // Work is queued but the stream is down: bring it
                        // back without waiting for another key.
                        control.set_active(true);
                    }
                    state.mark_active(state.is_paused());
                    continue;
                }

                let since = *quiet_since.get_or_insert_with(Instant::now);
                let quiet = since.elapsed();
                state.mark_quiet(quiet);

                if config.enabled && stream_up && quiet >= config.idle_after {
                    control.set_active(false);
                    state.mark_paused();
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_phase_round_trips() {
        for phase in [IdlePhase::Active, IdlePhase::Idle, IdlePhase::Paused] {
            assert_eq!(IdlePhase::from_u8(phase as u8), phase);
        }
        assert_eq!(IdlePhase::from_u8(200), IdlePhase::Active);
    }

    #[test]
    fn idle_state_starts_active_and_counts_transitions() {
        let state = IdleState::new();
        assert_eq!(state.phase(), IdlePhase::Active);

        state.mark_quiet(Duration::from_millis(10));
        assert_eq!(state.phase(), IdlePhase::Active);
        assert_eq!(state.quiet_for(), Duration::from_millis(10));

        state.mark_paused();
        assert!(state.is_paused());
        assert_eq!(state.snapshot().pauses, 1);

        state.mark_active(true);
        assert_eq!(state.phase(), IdlePhase::Active);
        assert_eq!(state.snapshot().resumes, 1);
        assert_eq!(state.quiet_for(), Duration::ZERO);
    }

    #[test]
    fn longest_quiet_is_retained() {
        let state = IdleState::new();
        state.mark_quiet(Duration::from_millis(5));
        state.mark_active(false);
        state.mark_quiet(Duration::from_millis(50));
        assert_eq!(state.snapshot().longest_quiet, Duration::from_millis(50));
        state.mark_active(false);
        state.mark_quiet(Duration::from_millis(1));
        assert_eq!(
            state.snapshot().longest_quiet,
            Duration::from_millis(50),
            "the peak must not be forgotten"
        );
    }

    #[test]
    fn monitor_config_defaults_are_conservative() {
        let config = IdleConfig::default();
        assert!(config.enabled);
        assert!(config.idle_after >= Duration::from_secs(1));
        assert!(config.poll >= Duration::from_millis(1));
    }
}
