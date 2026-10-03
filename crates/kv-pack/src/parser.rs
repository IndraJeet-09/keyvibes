//! Pack parsing structures.

use crate::error::{PackError, PackResult};
use crate::format::*;
use kv_core::PhysicalKey;

/// Parsed key entry from key table.
#[derive(Debug, Clone, Copy)]
pub struct KeyEntry {
    pub physical_key: PhysicalKey,
    pub variant_count: u16,
    pub first_clip: u32,
}

/// Parsed clip entry from clip table.
#[derive(Debug, Clone, Copy)]
pub struct ClipEntry {
    pub sample_offset: u64,
    pub sample_frames: u32,
    pub guard_before: u32,
    pub guard_after: u32,
    pub stored_frames: u32,
}

/// Parses key table from bytes.
pub fn parse_key_table(data: &[u8], key_count: u32) -> PackResult<Vec<KeyEntry>> {
    let expected_size = (key_count as usize)
        .checked_mul(KEY_ENTRY_SIZE)
        .ok_or(PackError::IntegerOverflow)?;

    if data.len() < expected_size {
        return Err(PackError::FileTruncated {
            expected: expected_size as u64,
            actual: data.len() as u64,
        });
    }

    let mut entries = Vec::with_capacity(key_count as usize);
    let mut prev_key_id = None;

    for i in 0..key_count {
        let offset = (i as usize) * KEY_ENTRY_SIZE;
        let entry_data = &data[offset..offset + KEY_ENTRY_SIZE];

        let physical_key_id = u16::from_le_bytes([entry_data[0], entry_data[1]]);
        let variant_count = u16::from_le_bytes([entry_data[2], entry_data[3]]);
        let first_clip = u32::from_le_bytes([
            entry_data[4],
            entry_data[5],
            entry_data[6],
            entry_data[7],
        ]);

        // Validate physical key ID
        let physical_key = PhysicalKey::from_u16(physical_key_id).ok_or(
            PackError::InvalidKeyId(physical_key_id, PhysicalKey::COUNT as u16 - 1),
        )?;

        // Check sorted order
        if let Some(prev) = prev_key_id {
            if physical_key_id <= prev {
                return Err(PackError::KeysNotSorted(physical_key_id, prev));
            }
        }
        prev_key_id = Some(physical_key_id);

        // Validate variant count
        if variant_count == 0 || variant_count > MAX_VARIANTS_PER_KEY {
            return Err(PackError::InvalidVariantCount {
                key: physical_key_id,
                count: variant_count,
                max: MAX_VARIANTS_PER_KEY,
            });
        }

        entries.push(KeyEntry {
            physical_key,
            variant_count,
            first_clip,
        });
    }

    Ok(entries)
}

/// Parses clip table from bytes.
pub fn parse_clip_table(data: &[u8], clip_count: u32) -> PackResult<Vec<ClipEntry>> {
    let expected_size = (clip_count as usize)
        .checked_mul(CLIP_ENTRY_SIZE)
        .ok_or(PackError::IntegerOverflow)?;

    if data.len() < expected_size {
        return Err(PackError::FileTruncated {
            expected: expected_size as u64,
            actual: data.len() as u64,
        });
    }

    let mut entries = Vec::with_capacity(clip_count as usize);

    for i in 0..clip_count {
        let offset = (i as usize) * CLIP_ENTRY_SIZE;
        let entry_data = &data[offset..offset + CLIP_ENTRY_SIZE];

        let sample_offset = u64::from_le_bytes([
            entry_data[0],
            entry_data[1],
            entry_data[2],
            entry_data[3],
            entry_data[4],
            entry_data[5],
            entry_data[6],
            entry_data[7],
        ]);

        let sample_frames = u32::from_le_bytes([
            entry_data[8],
            entry_data[9],
            entry_data[10],
            entry_data[11],
        ]);

        let guard_before = u32::from_le_bytes([
            entry_data[12],
            entry_data[13],
            entry_data[14],
            entry_data[15],
        ]);

        let guard_after = u32::from_le_bytes([
            entry_data[16],
            entry_data[17],
            entry_data[18],
            entry_data[19],
        ]);

        let stored_frames = u32::from_le_bytes([
            entry_data[20],
            entry_data[21],
            entry_data[22],
            entry_data[23],
        ]);

        // Validate zero-length
        if sample_frames == 0 {
            return Err(PackError::ZeroLengthClip(i));
        }

        // Validate clip size
        if sample_frames > MAX_CLIP_FRAMES {
            return Err(PackError::ClipTooLarge {
                clip: i,
                frames: sample_frames,
                max: MAX_CLIP_FRAMES,
            });
        }

        // Validate guard samples
        if guard_before != GUARD_BEFORE || guard_after != GUARD_AFTER {
            return Err(PackError::InvalidGuardSamples {
                clip: i,
                before: guard_before,
                after: guard_after,
                expected_before: GUARD_BEFORE,
                expected_after: GUARD_AFTER,
            });
        }

        // Validate stored frames calculation
        let expected_stored = guard_before
            .checked_add(sample_frames)
            .and_then(|v| v.checked_add(guard_after))
            .ok_or(PackError::IntegerOverflow)?;

        if stored_frames != expected_stored {
            return Err(PackError::StoredFramesMismatch {
                clip: i,
                stored: stored_frames,
                expected: expected_stored,
            });
        }

        entries.push(ClipEntry {
            sample_offset,
            sample_frames,
            guard_before,
            guard_after,
            stored_frames,
        });
    }

    Ok(entries)
}

/// Parses metadata strings.
pub struct Metadata {
    pub name: String,
    pub author: String,
    pub description: String,
    pub license: String,
    pub source: String,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            author: String::new(),
            description: String::new(),
            license: String::new(),
            source: String::new(),
        }
    }
}

pub fn parse_metadata(data: &[u8], metadata_size: u32) -> PackResult<Metadata> {
    if data.len() < metadata_size as usize {
        return Err(PackError::FileTruncated {
            expected: metadata_size as u64,
            actual: data.len() as u64,
        });
    }

    let mut metadata = Metadata::default();
    let mut offset = 0u32;

    // Helper to read a length-prefixed string
    let mut read_string = |_field_name: &str| -> PackResult<String> {
        if offset + 4 > metadata_size {
            return Err(PackError::MetadataOverflow {
                offset,
                length: 4,
                size: metadata_size,
            });
        }

        let len = u32::from_le_bytes([
            data[offset as usize],
            data[offset as usize + 1],
            data[offset as usize + 2],
            data[offset as usize + 3],
        ]);
        offset += 4;

        if len > MAX_STRING_SIZE {
            return Err(PackError::InvalidStringLength(len, MAX_STRING_SIZE));
        }

        if offset + len > metadata_size {
            return Err(PackError::MetadataOverflow {
                offset,
                length: len,
                size: metadata_size,
            });
        }

        let string_data = &data[offset as usize..(offset + len) as usize];
        offset += len;

        let s = std::str::from_utf8(string_data).map_err(|_| PackError::InvalidUtf8)?;

        Ok(s.to_string())
    };

    // Parse each field (optional - length 0 means omitted)
    metadata.name = read_string("name")?;
    metadata.author = read_string("author")?;
    metadata.description = read_string("description")?;
    metadata.license = read_string("license")?;
    metadata.source = read_string("source")?;

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_entry() {
        let mut data = vec![0u8; KEY_ENTRY_SIZE];
        data[0..2].copy_from_slice(&PhysicalKey::A.as_u16().to_le_bytes());
        // 3 variants
        data[2..4].copy_from_slice(&3u16.to_le_bytes());
        // first_clip = 10
        data[4..8].copy_from_slice(&10u32.to_le_bytes());

        let entries = parse_key_table(&data, 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].physical_key, PhysicalKey::A);
        assert_eq!(entries[0].variant_count, 3);
        assert_eq!(entries[0].first_clip, 10);
    }

    #[test]
    fn test_invalid_key_id() {
        let mut data = vec![0u8; KEY_ENTRY_SIZE];
        data[0..2].copy_from_slice(&999u16.to_le_bytes()); // Invalid
        data[2..4].copy_from_slice(&1u16.to_le_bytes());

        assert!(matches!(
            parse_key_table(&data, 1),
            Err(PackError::InvalidKeyId(999, _))
        ));
    }

    #[test]
    fn test_keys_not_sorted() {
        let mut data = vec![0u8; KEY_ENTRY_SIZE * 2];
        // Key 67 then 66 (not sorted)
        data[0..2].copy_from_slice(&67u16.to_le_bytes());
        data[2..4].copy_from_slice(&1u16.to_le_bytes());

        data[8..10].copy_from_slice(&66u16.to_le_bytes());
        data[10..12].copy_from_slice(&1u16.to_le_bytes());

        assert!(matches!(
            parse_key_table(&data, 2),
            Err(PackError::KeysNotSorted(66, 67))
        ));
    }
}
