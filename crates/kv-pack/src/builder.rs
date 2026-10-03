//! KVPack builder - creates binary packs from audio sources.

use crate::error::{PackError, PackResult};
use crate::format::*;
use kv_core::PhysicalKey;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Audio clip data for a single variant.
#[derive(Debug, Clone)]
pub struct ClipData {
    /// Sample data (mono i16, includes guard samples).
    pub samples: Vec<i16>,
    /// Logical frame count (without guards).
    pub logical_frames: u32,
    /// Sample rate.
    pub sample_rate: u32,
}

impl ClipData {
    /// Creates clip data from a WAV file.
    ///
    /// Automatically adds guard samples (GUARD_BEFORE at start, GUARD_AFTER at end).
    pub fn from_wav<P: AsRef<Path>>(path: P) -> PackResult<Self> {
        let mut reader = hound::WavReader::open(path.as_ref())?;
        let spec = reader.spec();

        // Validate format
        if spec.sample_format != hound::SampleFormat::Int {
            return Err(PackError::UnsupportedFormat(
                "Only 16-bit PCM WAV supported".to_string(),
            ));
        }
        if spec.bits_per_sample != 16 {
            return Err(PackError::UnsupportedFormat(format!(
                "Expected 16-bit, got {}",
                spec.bits_per_sample
            )));
        }
        if spec.channels != 1 {
            return Err(PackError::UnsupportedFormat(format!(
                "Expected mono, got {} channels",
                spec.channels
            )));
        }

        let sample_rate = spec.sample_rate;
        let mut samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
        let logical_frames = samples.len() as u32;

        if logical_frames == 0 {
            return Err(PackError::ZeroLengthClip(0));
        }
        if logical_frames > MAX_CLIP_FRAMES {
            return Err(PackError::ClipTooLarge {
                clip: 0,
                frames: logical_frames,
                max: MAX_CLIP_FRAMES,
            });
        }

        // Add guard samples
        let first_sample = samples[0];
        let last_sample = *samples.last().unwrap();

        // Prepend GUARD_BEFORE copies of first sample
        let mut guarded = vec![first_sample; GUARD_BEFORE as usize];
        guarded.extend_from_slice(&samples);
        // Append GUARD_AFTER copies of last sample
        guarded.extend_from_slice(&vec![last_sample; GUARD_AFTER as usize]);

        Ok(Self {
            samples: guarded,
            logical_frames,
            sample_rate,
        })
    }

    /// Creates clip data from raw i16 samples (adds guards).
    pub fn from_samples(samples: Vec<i16>, sample_rate: u32) -> PackResult<Self> {
        let logical_frames = samples.len() as u32;

        if logical_frames == 0 {
            return Err(PackError::ZeroLengthClip(0));
        }
        if logical_frames > MAX_CLIP_FRAMES {
            return Err(PackError::ClipTooLarge {
                clip: 0,
                frames: logical_frames,
                max: MAX_CLIP_FRAMES,
            });
        }

        let first_sample = samples[0];
        let last_sample = *samples.last().unwrap();

        let mut guarded = vec![first_sample; GUARD_BEFORE as usize];
        guarded.extend_from_slice(&samples);
        guarded.extend_from_slice(&vec![last_sample; GUARD_AFTER as usize]);

        Ok(Self {
            samples: guarded,
            logical_frames,
            sample_rate,
        })
    }

    /// Returns stored frame count (logical + guards).
    pub fn stored_frames(&self) -> u32 {
        self.samples.len() as u32
    }
}

/// Builder for creating KVPack files.
pub struct PackBuilder {
    /// Keys mapped to their variant clips.
    keys: BTreeMap<PhysicalKey, Vec<ClipData>>,
    /// Metadata fields.
    pub name: String,
    pub author: String,
    pub description: String,
    pub license: String,
    pub source: String,
}

impl Default for PackBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PackBuilder {
    /// Creates a new empty pack builder.
    pub fn new() -> Self {
        Self {
            keys: BTreeMap::new(),
            name: String::new(),
            author: String::new(),
            description: String::new(),
            license: String::new(),
            source: String::new(),
        }
    }

    /// Adds a clip variant for a key.
    pub fn add_clip(&mut self, key: PhysicalKey, clip: ClipData) -> PackResult<()> {
        let variants = self.keys.entry(key).or_insert_with(Vec::new);

        if variants.len() >= MAX_VARIANTS_PER_KEY as usize {
            return Err(PackError::InvalidVariantCount {
                key: key.as_u16(),
                count: variants.len() as u16 + 1,
                max: MAX_VARIANTS_PER_KEY,
            });
        }

        variants.push(clip);
        Ok(())
    }

    /// Adds a WAV file as a variant for a key.
    pub fn add_wav(&mut self, key: PhysicalKey, path: impl AsRef<Path>) -> PackResult<()> {
        let clip = ClipData::from_wav(path)?;
        self.add_clip(key, clip)
    }

    /// Writes the pack to a file.
    pub fn write<P: AsRef<Path>>(&self, path: P) -> PackResult<()> {
        let file = File::create(path.as_ref())?;
        let mut writer = BufWriter::new(file);

        // Validate counts
        let key_count = self.keys.len() as u32;
        if key_count == 0 {
            return Err(PackError::TooManyKeys(0, 1));
        }
        if key_count > MAX_KEYS {
            return Err(PackError::TooManyKeys(key_count, MAX_KEYS));
        }

        let clip_count: u32 = self
            .keys
            .values()
            .map(|v| v.len() as u32)
            .sum();
        if clip_count > MAX_CLIPS {
            return Err(PackError::TooManyClips(clip_count, MAX_CLIPS));
        }

        // Serialize metadata
        let metadata_bytes = self.serialize_metadata()?;
        let metadata_size = metadata_bytes.len() as u32;

        // Calculate offsets
        let metadata_offset = HEADER_SIZE as u64;
        let key_table_offset = metadata_offset + metadata_size as u64;
        let key_table_size = key_count * KEY_ENTRY_SIZE as u32;
        let clip_table_offset = key_table_offset + key_table_size as u64;
        let clip_table_size = clip_count * CLIP_ENTRY_SIZE as u32;

        // Align sample data to SAMPLE_ALIGNMENT
        let unaligned_offset = clip_table_offset + clip_table_size as u64;
        let sample_data_offset = ((unaligned_offset + SAMPLE_ALIGNMENT - 1) / SAMPLE_ALIGNMENT) * SAMPLE_ALIGNMENT;
        let padding = (sample_data_offset - unaligned_offset) as usize;

        // Build key and clip tables
        let (key_table, clip_table, sample_data) = self.build_tables(sample_data_offset)?;

        let sample_data_size = sample_data.len() as u64 * std::mem::size_of::<i16>() as u64;
        let file_size = sample_data_offset + sample_data_size;

        if file_size > MAX_PACK_SIZE {
            return Err(PackError::InvalidLength {
                field: "file_size",
                length: file_size,
            });
        }

        // Get sample rate from first clip
        let sample_rate = self
            .keys
            .values()
            .next()
            .and_then(|v| v.first())
            .map(|c| c.sample_rate)
            .unwrap_or(48000);

        // Write header
        self.write_header(
            &mut writer,
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
        )?;

        // Write metadata
        writer.write_all(&metadata_bytes)?;

        // Write key table
        writer.write_all(&key_table)?;

        // Write clip table
        writer.write_all(&clip_table)?;

        // Write padding
        writer.write_all(&vec![0u8; padding])?;

        // Write sample data
        for sample in &sample_data {
            writer.write_all(&sample.to_le_bytes())?;
        }

        writer.flush()?;
        Ok(())
    }

    fn serialize_metadata(&self) -> PackResult<Vec<u8>> {
        let mut buf = Vec::new();

        let write_string = |buf: &mut Vec<u8>, s: &str| -> PackResult<()> {
            let bytes = s.as_bytes();
            let len = bytes.len() as u32;

            if len > MAX_STRING_SIZE {
                return Err(PackError::InvalidStringLength(len, MAX_STRING_SIZE));
            }

            buf.extend_from_slice(&len.to_le_bytes());
            buf.extend_from_slice(bytes);
            Ok(())
        };

        write_string(&mut buf, &self.name)?;
        write_string(&mut buf, &self.author)?;
        write_string(&mut buf, &self.description)?;
        write_string(&mut buf, &self.license)?;
        write_string(&mut buf, &self.source)?;

        if buf.len() as u32 > MAX_METADATA_SIZE {
            return Err(PackError::InvalidMetadataSize(
                buf.len() as u32,
                MAX_METADATA_SIZE,
            ));
        }

        Ok(buf)
    }

    fn build_tables(&self, sample_data_offset: u64) -> PackResult<(Vec<u8>, Vec<u8>, Vec<i16>)> {
        let mut key_table = Vec::new();
        let mut clip_table = Vec::new();
        let mut sample_data = Vec::new();

        let mut clip_index = 0u32;

        for (key, variants) in &self.keys {
            // Write key entry
            key_table.extend_from_slice(&key.as_u16().to_le_bytes());
            key_table.extend_from_slice(&(variants.len() as u16).to_le_bytes());
            key_table.extend_from_slice(&clip_index.to_le_bytes());

            // Write clip entries for this key's variants
            for clip in variants {
                let sample_offset = sample_data.len() as u64 * std::mem::size_of::<i16>() as u64;

                clip_table.extend_from_slice(&sample_offset.to_le_bytes());
                clip_table.extend_from_slice(&clip.logical_frames.to_le_bytes());
                clip_table.extend_from_slice(&GUARD_BEFORE.to_le_bytes());
                clip_table.extend_from_slice(&GUARD_AFTER.to_le_bytes());
                clip_table.extend_from_slice(&clip.stored_frames().to_le_bytes());

                // Append sample data
                sample_data.extend_from_slice(&clip.samples);

                clip_index += 1;
            }
        }

        Ok((key_table, clip_table, sample_data))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_header<W: Write>(
        &self,
        writer: &mut W,
        file_size: u64,
        key_count: u32,
        clip_count: u32,
        metadata_offset: u64,
        metadata_size: u32,
        key_table_offset: u64,
        key_table_size: u32,
        clip_table_offset: u64,
        clip_table_size: u32,
        sample_data_offset: u64,
        sample_data_size: u64,
        sample_rate: u32,
    ) -> PackResult<()> {
        writer.write_all(&MAGIC)?;
        writer.write_all(&FORMAT_VERSION.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?; // flags
        writer.write_all(&HEADER_SIZE.to_le_bytes())?;
        writer.write_all(&file_size.to_le_bytes())?;
        writer.write_all(&key_count.to_le_bytes())?;
        writer.write_all(&clip_count.to_le_bytes())?;
        writer.write_all(&metadata_offset.to_le_bytes())?;
        writer.write_all(&metadata_size.to_le_bytes())?;
        writer.write_all(&key_table_offset.to_le_bytes())?;
        writer.write_all(&key_table_size.to_le_bytes())?;
        writer.write_all(&clip_table_offset.to_le_bytes())?;
        writer.write_all(&clip_table_size.to_le_bytes())?;
        writer.write_all(&sample_data_offset.to_le_bytes())?;
        writer.write_all(&sample_data_size.to_le_bytes())?;
        writer.write_all(&sample_rate.to_le_bytes())?;
        writer.write_all(&1u16.to_le_bytes())?; // channels = 1
        writer.write_all(&SAMPLE_FORMAT_I16LE.to_le_bytes())?;
        writer.write_all(&[0u8; 28])?; // reserved

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_from_samples() {
        let samples = vec![100i16, 200, 300, 400, 500];
        let clip = ClipData::from_samples(samples.clone(), 48000).unwrap();

        assert_eq!(clip.logical_frames, 5);
        assert_eq!(clip.stored_frames(), 5 + GUARD_BEFORE + GUARD_AFTER);
        assert_eq!(clip.samples[0], 100); // First guard = first sample
        assert_eq!(clip.samples[GUARD_BEFORE as usize], 100); // Logical start
        assert_eq!(
            clip.samples[clip.samples.len() - 1],
            500
        ); // Last guard = last sample
    }

    #[test]
    fn test_builder_empty_fails() {
        let builder = PackBuilder::new();
        let result = builder.write("/tmp/test_empty.kvpack");
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_single_key() {
        let mut builder = PackBuilder::new();
        builder.name = "Test Pack".to_string();
        builder.author = "Test Author".to_string();

        let samples = vec![0i16; 1000];
        let clip = ClipData::from_samples(samples, 48000).unwrap();
        builder.add_clip(PhysicalKey::A, clip).unwrap();

        // Would write to temp file in real test
        assert_eq!(builder.keys.len(), 1);
    }

    #[test]
    fn test_too_many_variants() {
        let mut builder = PackBuilder::new();

        for _ in 0..=MAX_VARIANTS_PER_KEY {
            let samples = vec![0i16; 100];
            let clip = ClipData::from_samples(samples, 48000).unwrap();
            let result = builder.add_clip(PhysicalKey::A, clip);

            if builder.keys[&PhysicalKey::A].len() >= MAX_VARIANTS_PER_KEY as usize {
                assert!(result.is_err());
                break;
            }
        }
    }
}
