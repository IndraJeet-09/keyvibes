//! Pack layout calculation.
//!
//! Every offset and size in a KVPack is computed here *before* any bytes are
//! written. All arithmetic is checked so that a hostile or accidentally huge
//! manifest can never wrap around.

use crate::error::{PackError, PackResult};
use crate::format::*;

/// Fully computed byte layout of a pack file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackLayout {
    pub metadata_offset: u64,
    pub metadata_size: u32,
    pub key_table_offset: u64,
    pub key_table_size: u32,
    pub clip_table_offset: u64,
    pub clip_table_size: u32,
    pub sample_data_offset: u64,
    pub sample_data_size: u64,
    /// Padding bytes between the end of the clip table and `sample_data_offset`.
    pub padding: u64,
    pub file_size: u64,
}

/// Rounds `offset` up to the next multiple of `alignment`.
///
/// Returns an error if the alignment is zero/not a power of two or if the
/// calculation would overflow.
pub fn align_up(offset: u64, alignment: u64) -> PackResult<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(PackError::InvalidAlignment {
            field: "alignment",
            offset,
            alignment,
        });
    }

    let mask = alignment - 1;
    offset
        .checked_add(mask)
        .map(|v| v & !mask)
        .ok_or(PackError::IntegerOverflow)
}

/// Computes sample-region byte offsets for each clip, in clip-table order.
///
/// Returns the per-clip offsets plus the total number of sample bytes.
pub fn compute_sample_offsets(stored_frames: &[u32]) -> PackResult<(Vec<u64>, u64)> {
    let mut offsets = Vec::with_capacity(stored_frames.len());
    let mut offset = 0u64;

    for &frames in stored_frames {
        offsets.push(offset);
        let bytes = (frames as u64)
            .checked_mul(std::mem::size_of::<i16>() as u64)
            .ok_or(PackError::IntegerOverflow)?;
        offset = offset
            .checked_add(bytes)
            .ok_or(PackError::IntegerOverflow)?;
    }

    Ok((offsets, offset))
}

/// Computes the complete layout for a pack.
///
/// Layout (per format specification):
///
/// ```text
/// Header (120) → Metadata → Key table → Clip table → Padding → Sample data
/// ```
pub fn compute_layout(
    metadata_size: u32,
    key_count: u32,
    clip_count: u32,
    sample_bytes: u64,
) -> PackResult<PackLayout> {
    if key_count == 0 {
        return Err(PackError::EmptyPack);
    }
    if key_count > MAX_KEYS {
        return Err(PackError::TooManyKeys(key_count, MAX_KEYS));
    }
    if clip_count > MAX_CLIPS {
        return Err(PackError::TooManyClips(clip_count, MAX_CLIPS));
    }
    if metadata_size > MAX_METADATA_SIZE {
        return Err(PackError::InvalidMetadataSize(
            metadata_size,
            MAX_METADATA_SIZE,
        ));
    }

    let metadata_offset = HEADER_SIZE as u64;

    let key_table_offset = metadata_offset
        .checked_add(metadata_size as u64)
        .ok_or(PackError::IntegerOverflow)?;
    let key_table_size = (key_count as u64)
        .checked_mul(KEY_ENTRY_SIZE as u64)
        .ok_or(PackError::IntegerOverflow)?;

    let clip_table_offset = key_table_offset
        .checked_add(key_table_size)
        .ok_or(PackError::IntegerOverflow)?;
    let clip_table_size = (clip_count as u64)
        .checked_mul(CLIP_ENTRY_SIZE as u64)
        .ok_or(PackError::IntegerOverflow)?;

    let unaligned = clip_table_offset
        .checked_add(clip_table_size)
        .ok_or(PackError::IntegerOverflow)?;

    let sample_data_offset = align_up(unaligned, SAMPLE_ALIGNMENT)?;
    let padding = sample_data_offset - unaligned;

    let file_size = sample_data_offset
        .checked_add(sample_bytes)
        .ok_or(PackError::IntegerOverflow)?;

    if file_size > MAX_PACK_SIZE {
        return Err(PackError::InvalidLength {
            field: "file_size",
            length: file_size,
        });
    }

    Ok(PackLayout {
        metadata_offset,
        metadata_size,
        key_table_offset,
        key_table_size: key_table_size as u32,
        clip_table_offset,
        clip_table_size: clip_table_size as u32,
        sample_data_offset,
        sample_data_size: sample_bytes,
        padding,
        file_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 16).unwrap(), 0);
        assert_eq!(align_up(1, 16).unwrap(), 16);
        assert_eq!(align_up(16, 16).unwrap(), 16);
        assert_eq!(align_up(17, 16).unwrap(), 32);
        assert_eq!(align_up(u64::MAX - 15, 16).unwrap(), u64::MAX - 15);
    }

    #[test]
    fn test_align_up_zero_alignment_errors() {
        assert!(align_up(16, 0).is_err());
        assert!(align_up(16, 3).is_err()); // not a power of two
    }

    #[test]
    fn test_align_up_overflow_errors() {
        assert!(align_up(u64::MAX, 16).is_err());
    }

    #[test]
    fn test_sample_offsets_are_sequential() {
        let (offsets, total) = compute_sample_offsets(&[105, 50, 5]).unwrap();
        assert_eq!(offsets, vec![0, 210, 310]);
        assert_eq!(total, 320);
    }

    #[test]
    fn test_layout_minimal() {
        let layout = compute_layout(32, 1, 1, 210).unwrap();
        assert_eq!(layout.metadata_offset, HEADER_SIZE as u64);
        assert_eq!(layout.key_table_offset, HEADER_SIZE as u64 + 32);
        assert_eq!(layout.key_table_size, KEY_ENTRY_SIZE as u32);
        assert_eq!(layout.clip_table_size, CLIP_ENTRY_SIZE as u32);
        assert_eq!(layout.sample_data_offset % SAMPLE_ALIGNMENT, 0);
        assert!(layout.sample_data_offset >= layout.clip_table_offset + CLIP_ENTRY_SIZE as u64);
        assert_eq!(layout.file_size, layout.sample_data_offset + 210);
    }

    #[test]
    fn test_layout_rejects_zero_keys() {
        assert!(compute_layout(0, 0, 0, 0).is_err());
    }

    #[test]
    fn test_layout_rejects_huge_counts() {
        assert!(compute_layout(0, MAX_KEYS + 1, 1, 0).is_err());
        assert!(compute_layout(0, 1, MAX_CLIPS + 1, 0).is_err());
    }
}
