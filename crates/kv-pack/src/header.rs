//! Header parsing and validation.

use crate::error::{PackError, PackResult};
use crate::format::*;

/// Parsed and validated header.
#[derive(Debug, Clone)]
pub struct Header {
    pub format_version: u16,
    pub flags: u16,
    pub file_size: u64,
    pub key_count: u32,
    pub clip_count: u32,
    pub metadata_offset: u64,
    pub metadata_size: u32,
    pub key_table_offset: u64,
    pub key_table_size: u32,
    pub clip_table_offset: u64,
    pub clip_table_size: u32,
    pub sample_data_offset: u64,
    pub sample_data_size: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: u16,
}

impl Header {
    /// Parses header from bytes with strict validation.
    ///
    /// # Validation
    ///
    /// - Magic number
    /// - Format version
    /// - All offsets and sizes
    /// - Limits on counts
    /// - Overflow checks
    pub fn parse(data: &[u8], file_size: u64) -> PackResult<Self> {
        if data.len() < HEADER_SIZE as usize {
            return Err(PackError::FileTruncated {
                expected: HEADER_SIZE as u64,
                actual: data.len() as u64,
            });
        }

        // Check magic
        if data[0..8] != MAGIC {
            return Err(PackError::InvalidMagic);
        }

        // Parse fields (all little-endian)
        let format_version = u16::from_le_bytes([data[8], data[9]]);
        let flags = u16::from_le_bytes([data[10], data[11]]);
        let header_size = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let file_size_claimed = u64::from_le_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]);
        let key_count = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        let clip_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);

        let metadata_offset = u64::from_le_bytes([
            data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
        ]);
        let metadata_size = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);

        let key_table_offset = u64::from_le_bytes([
            data[44], data[45], data[46], data[47], data[48], data[49], data[50], data[51],
        ]);
        let key_table_size = u32::from_le_bytes([data[52], data[53], data[54], data[55]]);

        let clip_table_offset = u64::from_le_bytes([
            data[56], data[57], data[58], data[59], data[60], data[61], data[62], data[63],
        ]);
        let clip_table_size = u32::from_le_bytes([data[64], data[65], data[66], data[67]]);

        let sample_data_offset = u64::from_le_bytes([
            data[68], data[69], data[70], data[71], data[72], data[73], data[74], data[75],
        ]);
        let sample_data_size = u64::from_le_bytes([
            data[76], data[77], data[78], data[79], data[80], data[81], data[82], data[83],
        ]);

        let sample_rate = u32::from_le_bytes([data[84], data[85], data[86], data[87]]);
        let channels = u16::from_le_bytes([data[88], data[89]]);
        let sample_format = u16::from_le_bytes([data[90], data[91]]);

        // Check reserved bytes are zero
        for &byte in data[92..120].iter() {
            if byte != 0 {
                return Err(PackError::ReservedNotZero("reserved"));
            }
        }

        // Validate version
        if format_version != FORMAT_VERSION {
            return Err(PackError::UnsupportedVersion(format_version));
        }

        // Validate header size
        if header_size != HEADER_SIZE {
            return Err(PackError::InvalidHeaderSize(header_size));
        }

        // Validate file size
        if file_size_claimed != file_size {
            return Err(PackError::FileSizeMismatch {
                header: file_size_claimed,
                actual: file_size,
            });
        }

        // Validate counts
        if key_count > MAX_KEYS {
            return Err(PackError::TooManyKeys(key_count, MAX_KEYS));
        }
        if clip_count > MAX_CLIPS {
            return Err(PackError::TooManyClips(clip_count, MAX_CLIPS));
        }

        // Validate metadata
        if metadata_size > MAX_METADATA_SIZE {
            return Err(PackError::InvalidMetadataSize(
                metadata_size,
                MAX_METADATA_SIZE,
            ));
        }

        // Validate table sizes
        let expected_key_table_size = key_count
            .checked_mul(KEY_ENTRY_SIZE as u32)
            .ok_or(PackError::IntegerOverflow)?;
        if key_table_size != expected_key_table_size {
            return Err(PackError::InvalidLength {
                field: "key_table_size",
                length: key_table_size as u64,
            });
        }

        let expected_clip_table_size = clip_count
            .checked_mul(CLIP_ENTRY_SIZE as u32)
            .ok_or(PackError::IntegerOverflow)?;
        if clip_table_size != expected_clip_table_size {
            return Err(PackError::InvalidLength {
                field: "clip_table_size",
                length: clip_table_size as u64,
            });
        }

        // Validate sample rate
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(PackError::InvalidSampleRate(
                sample_rate,
                MIN_SAMPLE_RATE,
                MAX_SAMPLE_RATE,
            ));
        }

        // Validate channels
        if channels != 1 {
            return Err(PackError::InvalidChannelCount(channels));
        }

        // Validate sample format
        if sample_format != SAMPLE_FORMAT_I16LE {
            return Err(PackError::InvalidSampleFormat(sample_format));
        }

        // Validate alignment
        if sample_data_offset % SAMPLE_ALIGNMENT != 0 {
            return Err(PackError::InvalidAlignment {
                field: "sample_data_offset",
                offset: sample_data_offset,
                alignment: SAMPLE_ALIGNMENT,
            });
        }

        // Validate all offsets are within file
        Self::validate_offset("metadata_offset", metadata_offset, file_size)?;
        Self::validate_offset("key_table_offset", key_table_offset, file_size)?;
        Self::validate_offset("clip_table_offset", clip_table_offset, file_size)?;
        Self::validate_offset("sample_data_offset", sample_data_offset, file_size)?;

        // Validate offset + size combinations
        Self::validate_region("metadata", metadata_offset, metadata_size as u64, file_size)?;
        Self::validate_region(
            "key_table",
            key_table_offset,
            key_table_size as u64,
            file_size,
        )?;
        Self::validate_region(
            "clip_table",
            clip_table_offset,
            clip_table_size as u64,
            file_size,
        )?;
        Self::validate_region(
            "sample_data",
            sample_data_offset,
            sample_data_size,
            file_size,
        )?;

        Ok(Self {
            format_version,
            flags,
            file_size,
            key_count,
            clip_count,
            metadata_offset,
            metadata_size,
            key_table_offset,
            key_table_size,
            clip_table_offset,
            clip_table_size,
            sample_data_offset,
            sample_data_size,
            sample_rate,
            channels,
            sample_format,
        })
    }

    fn validate_offset(field: &'static str, offset: u64, file_size: u64) -> PackResult<()> {
        if offset > file_size {
            return Err(PackError::InvalidOffset {
                field,
                offset,
                file_size,
            });
        }
        Ok(())
    }

    fn validate_region(
        field: &'static str,
        offset: u64,
        size: u64,
        file_size: u64,
    ) -> PackResult<()> {
        let end = offset.checked_add(size).ok_or(PackError::IntegerOverflow)?;
        if end > file_size {
            return Err(PackError::InvalidLength {
                field,
                length: size,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_minimal_header() -> Vec<u8> {
        let mut data = vec![0u8; HEADER_SIZE as usize];

        // Magic
        data[0..8].copy_from_slice(&MAGIC);

        // Version
        data[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());

        // Header size
        data[12..16].copy_from_slice(&HEADER_SIZE.to_le_bytes());

        // File size (128 for 16-byte alignment)
        data[16..24].copy_from_slice(&(128u64).to_le_bytes());

        // Counts = 0

        // Offsets after header (aligned to 16 bytes for sample_data)
        let after_header = HEADER_SIZE as u64;
        let sample_offset = 128u64; // 16-byte aligned
        data[32..40].copy_from_slice(&after_header.to_le_bytes()); // metadata
        data[44..52].copy_from_slice(&after_header.to_le_bytes()); // key table
        data[56..64].copy_from_slice(&after_header.to_le_bytes()); // clip table
        data[68..76].copy_from_slice(&sample_offset.to_le_bytes()); // sample data

        // Sample rate
        data[84..88].copy_from_slice(&48000u32.to_le_bytes());

        // Channels = 1
        data[88..90].copy_from_slice(&1u16.to_le_bytes());

        // Sample format = 0
        data[90..92].copy_from_slice(&0u16.to_le_bytes());

        data
    }

    #[test]
    fn test_valid_minimal_header() {
        let data = make_minimal_header();
        let header = Header::parse(&data, 128).unwrap();
        assert_eq!(header.format_version, FORMAT_VERSION);
        assert_eq!(header.key_count, 0);
        assert_eq!(header.clip_count, 0);
    }

    #[test]
    fn test_invalid_magic() {
        let mut data = make_minimal_header();
        data[0] = 0xFF;
        assert!(matches!(
            Header::parse(&data, 128),
            Err(PackError::InvalidMagic)
        ));
    }

    #[test]
    fn test_unsupported_version() {
        let mut data = make_minimal_header();
        data[8] = 99; // version = 99
        assert!(matches!(
            Header::parse(&data, 128),
            Err(PackError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn test_file_size_mismatch() {
        let data = make_minimal_header();
        assert!(matches!(
            Header::parse(&data, 1000),
            Err(PackError::FileSizeMismatch { .. })
        ));
    }
}
