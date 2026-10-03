//! KVPack: Binary sound pack format for KeyVibes.
//!
//! This crate provides:
//! - Binary format parsing and validation
//! - Memory-mapped pack loading
//! - Zero-copy sample access
//! - Pack building from audio files
//!
//! # RT Safety
//!
//! The runtime pack representation is designed for real-time audio:
//! - No allocations during lookup
//! - No locks during sample access
//! - No I/O during playback
//! - Immutable after validation

pub mod format;
pub mod header;
pub mod error;
pub mod parser;
pub mod validator;
pub mod loader;
pub mod lookup;
pub mod playback;

#[cfg(feature = "builder")]
pub mod builder;

pub use error::PackError;
pub use format::{MAGIC, FORMAT_VERSION, HEADER_SIZE};
pub use header::Header;
pub use loader::{KvPack, PackStats};
pub use lookup::{KeySounds, ClipInfo, VariantState};
pub use playback::PlayCommand;

// Re-export limits
pub use format::{
    MAX_KEYS, MAX_CLIPS, MAX_VARIANTS_PER_KEY, MAX_CLIP_FRAMES,
    MAX_METADATA_SIZE, MAX_STRING_SIZE, MAX_PACK_SIZE,
};
