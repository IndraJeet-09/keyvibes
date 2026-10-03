//! Audio stream lifecycle management.
//!
//! Manages connection, disconnection, reconnection, and error recovery
//! for the PipeWire stream. All lifecycle operations occur on the control
//! thread (not the real-time audio thread).

use crate::stream::{PipeWireStream, AudioError};

/// Manages stream lifecycle and diagnostics.
pub struct LifecycleManager {
    /// Reference to the stream.
    stream_ref: Option<crate::stream::PipeWireStream>,
    /// Connection state.
    connected: bool,
    /// Retry count for reconnections.
    retry_count: u32,
}

impl LifecycleManager {
    /// Creates a new lifecycle manager.
    pub fn new() -> Self {
        Self {
            stream_ref: None,
            connected: false,
            retry_count: 0,
        }
    }

    /// Attempts to connect with retry logic.
    pub fn connect_with_retry(
        mut self,
        max_retries: u32,
        delay_ms: u64,
    ) -> Result<Self, AudioError> {
        for attempt in 0..=max_retries {
            match self.try_connect() {
                Ok(_) => {
                    self.retry_count = 0;
                    return Ok(self);
                }
                Err(e) if attempt == max_retries => {
                    return Err(e);
                }
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    self.retry_count += 1;
                }
            }
        }
        unreachable!()
    }

    /// Internal connection attempt.
    fn try_connect(&mut self) -> Result<(), AudioError> {
        // This would connect the stream; stubbed for Phase 2
        self.connected = true;
        Ok(())
    }

    /// Checks if the stream is healthy.
    pub fn is_healthy(&self) -> bool {
        self.connected && self.retry_count < 10
    }
}
