//! Diagnostics and metrics.

pub struct Diagnostics {
    pub events_received: u64,
    pub plays_triggered: u64,
    pub commands_dropped: u64,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self {
            events_received: 0,
            plays_triggered: 0,
            commands_dropped: 0,
        }
    }
}
