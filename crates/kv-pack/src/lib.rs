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

pub mod error;
pub mod format;
pub mod header;
pub mod loader;
pub mod lookup;
pub mod parser;
pub mod playback;
pub mod validator;

#[cfg(feature = "builder")]
pub mod builder;
#[cfg(feature = "builder")]
pub mod layout;
#[cfg(feature = "builder")]
pub mod manifest;
#[cfg(feature = "builder")]
pub mod wav;
#[cfg(feature = "builder")]
pub mod writer;

pub use error::PackError;
pub use format::{FORMAT_VERSION, HEADER_SIZE, MAGIC};
pub use header::Header;
pub use loader::{KvPack, PackStats};
pub use lookup::{ClipInfo, KeySounds, VariantState};
pub use playback::PlayCommand;

#[cfg(feature = "builder")]
pub use builder::{build_from_manifest, BuildEvent, BuildReport, ClipData, PackBuilder};
#[cfg(feature = "builder")]
pub use manifest::PackManifest;
#[cfg(feature = "builder")]
pub use writer::{PackPlan, WriteStage};

// Re-export limits
pub use format::{
    MAX_CLIPS, MAX_CLIP_FRAMES, MAX_KEYS, MAX_METADATA_SIZE, MAX_PACK_SIZE, MAX_STRING_SIZE,
    MAX_VARIANTS_PER_KEY,
};
