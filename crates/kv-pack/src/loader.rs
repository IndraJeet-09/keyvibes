//! Memory-mapped pack loader.

use crate::error::{PackError, PackResult};
use crate::format::*;
use crate::header::Header;
use crate::parser::{self, KeyEntry};
use crate::playback::PlayCommand;
use crate::validator;
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

/// A loaded, validated, immutable KVPack.
///
/// The pack owns the memory mapping and provides safe access to samples.
/// All access is through pre-validated slices.
///
/// # Lifetime
///
/// The pack must remain alive while any derived sample slices or voices
/// reference its data.
pub struct KvPack {
    /// Memory mapping (owns the file data)
    mmap: Mmap,
    /// Validated header
    header: Header,
    /// Parsed key entries
    keys: Vec<KeyEntry>,
    /// Parsed clip entries
    clips: Vec<parser::ClipEntry>,
    /// Parsed metadata (eagerly decoded at open time)
    metadata: crate::parser::Metadata,
    /// Sample data region (slice into mmap)
    samples: &'static [i16], // Valid for lifetime of mmap
}

impl KvPack {
    /// Opens and validates a KVPack file.
    ///
    /// # Safety
    ///
    /// This performs filesystem I/O and must not be called from the RT thread.
    /// The resulting pack is immutable and safe for concurrent access.
    pub fn open<P: AsRef<Path>>(path: P) -> PackResult<Self> {
        let file = File::open(path.as_ref())?;
        let metadata = file.metadata()?;
        let file_size = metadata.len();

        // Must be at least header size
        if file_size < HEADER_SIZE as u64 {
            return Err(PackError::FileTruncated {
                expected: HEADER_SIZE as u64,
                actual: file_size,
            });
        }

        // Must not exceed maximum
        if file_size > MAX_PACK_SIZE {
            return Err(PackError::InvalidLength {
                field: "file_size",
                length: file_size,
            });
        }

        // Memory-map the file (read-only)
        let mmap = unsafe { Mmap::map(&file)? };

        // Validate header
        let header = Header::parse(&mmap[..], file_size)?;

        // Extract key table
        let key_data = &mmap[header.key_table_offset as usize
            ..(header.key_table_offset as usize + header.key_table_size as usize)];
        let keys = parser::parse_key_table(key_data, header.key_count)?;

        // Extract clip table
        let clip_data = &mmap[header.clip_table_offset as usize
            ..(header.clip_table_offset as usize + header.clip_table_size as usize)];
        let clips = parser::parse_clip_table(clip_data, header.clip_count)?;

        // Full validation of relationships
        validator::validate_full(&header, &keys, &clips)?;

        // Decode metadata eagerly: a corrupt metadata section must fail at
        // open time, not on the first `metadata()` call, and the hot path
        // never re-parses strings.
        let metadata = if header.metadata_size == 0 {
            crate::parser::Metadata::default()
        } else {
            let metadata_bytes = &mmap[header.metadata_offset as usize
                ..(header.metadata_offset as usize + header.metadata_size as usize)];
            parser::parse_metadata(metadata_bytes, header.metadata_size)?
        };

        // Create sample slice (zero-copy into mmap)
        let sample_offset = header.sample_data_offset as usize;
        let sample_size = header.sample_data_size as usize;

        // Validate sample data region is within mmap
        if sample_offset + sample_size > mmap.len() {
            return Err(PackError::InvalidLength {
                field: "sample_data",
                length: (sample_offset + sample_size) as u64,
            });
        }

        // Create i16 slice (little-endian is native on little-endian systems)
        let sample_bytes = &mmap[sample_offset..sample_offset + sample_size];
        let samples: &[i16] = unsafe {
            std::slice::from_raw_parts(
                sample_bytes.as_ptr() as *const i16,
                sample_size / std::mem::size_of::<i16>(),
            )
        };

        Ok(Self {
            mmap,
            header,
            keys,
            clips,
            metadata,
            samples,
        })
    }

    /// Gets the pack metadata (parsed once at open time).
    pub fn metadata(&self) -> &crate::parser::Metadata {
        &self.metadata
    }

    /// Looks up sounds by binary search (key table sorted by physical_key ID).
    pub fn lookup(&self, key: kv_core::PhysicalKey) -> Option<crate::lookup::KeySounds> {
        let id = key.as_u16() as u32;
        let mut low = 0usize;
        let mut high = self.keys.len();
        while low < high {
            let mid = (low + high) / 2;
            let mid_id = self.keys[mid].physical_key.as_u16() as u32;
            if mid_id == id {
                let entry = &self.keys[mid];
                return Some(crate::lookup::KeySounds {
                    key,
                    first_clip: entry.first_clip,
                    variant_count: entry.variant_count,
                });
            } else if mid_id < id {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        None
    }

    /// Gets a clip by index.
    ///
    /// Returns a slice into the sample data.
    /// Must not outlive `self`.
    pub fn get_clip(&self, clip_index: u32) -> Option<&[i16]> {
        if (clip_index as usize) >= self.clips.len() {
            return None;
        }
        let clip = &self.clips[clip_index as usize];

        // Calculate byte offset into sample region
        let byte_offset = clip.sample_offset as usize;
        let byte_length = (clip.stored_frames as usize) * std::mem::size_of::<i16>();

        if byte_offset + byte_length > std::mem::size_of_val(self.samples) {
            return None;
        }

        // Return slice of i16 samples (includes guard samples)
        Some(&self.samples[byte_offset / 2..(byte_offset + byte_length) / 2])
    }

    /// Gets statistics.
    pub fn stats(&self) -> PackStats {
        PackStats {
            name: self.metadata.name.clone(),
            keys: self.header.key_count,
            clips: self.header.clip_count,
            sample_rate: self.header.sample_rate,
            channels: self.header.channels,
            sample_frames: self.clips.iter().map(|c| c.sample_frames as u64).sum(),
            mapped_size: self.mmap.len(),
        }
    }
}

/// Pack statistics for diagnostics.
#[derive(Debug, Clone)]
pub struct PackStats {
    pub name: String,
    pub keys: u32,
    pub clips: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Total logical frames across all clips (guards excluded).
    pub sample_frames: u64,
    pub mapped_size: usize,
}

impl KvPack {
    /// Returns the header for inspection.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Parsed key table entries, sorted by physical key ID.
    pub fn keys(&self) -> &[KeyEntry] {
        &self.keys
    }

    /// Parsed clip table entries in clip-table order.
    pub fn clips(&self) -> &[parser::ClipEntry] {
        &self.clips
    }

    /// Creates a PlayCommand for a key press with variant selection.
    ///
    /// Returns `None` when the pack has no sound for `key`, when the clip is
    /// degenerate, or when `output_rate` is zero.
    ///
    /// # RT safety
    ///
    /// Binary search plus slice indexing only: no allocation, no locks, no I/O.
    /// The returned command points into this pack's memory mapping, so the
    /// pack must outlive every voice built from it.
    pub fn play_command(
        &self,
        key: kv_core::PhysicalKey,
        variant_state: &mut crate::lookup::VariantState,
        output_rate: u32,
        left_gain: f32,
        right_gain: f32,
    ) -> Option<PlayCommand> {
        if output_rate == 0 {
            return None;
        }

        let sounds = self.lookup(key)?;
        let selected_variant = variant_state.select(sounds.variant_count);
        let clip_idx = sounds.first_clip + selected_variant as u32;
        let clip_slice = self.get_clip(clip_idx)?;

        // clip_slice includes guard samples; logical frames start after
        // GUARD_BEFORE samples.
        let logical_frames = clip_slice
            .len()
            .saturating_sub((GUARD_BEFORE + GUARD_AFTER) as usize);
        if logical_frames == 0 {
            return None;
        }

        // Point at the start of the logical frames (after the leading guards).
        let ptr = unsafe { clip_slice.as_ptr().add(GUARD_BEFORE as usize) };

        // Fixed-point pitch step: source_rate / output_rate in 32.32.
        let source_rate = self.header.sample_rate;
        let step = ((source_rate as u64) << 32) / (output_rate as u64);
        if step == 0 {
            return None;
        }

        Some(PlayCommand {
            sample_ptr: ptr,
            sample_len: logical_frames as u32,
            source_rate,
            pitch_step: step,
            left_gain,
            right_gain,
            release: false,
            enqueued_ns: 0,
        })
    }
}
