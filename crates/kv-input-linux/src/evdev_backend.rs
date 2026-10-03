//! Evdev backend (stub for Phase 0).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum InputError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct EvdevBackend;

impl EvdevBackend {
    pub fn new() -> Result<Self, InputError> {
        Ok(Self)
    }
}
