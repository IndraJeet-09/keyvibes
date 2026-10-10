//! Full pack validation.

use crate::error::{PackError, PackResult};
use crate::header::Header;
use crate::parser::{ClipEntry, KeyEntry};

/// Validates all structural relationships in a parsed pack.
pub fn validate_full(
    header: &Header,
    key_entries: &[KeyEntry],
    clip_entries: &[ClipEntry],
) -> PackResult<()> {
    // Validate key → clip relationships
    for key in key_entries.iter() {
        // First clip index within bounds
        if key.first_clip >= header.clip_count {
            return Err(PackError::InvalidClipIndex {
                key: key.physical_key.as_u16(),
                first_clip: key.first_clip,
                clip_count: header.clip_count,
            });
        }

        // Clip range within bounds
        let end_clip = (key.first_clip as u64)
            .checked_add(key.variant_count as u64)
            .ok_or(PackError::IntegerOverflow)?;

        if end_clip > header.clip_count as u64 {
            return Err(PackError::ClipRangeOverflow {
                key: key.physical_key.as_u16(),
                first_clip: key.first_clip,
                variant_count: key.variant_count,
                clip_count: header.clip_count,
            });
        }
    }

    // Validate clip sample regions
    for (i, clip) in clip_entries.iter().enumerate() {
        // Sample offset must be even: clips are indexed as `i16` slices, and
        // an odd byte offset would hand the mixer an unaligned pointer.
        if clip.sample_offset % std::mem::size_of::<i16>() as u64 != 0 {
            return Err(PackError::InvalidAlignment {
                field: "clip.sample_offset",
                offset: clip.sample_offset,
                alignment: std::mem::size_of::<i16>() as u64,
            });
        }

        // Sample offset within region
        if clip.sample_offset >= header.sample_data_size {
            return Err(PackError::ClipOffsetOutOfBounds {
                clip: i as u32,
                offset: clip.sample_offset,
                size: header.sample_data_size,
            });
        }

        // Stored frames * sizeof(i16) within region
        let byte_length = (clip.stored_frames as u64)
            .checked_mul(std::mem::size_of::<i16>() as u64)
            .ok_or(PackError::IntegerOverflow)?;

        let byte_end = clip
            .sample_offset
            .checked_add(byte_length)
            .ok_or(PackError::IntegerOverflow)?;

        if byte_end > header.sample_data_size {
            return Err(PackError::ClipLengthExceeds {
                clip: i as u32,
                offset: clip.sample_offset,
                length: byte_length,
                size: header.sample_data_size,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::*;
    use kv_core::PhysicalKey;

    fn make_header(key_count: u32, clip_count: u32, sample_data_size: u64) -> Header {
        Header {
            format_version: FORMAT_VERSION,
            flags: 0,
            file_size: HEADER_SIZE as u64 + sample_data_size,
            key_count,
            clip_count,
            metadata_offset: HEADER_SIZE as u64,
            metadata_size: 0,
            key_table_offset: HEADER_SIZE as u64,
            key_table_size: key_count * KEY_ENTRY_SIZE as u32,
            clip_table_offset: HEADER_SIZE as u64,
            clip_table_size: clip_count * CLIP_ENTRY_SIZE as u32,
            sample_data_offset: HEADER_SIZE as u64,
            sample_data_size,
            sample_rate: 48000,
            channels: 1,
            sample_format: 0,
        }
    }

    #[test]
    fn test_valid_single_key_single_clip() {
        let header = make_header(1, 1, 1000);
        let keys = vec![KeyEntry {
            physical_key: PhysicalKey::A,
            variant_count: 1,
            first_clip: 0,
        }];
        let clips = vec![ClipEntry {
            sample_offset: 0,
            sample_frames: 100,
            guard_before: 2,
            guard_after: 3,
            stored_frames: 105,
        }];

        assert!(validate_full(&header, &keys, &clips).is_ok());
    }

    #[test]
    fn test_clip_out_of_bounds() {
        let header = make_header(1, 1, 100); // small region
        let keys = vec![KeyEntry {
            physical_key: PhysicalKey::A,
            variant_count: 1,
            first_clip: 0,
        }];
        let clips = vec![ClipEntry {
            sample_offset: 0,
            sample_frames: 10000, // WAY too large for region
            guard_before: 2,
            guard_after: 3,
            stored_frames: 10005,
        }];

        assert!(validate_full(&header, &keys, &clips).is_err());
    }
}
