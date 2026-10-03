//! Sound pack format and loading.
//!
//! Defines the KeyVibes pack format (.kvpack) and provides memory-mapped loading.

pub mod format;
pub mod loader;

pub use format::{PackHeader, ClipRange, Clip};
pub use loader::PackLoader;
