//! Diagnostics and statistics for input backend.

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

    pub fn increment_dropped(&self) {
        self.sync_dropped.fetch_add(1, Ordering::Relaxed);
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

    /// Gets a snapshot of current statistics.
    pub fn snapshot(&self) -> InputStatsSnapshot {
        InputStatsSnapshot {
            key_presses: self.key_presses.load(Ordering::Relaxed),
            key_releases: self.key_releases.load(Ordering::Relaxed),
            repeats_ignored: self.repeats_ignored.load(Ordering::Relaxed),
            unknown_keys: self.unknown_keys.load(Ordering::Relaxed),
            sync_dropped: self.sync_dropped.load(Ordering::Relaxed),
            commands_generated: self.commands_generated.load(Ordering::Relaxed),
            commands_dropped: self.commands_dropped.load(Ordering::Relaxed),
            devices_added: self.devices_added.load(Ordering::Relaxed),
            devices_removed: self.devices_removed.load(Ordering::Relaxed),
        }
    }
}

impl Default for InputStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of input statistics at a point in time.
#[derive(Debug, Clone, Copy)]
pub struct InputStatsSnapshot {
    pub key_presses: u64,
    pub key_releases: u64,
    pub repeats_ignored: u64,
    pub unknown_keys: u64,
    pub sync_dropped: u64,
    pub commands_generated: u64,
    pub commands_dropped: u64,
    pub devices_added: u64,
    pub devices_removed: u64,
}

impl InputStatsSnapshot {
    /// Formats the statistics for display.
    pub fn format(&self) -> String {
        format!(
            "Input Statistics:\n\
             Presses: {}\n\
             Releases: {}\n\
             Repeats ignored: {}\n\
             Unknown keys: {}\n\
             SYN_DROPPED: {}\n\
             Commands generated: {}\n\
             Commands dropped: {}\n\
             Devices added: {}\n\
             Devices removed: {}",
            self.key_presses,
            self.key_releases,
            self.repeats_ignored,
            self.unknown_keys,
            self.sync_dropped,
            self.commands_generated,
            self.commands_dropped,
            self.devices_added,
            self.devices_removed
        )
    }
}
