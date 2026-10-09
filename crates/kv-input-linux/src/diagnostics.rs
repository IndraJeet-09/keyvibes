//! Diagnostics and statistics for input backend.

use kv_core::AtomicHistogram;
use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free statistics for input events.
#[derive(Debug)]
pub struct InputStats {
    pub key_presses: AtomicU64,
    pub key_releases: AtomicU64,
    pub repeats_ignored: AtomicU64,
    pub unknown_keys: AtomicU64,
    pub sync_dropped: AtomicU64,
    pub commands_generated: AtomicU64,
    pub commands_dropped: AtomicU64,
    pub devices_added: AtomicU64,
    pub devices_removed: AtomicU64,
    /// Recoverable device read errors (not yet fatal for that device).
    pub read_errors: AtomicU64,

    /// Gauge: keys currently held across every attached keyboard.
    keys_held: AtomicU64,

    /// Software latency from event receipt to command enqueue.
    command_latency_hist: AtomicHistogram,
    command_latency_total_ns: AtomicU64,
    command_latency_max_ns: AtomicU64,
}

impl InputStats {
    pub fn new() -> Self {
        Self {
            key_presses: AtomicU64::new(0),
            key_releases: AtomicU64::new(0),
            repeats_ignored: AtomicU64::new(0),
            unknown_keys: AtomicU64::new(0),
            sync_dropped: AtomicU64::new(0),
            commands_generated: AtomicU64::new(0),
            commands_dropped: AtomicU64::new(0),
            devices_added: AtomicU64::new(0),
            devices_removed: AtomicU64::new(0),
            read_errors: AtomicU64::new(0),
            keys_held: AtomicU64::new(0),
            command_latency_hist: AtomicHistogram::new(),
            command_latency_total_ns: AtomicU64::new(0),
            command_latency_max_ns: AtomicU64::new(0),
        }
    }

    pub fn increment_press(&self) {
        self.key_presses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_release(&self) {
        self.key_releases.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_repeat_ignored(&self) {
        self.repeats_ignored.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_unknown(&self) {
        self.unknown_keys.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a `SYN_DROPPED` marker: the kernel says our view of one
    /// keyboard is stale.
    pub fn increment_sync_dropped(&self) {
        self.sync_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a read error that the pipeline decided to retry.
    pub fn increment_read_error(&self) {
        self.read_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Publishes the number of keys currently held (a gauge, not a counter).
    pub fn set_keys_held(&self, held: u64) {
        self.keys_held.store(held, Ordering::Relaxed);
    }

    pub fn increment_command_generated(&self) {
        self.commands_generated.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_command_dropped(&self) {
        self.commands_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_device_added(&self) {
        self.devices_added.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_device_removed(&self) {
        self.devices_removed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records how long turning one input event into a queued command took.
    ///
    /// One atomic increment plus one histogram bucket: safe from the reader
    /// thread, which is not the RT thread, but kept allocation-free anyway.
    pub fn record_command_latency(&self, ns: u64) {
        self.command_latency_hist.record_ns(ns);
        self.command_latency_total_ns
            .fetch_add(ns, Ordering::Relaxed);
        self.command_latency_max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Gets a snapshot of current statistics.
    pub fn snapshot(&self) -> InputStatsSnapshot {
        InputStatsSnapshot {
            command_latency_samples: self.command_latency_hist.count(),
            command_latency_p50_ns: self.command_latency_hist.percentile_ns(0.50),
            command_latency_p95_ns: self.command_latency_hist.percentile_ns(0.95),
            command_latency_p99_ns: self.command_latency_hist.percentile_ns(0.99),
            command_latency_max_ns: self.command_latency_max_ns.load(Ordering::Relaxed),
            key_presses: self.key_presses.load(Ordering::Relaxed),
            key_releases: self.key_releases.load(Ordering::Relaxed),
            repeats_ignored: self.repeats_ignored.load(Ordering::Relaxed),
            unknown_keys: self.unknown_keys.load(Ordering::Relaxed),
            sync_dropped: self.sync_dropped.load(Ordering::Relaxed),
            commands_generated: self.commands_generated.load(Ordering::Relaxed),
            commands_dropped: self.commands_dropped.load(Ordering::Relaxed),
            devices_added: self.devices_added.load(Ordering::Relaxed),
            devices_removed: self.devices_removed.load(Ordering::Relaxed),
            read_errors: self.read_errors.load(Ordering::Relaxed),
            keys_held: self.keys_held.load(Ordering::Relaxed),
        }
    }
}

impl Default for InputStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of input statistics at a point in time.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputStatsSnapshot {
    /// Samples recorded in the input-to-command histogram.
    pub command_latency_samples: u64,
    /// Software input-to-command latency percentiles (nanoseconds).
    pub command_latency_p50_ns: u64,
    pub command_latency_p95_ns: u64,
    pub command_latency_p99_ns: u64,
    pub command_latency_max_ns: u64,
    pub key_presses: u64,
    pub key_releases: u64,
    pub repeats_ignored: u64,
    pub unknown_keys: u64,
    pub sync_dropped: u64,
    pub commands_generated: u64,
    pub commands_dropped: u64,
    pub devices_added: u64,
    pub devices_removed: u64,
    /// Read errors that were retried rather than treated as removals.
    pub read_errors: u64,
    /// Keys currently held across every attached keyboard.
    pub keys_held: u64,
}

impl InputStatsSnapshot {
    /// Formats the statistics for display.
    pub fn format(&self) -> String {
        format!(
            "Input Statistics:\n\
             Input to command: p50 {}ns p95 {}ns p99 {}ns max {}ns ({} samples)\n\
             Presses: {}\n\
             Releases: {}\n\
             Repeats ignored: {}\n\
             Unknown keys: {}\n\
             SYN_DROPPED: {}\n\
             Commands generated: {}\n\
             Commands dropped: {}\n\
             Devices added: {}\n\
             Devices removed: {}\n\
             Read errors (retried): {}\n\
             Keys currently held: {}",
            self.command_latency_p50_ns,
            self.command_latency_p95_ns,
            self.command_latency_p99_ns,
            self.command_latency_max_ns,
            self.command_latency_samples,
            self.key_presses,
            self.key_releases,
            self.repeats_ignored,
            self.unknown_keys,
            self.sync_dropped,
            self.commands_generated,
            self.commands_dropped,
            self.devices_added,
            self.devices_removed,
            self.read_errors,
            self.keys_held,
        )
    }
}
