//! KeyVibes runtime coordination.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Runtime error: {0}")]
    General(String),
}

pub struct Runtime;

impl Runtime {
    pub fn new() -> Result<Self, RuntimeError> {
        Ok(Self)
    }
}
