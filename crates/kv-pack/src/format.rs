//! KVPack format constants and limits.

/// Magic number: "KVPACK\0\0"
pub const MAGIC: [u8; 8] = [0x4B, 0x56, 0x50, 0x41, 0x43, 0x4B, 0x00, 0x00];

/// Format version (major)
pub const FORMAT_VERSION: u16 = 1;

/// Header size in bytes
pub const HEADER_SIZE: u32 = 120;

/// Sample data alignment (bytes)
pub const SAMPLE_ALIGNMENT: u64 = 16;

/// Sample format: i16 little-endian
pub const SAMPLE_FORMAT_I16LE: u16 = 0;

/// Required guard samples before clip
pub const GUARD_BEFORE: u32 = 2;

/// Required guard samples after clip
pub const GUARD_AFTER: u32 = 3;

/// Maximum number of keys (PhysicalKey::COUNT)
pub const MAX_KEYS: u32 = 104;

/// Maximum total clips across all keys
pub const MAX_CLIPS: u32 = 10_000;

/// Maximum variants per individual key
pub const MAX_VARIANTS_PER_KEY: u16 = 16;

/// Maximum clip length in frames (~3 minutes at 48kHz)
pub const MAX_CLIP_FRAMES: u32 = 10_000_000;

/// Maximum metadata section size (64 KB)
pub const MAX_METADATA_SIZE: u32 = 65_536;

/// Maximum individual string size (4 KB)
pub const MAX_STRING_SIZE: u32 = 4_096;

/// Maximum pack file size (2 GB)
pub const MAX_PACK_SIZE: u64 = 2_147_483_648;

/// Minimum valid sample rate
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Maximum valid sample rate
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// Size of key entry in bytes
pub const KEY_ENTRY_SIZE: usize = 8;

/// Size of clip entry in bytes
pub const CLIP_ENTRY_SIZE: usize = 24;
