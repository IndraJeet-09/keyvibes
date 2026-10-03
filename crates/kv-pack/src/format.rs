//! Pack format definitions.

use kv_core::PhysicalKey;

/// Magic bytes for KeyVibes pack format: "KVPK"
pub const MAGIC: [u8; 4] = *b"KVPK";

/// Current pack format version.
pub const VERSION: u32 = 1;

/// Pack file header.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PackHeader {
    /// Magic bytes: "KVPK"
    pub magic: [u8; 4],

    /// Format version
    pub version: u32,

    /// Sample rate of all clips in this pack
    pub sample_rate: u32,

    /// Total number of clips
    pub clip_count: u32,

    /// Offset to clip table
    pub clip_table_offset: u64,

    /// Offset to PCM data
    pub pcm_data_offset: u64,

    /// Total size of PCM data in bytes
    pub pcm_data_size: u64,
}

/// A reference to a range of clip variants for a key.
#[derive(Debug, Copy, Clone)]
pub struct ClipRange {
    /// Index of first clip
    pub start: u32,

    /// Number of clips (variants)
    pub count: u32,
}

/// Metadata for one audio clip.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Clip {
    /// Offset within PCM data (in samples, not bytes)
    pub offset: u32,

    /// Length in samples (mono)
    pub length: u32,
}

impl PackHeader {
    /// Validates the header.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.magic != MAGIC {
            return Err("Invalid magic bytes");
        }

        if self.version != VERSION {
            return Err("Unsupported pack version");
        }

        if self.sample_rate == 0 || self.sample_rate > 192000 {
            return Err("Invalid sample rate");
        }

        Ok(())
    }
}
