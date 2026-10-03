//! Diagnostics and metrics.

use kv_input_linux::diagnostics::InputStatsSnapshot;

/// Aggregated input-path diagnostics for a session.
#[derive(Debug, Default, Clone, Copy)]
pub struct Diagnostics {
    /// Key events received (presses and releases).
    pub events_received: u64,
    /// Play commands successfully queued for the mixer.
    pub plays_triggered: u64,
    /// Play commands dropped because the command queue was full.
    pub commands_dropped: u64,
}

impl Diagnostics {
    /// Builds a diagnostics record from an input statistics snapshot.
    ///
    /// Snapshot counters are absolute for the session, so this always reflects
    /// the latest known values.
    pub fn from_stats(stats: &InputStatsSnapshot) -> Self {
        Self {
            events_received: stats.key_presses + stats.key_releases,
            plays_triggered: stats.commands_generated,
            commands_dropped: stats.commands_dropped,
        }
    }

    /// Renders the diagnostics as a human-readable report.
    pub fn report(&self) -> String {
        format!(
            "  Input:       {} events received\n  \
             Playback:    {} commands triggered\n  \
             Dropped:     {} commands dropped",
            self.events_received, self.plays_triggered, self.commands_dropped
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostics_empty_by_default() {
        let d = Diagnostics::default();
        assert_eq!(d.events_received, 0);
        assert_eq!(d.plays_triggered, 0);
        assert_eq!(d.commands_dropped, 0);
    }

    #[test]
    fn test_diagnostics_from_stats() {
        let stats = InputStatsSnapshot {
            key_presses: 10,
            key_releases: 8,
            repeats_ignored: 2,
            unknown_keys: 1,
            sync_dropped: 0,
            commands_generated: 7,
            commands_dropped: 3,
            devices_added: 1,
            devices_removed: 0,
        };

        let d = Diagnostics::from_stats(&stats);
        assert_eq!(d.events_received, 18);
        assert_eq!(d.plays_triggered, 7);
        assert_eq!(d.commands_dropped, 3);
        assert!(d.report().contains("18 events received"));
    }
}
