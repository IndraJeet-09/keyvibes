//! Pack loader with memory mapping.

use crate::format::{PackHeader, Clip};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid pack format: {0}")]
    InvalidFormat(&'static str),
}

/// Pack loader (stub for Phase 0).
pub struct PackLoader;

impl PackLoader {
    pub fn load(_path: &Path) -> Result<Self, LoadError> {
        Ok(Self)
    }
}
