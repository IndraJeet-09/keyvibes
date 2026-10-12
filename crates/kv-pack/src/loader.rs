//! Memory-mapped pack loader.

use crate::error::{PackError, PackResult};
use crate::format::*;
use crate::header::Header;
use crate::parser::{self, KeyEntry, ReleaseEntry};
use crate::playback::PlayCommand;
use crate::validator;
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

/// Bounds-checked view of a byte range inside the memory mapping.
///
/// Every offset and length comes from an attacker-controlled file header, so
/// nothing is ever sliced without going through this first. It rejects
/// overflow, values that do not fit a `usize`, and ranges past the end of the
/// mapping, returning a structured error instead of panicking.
fn region<'a>(data: &'a [u8], field: &'static str, offset: u64, size: u64) -> PackResult<&'a [u8]> {
    let end = offset.checked_add(size).ok_or(PackError::IntegerOverflow)?;
    let start = usize::try_from(offset).map_err(|_| PackError::IntegerOverflow)?;
    let end = usize::try_from(end).map_err(|_| PackError::IntegerOverflow)?;
    if end > data.len() {
        return Err(PackError::InvalidLength {
            field,
            length: size,
        });
    }
    Ok(&data[start..end])
}

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
    /// Parsed release entries (format version 2; empty for version 1)
    releases: Vec<ReleaseEntry>,
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

        // The mapping must cover exactly the file we sized. A file that
        // shrank between `metadata()` and `Mmap::map` would otherwise let the
        // validated regions below run off the end of the mapping.
        if mmap.len() as u64 != file_size {
            return Err(PackError::FileTruncated {
                expected: file_size,
                actual: mmap.len() as u64,
            });
        }

        // Validate header
        let header = Header::parse(&mmap[..], file_size)?;

        // Extract key table
        let key_data = region(
            &mmap,
            "key_table",
            header.key_table_offset,
            header.key_table_size as u64,
        )?;
        let keys = parser::parse_key_table(key_data, header.key_count)?;

        // Extract release table (format version 2 only)
        let release_count = header.release_table_size as usize / RELEASE_ENTRY_SIZE;
        let releases = if release_count == 0 {
            Vec::new()
        } else {
            let release_data = region(
                &mmap,
                "release_table",
                header.release_table_offset,
                header.release_table_size as u64,
            )?;
            parser::parse_release_table(release_data, release_count as u32)?
        };

        // Extract clip table
        let clip_data = region(
            &mmap,
            "clip_table",
            header.clip_table_offset,
            header.clip_table_size as u64,
        )?;
        let clips = parser::parse_clip_table(clip_data, header.clip_count)?;

        // Full validation of relationships
        validator::validate_full(&header, &keys, &releases, &clips)?;

        // Decode metadata eagerly: a corrupt metadata section must fail at
        // open time, not on the first `metadata()` call, and the hot path
        // never re-parses strings.
        let metadata = if header.metadata_size == 0 {
            crate::parser::Metadata::default()
        } else {
            let metadata_bytes = region(
                &mmap,
                "metadata",
                header.metadata_offset,
                header.metadata_size as u64,
            )?;
            parser::parse_metadata(metadata_bytes, header.metadata_size)?
        };

        // Create sample slice (zero-copy into mmap).
        //
        // The header guarantees `sample_data_offset % 16 == 0`, so the cast
        // below always produces an aligned `i16` pointer.
        let sample_bytes = region(
            &mmap,
            "sample_data",
            header.sample_data_offset,
            header.sample_data_size,
        )?;
        let samples: &[i16] = unsafe {
            std::slice::from_raw_parts(
                sample_bytes.as_ptr() as *const i16,
                sample_bytes.len() / std::mem::size_of::<i16>(),
            )
        };

        Ok(Self {
            mmap,
            header,
            keys,
            releases,
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
        let byte_length = (clip.stored_frames as usize).checked_mul(std::mem::size_of::<i16>())?;
        let byte_end = byte_offset.checked_add(byte_length)?;
        if byte_end > std::mem::size_of_val(self.samples) {
            return None;
        }
        // `sample_offset` is validated even, so every clip slice starts on an
        // `i16` boundary inside the aligned sample region.
        if byte_offset % std::mem::size_of::<i16>() != 0 {
            return None;
        }

        // Return slice of i16 samples (includes guard samples)
        Some(&self.samples[byte_offset / 2..byte_end / 2])
    }

    /// Gets statistics.
    pub fn stats(&self) -> PackStats {
        PackStats {
            name: self.metadata.name.clone(),
            keys: self.header.key_count,
            release_keys: self.releases.len() as u32,
            clips: self.header.clip_count,
            sample_rate: self.header.sample_rate,
            channels: self.header.channels,
            sample_frames: self.clips.iter().map(|c| c.sample_frames as u64).sum(),
            mapped_size: self.mmap.len(),
        }
    }

    /// Physical keys this pack carries no sound for, in `PhysicalKey` order.
    ///
    /// A key missing here is silent at runtime - [`play_command`](Self::play_command)
    /// returns `None` for it and the input pipeline counts the press rather
    /// than substituting another key's sound. Diagnostics report this list so
    /// "which keys does this pack actually cover" is answerable from the pack
    /// alone, without pressing anything.
    pub fn missing_keys(&self) -> Vec<kv_core::PhysicalKey> {
        (0..kv_core::PhysicalKey::COUNT as u16)
            .filter_map(kv_core::PhysicalKey::from_u16)
            .filter(|key| self.lookup(*key).is_none())
            .collect()
    }

    /// Number of physical keys this pack covers (out of
    /// [`PhysicalKey::COUNT`](kv_core::PhysicalKey::COUNT)).
    pub fn key_coverage(&self) -> u32 {
        (kv_core::PhysicalKey::COUNT as u32) - self.missing_keys().len() as u32
    }

    /// Whether the pack carries a sound for every physical key.
    pub fn covers_all_keys(&self) -> bool {
        self.missing_keys().is_empty()
    }
}

/// Pack statistics for diagnostics.
#[derive(Debug, Clone)]
pub struct PackStats {
    pub name: String,
    pub keys: u32,
    /// Number of keys that carry at least one release clip.
    pub release_keys: u32,
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

    /// Parsed release table entries, sorted by physical key.
    pub fn release_entries(&self) -> &[ReleaseEntry] {
        &self.releases
    }

    /// Looks up release sounds by binary search (release table sorted by key).
    pub fn release_lookup(&self, key: kv_core::PhysicalKey) -> Option<(u32, u16)> {
        let id = key.as_u16() as u32;
        let mut low = 0usize;
        let mut high = self.releases.len();
        while low < high {
            let mid = (low + high) / 2;
            let mid_id = self.releases[mid].physical_key.as_u16() as u32;
            if mid_id == id {
                let entry = &self.releases[mid];
                return Some((entry.first_release_clip, entry.release_variant_count));
            } else if mid_id < id {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        None
    }

    /// Whether the pack carries any release clip at all.
    pub fn has_release_sounds(&self) -> bool {
        !self.releases.is_empty()
    }

    /// Number of keys that carry at least one release clip.
    pub fn release_key_count(&self) -> u32 {
        self.releases.len() as u32
    }

    /// Creates a PlayCommand for a key **release**, when the pack has one.
    ///
    /// Returns `None` when the pack is version 1, when it has no release clip
    /// for `key`, or when the clip is degenerate. The returned command has
    /// [`PlayCommand::release`] set so the mixer can treat it differently
    /// from a press.
    ///
    /// # RT safety
    ///
    /// Same contract as [`play_command`](Self::play_command): binary search
    /// and slice indexing only.
    pub fn release_command(
        &self,
        key: kv_core::PhysicalKey,
        variant_state: &mut crate::lookup::VariantState,
        output_rate: u32,
        left_gain: f32,
        right_gain: f32,
    ) -> Option<PlayCommand> {
        let (first_clip, variant_count) = self.release_lookup(key)?;
        let selected = if variant_count == 1 {
            0
        } else {
            variant_state.select(variant_count)
        };
        let mut command = self.command_in_range(
            first_clip + selected as u32,
            output_rate,
            left_gain,
            right_gain,
        )?;
        command.release = true;
        Some(command)
    }

    /// Builds a play command for one clip index, shared by press and release.
    fn command_in_range(
        &self,
        clip_idx: u32,
        output_rate: u32,
        left_gain: f32,
        right_gain: f32,
    ) -> Option<PlayCommand> {
        if output_rate == 0 {
            return None;
        }

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
        self.command_in_range(clip_idx, output_rate, left_gain, right_gain)
    }
}
