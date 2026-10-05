//! Error types for pack operations.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum PackError {
    #[error("Invalid magic number")]
    InvalidMagic,

    #[error("Unsupported format version: {0}")]
    UnsupportedVersion(u16),

    #[error("File truncated: expected {expected} bytes, got {actual}")]
    FileTruncated { expected: u64, actual: u64 },

    #[error("Invalid header size: {0}")]
    InvalidHeaderSize(u32),

    #[error("File size mismatch: header claims {header}, actual {actual}")]
    FileSizeMismatch { header: u64, actual: u64 },

    #[error("Integer overflow in offset calculation")]
    IntegerOverflow,

    #[error("Invalid offset: {field} = {offset}, exceeds file size {file_size}")]
    InvalidOffset {
        field: &'static str,
        offset: u64,
        file_size: u64,
    },

    #[error("Invalid length: {field} = {length}")]
    InvalidLength { field: &'static str, length: u64 },

    #[error("Invalid alignment: {field} = {offset}, must be aligned to {alignment}")]
    InvalidAlignment {
        field: &'static str,
        offset: u64,
        alignment: u64,
    },

    #[error("Too many keys: {0} (max {1})")]
    TooManyKeys(u32, u32),

    #[error("Too many clips: {0} (max {1})")]
    TooManyClips(u32, u32),

    #[error("Invalid key ID: {0} (max {1})")]
    InvalidKeyId(u16, u16),

    #[error("Invalid variant count: key {key} has {count} variants (max {max})")]
    InvalidVariantCount { key: u16, count: u16, max: u16 },

    #[error(
        "Invalid clip index: key {key} first_clip {first_clip} exceeds clip_count {clip_count}"
    )]
    InvalidClipIndex {
        key: u16,
        first_clip: u32,
        clip_count: u32,
    },

    #[error("Clip range overflow: key {key} first_clip {first_clip} + variant_count {variant_count} exceeds clip_count {clip_count}")]
    ClipRangeOverflow {
        key: u16,
        first_clip: u32,
        variant_count: u16,
        clip_count: u32,
    },

    #[error("Invalid sample rate: {0} (must be {1}..{2})")]
    InvalidSampleRate(u32, u32, u32),

    #[error("Invalid channel count: {0} (must be 1)")]
    InvalidChannelCount(u16),

    #[error("Invalid sample format: {0} (must be 0 for i16 LE)")]
    InvalidSampleFormat(u16),

    #[error("Invalid guard samples: clip {clip} has before={before}, after={after} (expected {expected_before}, {expected_after})")]
    InvalidGuardSamples {
        clip: u32,
        before: u32,
        after: u32,
        expected_before: u32,
        expected_after: u32,
    },

    #[error("Stored frames mismatch: clip {clip} stored={stored}, expected={expected}")]
    StoredFramesMismatch {
        clip: u32,
        stored: u32,
        expected: u32,
    },

    #[error("Clip sample offset out of bounds: clip {clip} offset {offset} exceeds sample region size {size}")]
    ClipOffsetOutOfBounds { clip: u32, offset: u64, size: u64 },

    #[error("Clip sample length exceeds region: clip {clip} offset {offset} + length {length} exceeds sample region size {size}")]
    ClipLengthExceeds {
        clip: u32,
        offset: u64,
        length: u64,
        size: u64,
    },

    #[error("Zero-length clip: clip {0}")]
    ZeroLengthClip(u32),

    #[error("Clip too large: clip {clip} has {frames} frames (max {max})")]
    ClipTooLarge { clip: u32, frames: u32, max: u32 },

    #[error("Duplicate key: {0}")]
    DuplicateKey(u16),

    #[error("Keys not sorted: key {0} after key {1}")]
    KeysNotSorted(u16, u16),

    #[error("Invalid metadata size: {0} (max {1})")]
    InvalidMetadataSize(u32, u32),

    #[error("Invalid string length: {0} (max {1})")]
    InvalidStringLength(u32, u32),

    #[error("Invalid UTF-8 in metadata")]
    InvalidUtf8,

    #[error("Metadata overflow: offset {offset} + length {length} exceeds metadata size {size}")]
    MetadataOverflow { offset: u32, length: u32, size: u32 },

    #[error("Reserved field not zero: {0}")]
    ReservedNotZero(&'static str),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Memory mapping failed: {0}")]
    MmapFailed(String),

    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(String),

    #[error("Manifest error: {0}")]
    ManifestError(String),

    #[error("Unknown physical key: {0}")]
    UnknownPhysicalKey(String),

    #[error("Duplicate PhysicalKey: {0}")]
    DuplicatePhysicalKey(String),

    #[error("Key {0} has no samples; a key entry must contain at least one clip")]
    EmptyKeySamples(String),

    #[error("Source path escapes pack root: {0}")]
    PathTraversal(String),

    #[error("Source file not found: {0}")]
    SourceNotFound(String),

    #[error("Sample rate mismatch in {path}: expected {expected} Hz, got {actual} Hz")]
    SampleRateMismatch {
        path: String,
        expected: u32,
        actual: u32,
    },

    #[error("Non-finite sample value: {0}")]
    NonFiniteSample(String),

    #[error("Truncated audio data: expected {expected} frames, got {actual}")]
    TruncatedAudio { expected: u64, actual: u64 },

    #[error("Pack contains no keys")]
    EmptyPack,

    #[error("Pack validation failed: {0}")]
    ValidationFailed(String),

    #[error("Key: {key}\nFile: {file}\n\nReason:\n{reason}")]
    BuildContext {
        key: String,
        file: String,
        reason: String,
    },

    #[error(
        "Unusable signal: peak {peak_dbfs:.1} dBFS is below the minimum {minimum_dbfs:.1} dBFS.\n\
         The source is silence or pure DC once the DC offset is removed."
    )]
    UnusableSignal { peak_dbfs: f32, minimum_dbfs: f32 },

    #[error("Failed to process {file}:\n{reason}")]
    SourceFailed { file: String, reason: String },

    #[cfg(feature = "builder")]
    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),
}

pub type PackResult<T> = Result<T, PackError>;
