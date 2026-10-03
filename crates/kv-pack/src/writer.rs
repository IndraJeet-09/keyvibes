//! Deterministic `.kvpack` serialization.
//!
//! Given a [`PackPlan`], this module writes the file byte-for-byte in the
//! canonical layout:
//!
//! ```text
//! Header (120) → Metadata → Key table → Clip table → Padding → Sample data
//! ```
//!
//! Determinism rules:
//! - Keys are written in ascending `PhysicalKey` discriminant order.
//! - Variant order follows the plan (manifest order for manifest builds).
//! - Padding is always zero bytes.
//! - No timestamps, paths, or environment data are recorded.

use crate::builder::ClipData;
use crate::error::{PackError, PackResult};
use crate::format::*;
use crate::layout::PackLayout;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// A fully planned pack, ready for serialization.
#[derive(Debug, Clone)]
pub struct PackPlan<'a> {
    /// Byte layout (all offsets/sizes precomputed).
    pub layout: PackLayout,
    /// Pack-wide sample rate (all clips share it).
    pub sample_rate: u32,
    /// Number of key entries.
    pub key_count: u32,
    /// Number of clip entries.
    pub clip_count: u32,
    /// Encoded metadata section.
    pub metadata: Vec<u8>,
    /// Encoded key table (`key_count * 8` bytes).
    pub key_table: Vec<u8>,
    /// Encoded clip table (`clip_count * 24` bytes).
    pub clip_table: Vec<u8>,
    /// Clips in clip-table order.
    pub clips: Vec<&'a ClipData>,
}

/// Progress stages reported while writing a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteStage {
    /// Writing bytes to the temporary file.
    Writing,
    /// Re-opening the temporary file for self-validation.
    Validating,
    /// File committed to its final path.
    Committed,
}

/// Encodes the metadata section: five length-prefixed UTF-8 strings.
///
/// Order (fixed by the format): `name`, `author`, `description`, `license`,
/// `source`.
pub fn encode_metadata(
    name: &str,
    author: &str,
    description: &str,
    license: &str,
    source: &str,
) -> PackResult<Vec<u8>> {
    let mut buf = Vec::new();

    for value in [name, author, description, license, source] {
        let bytes = value.as_bytes();
        let len = bytes.len() as u32;

        if len > MAX_STRING_SIZE {
            return Err(PackError::InvalidStringLength(len, MAX_STRING_SIZE));
        }

        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(bytes);
    }

    if buf.len() as u32 > MAX_METADATA_SIZE {
        return Err(PackError::InvalidMetadataSize(
            buf.len() as u32,
            MAX_METADATA_SIZE,
        ));
    }

    Ok(buf)
}

/// Encodes the fixed-size 120-byte header.
fn encode_header(plan: &PackPlan<'_>) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_SIZE as usize);
    let layout = plan.layout;

    h.extend_from_slice(&MAGIC);
    h.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes()); // flags
    h.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    h.extend_from_slice(&layout.file_size.to_le_bytes());
    h.extend_from_slice(&plan.key_count.to_le_bytes());
    h.extend_from_slice(&plan.clip_count.to_le_bytes());
    h.extend_from_slice(&layout.metadata_offset.to_le_bytes());
    h.extend_from_slice(&layout.metadata_size.to_le_bytes());
    h.extend_from_slice(&layout.key_table_offset.to_le_bytes());
    h.extend_from_slice(&layout.key_table_size.to_le_bytes());
    h.extend_from_slice(&layout.clip_table_offset.to_le_bytes());
    h.extend_from_slice(&layout.clip_table_size.to_le_bytes());
    h.extend_from_slice(&layout.sample_data_offset.to_le_bytes());
    h.extend_from_slice(&layout.sample_data_size.to_le_bytes());
    h.extend_from_slice(&plan.sample_rate.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // channels = 1 (mono)
    h.extend_from_slice(&SAMPLE_FORMAT_I16LE.to_le_bytes());
    h.extend_from_slice(&[0u8; 28]); // reserved

    debug_assert_eq!(h.len(), HEADER_SIZE as usize);
    h
}

/// Serializes a complete plan to `w`, in layout order.
///
/// The number of bytes written always equals `plan.layout.file_size`.
pub fn write_pack<W: Write>(w: &mut W, plan: &PackPlan<'_>) -> PackResult<()> {
    let mut written: u64 = 0;

    let mut emit = |w: &mut W, bytes: &[u8]| -> PackResult<()> {
        w.write_all(bytes)?;
        written = written
            .checked_add(bytes.len() as u64)
            .ok_or(PackError::IntegerOverflow)?;
        Ok(())
    };

    emit(w, &encode_header(plan))?;
    emit(w, &plan.metadata)?;
    emit(w, &plan.key_table)?;
    emit(w, &plan.clip_table)?;

    if plan.layout.padding > 0 {
        let padding = vec![0u8; plan.layout.padding as usize];
        emit(w, &padding)?;
    }

    for clip in &plan.clips {
        let mut buf = Vec::with_capacity(clip.samples.len() * 2);
        for &sample in &clip.samples {
            buf.extend_from_slice(&sample.to_le_bytes());
        }
        emit(w, &buf)?;
    }

    if written != plan.layout.file_size {
        return Err(PackError::ValidationFailed(format!(
            "wrote {written} bytes, plan expects {}",
            plan.layout.file_size
        )));
    }

    w.flush()?;
    Ok(())
}

/// Temporary path used while building `path` (`<path>.tmp`).
fn temp_path(path: &Path) -> PathBuf {
    let mut os: OsString = path.as_os_str().to_owned();
    os.push(".tmp");
    PathBuf::from(os)
}

/// Writes a plan to `path` atomically.
///
/// Flow:
/// 1. Write everything to `<path>.tmp`.
/// 2. Flush and `fsync` the temporary file.
/// 3. Self-validate by opening the temporary file with [`crate::loader::KvPack`]
///    (the same code path used by the runtime).
/// 4. Rename onto the final path (atomic replace on POSIX).
///
/// On any failure the temporary file is removed and `path` is left untouched.
pub fn write_plan_atomic(
    path: &Path,
    plan: &PackPlan<'_>,
    mut on_stage: impl FnMut(WriteStage),
) -> PackResult<()> {
    let tmp = temp_path(path);

    let result = (|| -> PackResult<()> {
        on_stage(WriteStage::Writing);

        {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            write_pack(&mut writer, plan)?;
            writer.flush()?;
            let file = writer
                .into_inner()
                .map_err(|e| PackError::Io(e.into_error()))?;
            file.sync_all()?;
        }

        on_stage(WriteStage::Validating);

        {
            // Self-validate through the runtime loader: header, tables,
            // cross-references, and sample region must all check out.
            let pack = crate::loader::KvPack::open(&tmp)?;
            drop(pack);
        }

        std::fs::rename(&tmp, path)?;
        on_stage(WriteStage::Committed);
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{ClipData, PackBuilder};
    use kv_core::PhysicalKey;

    fn temp_out(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("kvpack-writer-test-{}-{name}", std::process::id()));
        p
    }

    #[test]
    fn test_encode_metadata_lengths() {
        let meta = encode_metadata("n", "a", "d", "l", "s").unwrap();
        let mut expected = Vec::new();
        for v in ["n", "a", "d", "l", "s"] {
            expected.extend_from_slice(&1u32.to_le_bytes());
            expected.extend_from_slice(v.as_bytes());
        }
        assert_eq!(meta, expected);
    }

    #[test]
    fn test_encode_metadata_rejects_oversized_string() {
        let big = "x".repeat((MAX_STRING_SIZE + 1) as usize);
        assert!(matches!(
            encode_metadata(&big, "", "", "", ""),
            Err(PackError::InvalidStringLength(..))
        ));
    }

    #[test]
    fn test_write_pack_matches_layout_size() {
        let mut builder = PackBuilder::new();
        builder.name = "Test".to_string();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![1, 2, 3], 48000).unwrap(),
            )
            .unwrap();
        builder
            .add_clip(
                PhysicalKey::B,
                ClipData::from_samples(vec![4, 5], 48000).unwrap(),
            )
            .unwrap();

        let plan = builder.plan().unwrap();
        let mut buf = Vec::new();
        write_pack(&mut buf, &plan).unwrap();

        assert_eq!(buf.len() as u64, plan.layout.file_size);
        assert_eq!(&buf[0..8], &MAGIC);
        assert_eq!(buf.len() % 2, 0);
    }

    #[test]
    fn test_write_plan_atomic_produces_valid_pack() {
        let out = temp_out("atomic.kvpack");

        let mut builder = PackBuilder::new();
        builder.name = "Atomic".to_string();
        builder
            .add_clip(
                PhysicalKey::Space,
                ClipData::from_samples(vec![0i16; 64], 48000).unwrap(),
            )
            .unwrap();

        let plan = builder.plan().unwrap();
        write_plan_atomic(&out, &plan, |_| {}).unwrap();

        let pack = crate::loader::KvPack::open(&out).unwrap();
        assert_eq!(pack.header().key_count, 1);
        assert_eq!(pack.header().sample_rate, 48000);
        drop(pack);

        // Temp file must not survive.
        assert!(!temp_path(&out).exists());
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn test_failed_write_leaves_no_output() {
        let out = temp_out("fail.kvpack");
        let _ = std::fs::remove_file(&out);

        let mut builder = PackBuilder::new();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![7i16; 8], 48000).unwrap(),
            )
            .unwrap();

        // Corrupt the plan so self-validation fails.
        let mut plan = builder.plan().unwrap();
        plan.sample_rate = 1;

        let err = write_plan_atomic(&out, &plan, |_| {}).unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(!out.exists(), "failed build must not leave output behind");
        assert!(
            !temp_path(&out).exists(),
            "temporary file must be cleaned up"
        );
    }
}
