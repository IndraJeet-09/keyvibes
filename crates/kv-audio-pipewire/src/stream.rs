//! PipeWire stream (stub for Phase 0).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("PipeWire error: {0}")]
    PipeWire(String),
}

pub struct PipeWireStream;

impl PipeWireStream {
    pub fn new(_sample_rate: u32) -> Result<Self, AudioError> {
        Ok(Self)
    }
}
