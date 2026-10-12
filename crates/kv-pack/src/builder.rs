//! KVPack builder: plans and serializes `.kvpack` files.
//!
//! The build pipeline is:
//!
//! ```text
//! pack.toml → manifest parse → WAV decode (per source)
//!           → [processing enabled?] f32 pipeline → i16 + dither
//!           → ClipData (guards)
//!           → PackPlan (layout, tables, offsets) → atomic write
//!           → self-validation via KvPack::open → rename into place
//! ```
//!
//! With `[processing] enabled = false` the pipeline is exactly the Phase 4
//! raw decode path (byte-identical output). Nothing is written to the final
//! path until the pack has been fully validated, so a failed build never
//! leaves a partial `.kvpack` behind.

use crate::dither::DitherSeed;
use crate::error::{PackError, PackResult};
use crate::format::*;
use crate::layout::{compute_layout, compute_sample_offsets};
use crate::manifest::PackManifest;
use crate::processing::{AudioProcessor, ProcessingConfig, ProcessingReport};
use crate::wav::{self, CanonicalPcm};
use crate::writer::{self, PackPlan, WriteStage};
use kv_core::PhysicalKey;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Audio clip data for a single variant (guard samples already applied).
#[derive(Debug, Clone)]
pub struct ClipData {
    /// Sample data (mono i16, includes guard samples).
    pub samples: Vec<i16>,
    /// Logical frame count (without guards).
    pub logical_frames: u32,
    /// Sample rate of the source clip.
    pub sample_rate: u32,
}

impl ClipData {
    /// Wraps canonical PCM (logical frames only) with guard samples.
    pub fn from_pcm(pcm: CanonicalPcm) -> PackResult<Self> {
        let logical_frames = pcm.samples.len() as u32;

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

        Ok(Self {
            samples: apply_guards(pcm.samples),
            logical_frames,
            sample_rate: pcm.sample_rate,
        })
    }

    /// Creates clip data from a WAV file.
    ///
    /// Accepts mono/stereo 8/16/24/32-bit PCM and 32-bit float WAVs; see
    /// [`crate::wav::load_wav`] for the full conversion rules.
    pub fn from_wav<P: AsRef<Path>>(path: P) -> PackResult<Self> {
        let pcm = wav::load_wav(path.as_ref())?;
        Self::from_pcm(pcm)
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

        Ok(Self {
            samples: apply_guards(samples),
            logical_frames,
            sample_rate,
        })
    }

    /// Returns stored frame count (logical + guards).
    pub fn stored_frames(&self) -> u32 {
        self.samples.len() as u32
    }
}

/// Adds `GUARD_BEFORE` copies of the first frame and `GUARD_AFTER` copies of
/// the last frame (edge extension), as allowed by the format specification.
fn apply_guards(samples: Vec<i16>) -> Vec<i16> {
    let first = samples[0];
    let last = samples[samples.len() - 1];

    let mut guarded =
        Vec::with_capacity(samples.len() + GUARD_BEFORE as usize + GUARD_AFTER as usize);
    guarded.extend(std::iter::repeat(first).take(GUARD_BEFORE as usize));
    guarded.extend_from_slice(&samples);
    guarded.extend(std::iter::repeat(last).take(GUARD_AFTER as usize));
    guarded
}

/// A progress event emitted while building a pack.
#[derive(Debug, Clone, PartialEq)]
pub enum BuildEvent {
    /// Build started; `total_sources` source files will be read.
    Start { total_sources: usize },
    /// Reading source `index` of `total` (manifest-relative path).
    Source {
        index: usize,
        total: usize,
        path: PathBuf,
    },
    /// One clip finished processing (`variant` is the index within the
    /// key's sample list). Only emitted when processing is enabled.
    Processed {
        key: PhysicalKey,
        variant: u16,
        report: ProcessingReport,
    },
    /// Serializing bytes to the temporary file.
    Writing,
    /// Re-opening the temporary file for self-validation.
    Validating,
    /// Pack written and committed to its final path.
    Finished,
}

/// Summary of a completed build.
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// Final output path.
    pub output: PathBuf,
    /// Number of keys in the pack.
    pub key_count: u32,
    /// Number of clips in the pack.
    pub clip_count: u32,
    /// Pack sample rate.
    pub sample_rate: u32,
    /// Final file size in bytes.
    pub file_size: u64,
    /// Sample region size in bytes.
    pub sample_bytes: u64,
    /// Total wall-clock build time.
    pub elapsed: std::time::Duration,
}

/// Builder for creating KVPack files.
pub struct PackBuilder {
    /// Keys mapped to their variant clips (BTreeMap = ascending key ID order).
    keys: BTreeMap<PhysicalKey, Vec<ClipData>>,
    /// Keys mapped to their release clips (ascending key ID order).
    ///
    /// Written as the version 2 release table; a pack with no release clips is
    /// written as version 1 and stays byte-identical to before this field
    /// existed.
    releases: BTreeMap<PhysicalKey, Vec<ClipData>>,
    /// Optional pack-wide sample rate requirement.
    sample_rate: Option<u32>,
    /// Metadata fields.
    pub name: String,
    pub author: String,
    pub description: String,
    pub license: String,
    pub source: String,
    /// Switch category; written as the optional sixth metadata string.
    pub category: String,
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
            releases: BTreeMap::new(),
            sample_rate: None,
            name: String::new(),
            author: String::new(),
            description: String::new(),
            license: String::new(),
            source: String::new(),
            category: String::new(),
        }
    }

    /// Requires every clip to use this exact sample rate.
    ///
    /// When unset, the rate is taken from the first clip and every other clip
    /// must match it (the v1 format has a single pack-wide rate; no resampling
    /// is performed in Phase 4B).
    pub fn set_sample_rate(&mut self, rate: u32) -> PackResult<()> {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate) {
            return Err(PackError::InvalidSampleRate(
                rate,
                MIN_SAMPLE_RATE,
                MAX_SAMPLE_RATE,
            ));
        }
        self.sample_rate = Some(rate);
        Ok(())
    }

    /// Adds a clip variant for a key.
    pub fn add_clip(&mut self, key: PhysicalKey, clip: ClipData) -> PackResult<()> {
        Self::push_variant(&mut self.keys, key, clip)
    }

    /// Adds a WAV file as a variant for a key.
    pub fn add_wav(&mut self, key: PhysicalKey, path: impl AsRef<Path>) -> PackResult<()> {
        let clip = ClipData::from_wav(path)?;
        self.add_clip(key, clip)
    }

    /// Adds a release clip variant for a key.
    pub fn add_release_clip(&mut self, key: PhysicalKey, clip: ClipData) -> PackResult<()> {
        Self::push_variant(&mut self.releases, key, clip)
    }

    /// Adds a WAV file as a release variant for a key.
    pub fn add_release_wav(&mut self, key: PhysicalKey, path: impl AsRef<Path>) -> PackResult<()> {
        let clip = ClipData::from_wav(path)?;
        self.add_release_clip(key, clip)
    }

    fn push_variant(
        map: &mut BTreeMap<PhysicalKey, Vec<ClipData>>,
        key: PhysicalKey,
        clip: ClipData,
    ) -> PackResult<()> {
        let variants = map.entry(key).or_default();

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

    /// Number of keys added so far.
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Number of keys that carry at least one release clip.
    pub fn release_count(&self) -> usize {
        self.releases.len()
    }

    /// Number of clips added so far (press and release combined).
    pub fn clip_count(&self) -> u32 {
        self.keys.values().map(|v| v.len() as u32).sum::<u32>()
            + self.releases.values().map(|v| v.len() as u32).sum::<u32>()
    }

    /// Resolves the pack-wide sample rate.
    ///
    /// All clips must agree; a mismatch is an error (no resampling).
    fn resolve_sample_rate(&self) -> PackResult<u32> {
        let mut rate = self.sample_rate;

        for (key, variants) in self.keys.iter().chain(self.releases.iter()) {
            for clip in variants {
                match rate {
                    None => rate = Some(clip.sample_rate),
                    Some(expected) if expected != clip.sample_rate => {
                        return Err(PackError::SampleRateMismatch {
                            path: format!("key {key}"),
                            expected,
                            actual: clip.sample_rate,
                        });
                    }
                    _ => {}
                }
            }
        }

        rate.ok_or(PackError::EmptyPack)
    }

    /// Computes the complete layout, tables, and metadata for this pack.
    ///
    /// Pure: performs no I/O and touches nothing on disk.
    pub fn plan(&self) -> PackResult<PackPlan<'_>> {
        let key_count = self.keys.len() as u32;
        if key_count == 0 {
            return Err(PackError::EmptyPack);
        }
        if key_count > MAX_KEYS {
            return Err(PackError::TooManyKeys(key_count, MAX_KEYS));
        }

        let clip_count = self.clip_count();
        if clip_count > MAX_CLIPS {
            return Err(PackError::TooManyClips(clip_count, MAX_CLIPS));
        }

        let release_count = self.releases.len() as u32;
        if release_count > MAX_KEYS {
            return Err(PackError::TooManyKeys(release_count, MAX_KEYS));
        }

        let sample_rate = self.resolve_sample_rate()?;

        let metadata = writer::encode_metadata(
            &self.name,
            &self.author,
            &self.description,
            &self.license,
            &self.source,
            &self.category,
        )?;

        // Sample offsets in clip-table order: every key's press variants
        // (ascending key), then every key's release variants (ascending key).
        let stored_frames: Vec<u32> = self
            .keys
            .values()
            .chain(self.releases.values())
            .flatten()
            .map(ClipData::stored_frames)
            .collect();
        let (sample_offsets, sample_bytes) = compute_sample_offsets(&stored_frames)?;

        let layout = compute_layout(
            metadata.len() as u32,
            key_count,
            release_count,
            clip_count,
            sample_bytes,
        )?;

        // Key, release, and clip tables (keys ascending by discriminant via
        // BTreeMap). Press clips occupy the clip table first; release clips
        // follow, and the release table points into that shared index space.
        let mut key_table = Vec::with_capacity(key_count as usize * KEY_ENTRY_SIZE);
        let mut release_table = Vec::with_capacity(release_count as usize * RELEASE_ENTRY_SIZE);
        let mut clip_table = Vec::with_capacity(clip_count as usize * CLIP_ENTRY_SIZE);
        let mut clips = Vec::with_capacity(clip_count as usize);
        let mut clip_index = 0u32;

        emit_key_and_clip_entries(
            &self.keys,
            &mut key_table,
            &mut clip_table,
            &mut clips,
            &sample_offsets,
            &mut clip_index,
        );
        emit_key_and_clip_entries(
            &self.releases,
            &mut release_table,
            &mut clip_table,
            &mut clips,
            &sample_offsets,
            &mut clip_index,
        );

        debug_assert_eq!(clip_index, clip_count);
        debug_assert_eq!(key_table.len(), layout.key_table_size as usize);
        debug_assert_eq!(release_table.len(), layout.release_table_size as usize);
        debug_assert_eq!(clip_table.len(), layout.clip_table_size as usize);

        Ok(PackPlan {
            layout,
            sample_rate,
            key_count,
            release_count,
            clip_count,
            metadata,
            key_table,
            release_table,
            clip_table,
            clips,
        })
    }

    /// Writes the pack atomically to `path`.
    pub fn write<P: AsRef<Path>>(&self, path: P) -> PackResult<()> {
        self.write_with_progress(path, |_| {})
    }

    /// Writes the pack atomically, reporting progress through `progress`.
    pub fn write_with_progress<P: AsRef<Path>>(
        &self,
        path: P,
        mut progress: impl FnMut(BuildEvent),
    ) -> PackResult<()> {
        let plan = self.plan()?;

        writer::write_plan_atomic(path.as_ref(), &plan, |stage| match stage {
            WriteStage::Writing => progress(BuildEvent::Writing),
            WriteStage::Validating => progress(BuildEvent::Validating),
            WriteStage::Committed => {}
        })?;

        progress(BuildEvent::Finished);
        Ok(())
    }
}

/// Emits one map's entries into its own table while assigning consecutive
/// clip-table slots. Press keys go first, release keys second; both write
/// clip entries in the shared clip-table order.
fn emit_key_and_clip_entries<'a>(
    map: &'a BTreeMap<PhysicalKey, Vec<ClipData>>,
    table: &mut Vec<u8>,
    clip_table: &mut Vec<u8>,
    clips: &mut Vec<&'a ClipData>,
    sample_offsets: &[u64],
    clip_index: &mut u32,
) {
    for (key, variants) in map {
        table.extend_from_slice(&key.as_u16().to_le_bytes());
        table.extend_from_slice(&(variants.len() as u16).to_le_bytes());
        table.extend_from_slice(&clip_index.to_le_bytes());

        for clip in variants {
            let sample_offset = sample_offsets[*clip_index as usize];

            clip_table.extend_from_slice(&sample_offset.to_le_bytes());
            clip_table.extend_from_slice(&clip.logical_frames.to_le_bytes());
            clip_table.extend_from_slice(&GUARD_BEFORE.to_le_bytes());
            clip_table.extend_from_slice(&GUARD_AFTER.to_le_bytes());
            clip_table.extend_from_slice(&clip.stored_frames().to_le_bytes());

            clips.push(clip);
            *clip_index += 1;
        }
    }
}

/// Resolves the dither seed for one clip, or `None` when dithering is off.
///
/// Deterministic mode derives from `(pack seed, key, variant, manifest-
/// relative source path)`, so the same manifest always produces the same
/// dither sequence — on any machine. `random_dither` trades that for OS
/// entropy (documented as build-time non-reproducible).
fn dither_seed_for(
    cfg: &ProcessingConfig,
    key: PhysicalKey,
    variant: u16,
    source: &str,
) -> Option<DitherSeed> {
    if !cfg.dither {
        return None;
    }
    if cfg.random_dither {
        return Some(DitherSeed::random());
    }
    Some(DitherSeed::derive(cfg.dither_seed, key, variant, source))
}

/// Resolves one manifest-relative sample path and turns it into a [`ClipData`].
///
/// Applies the Phase 5 processor when enabled, otherwise the legacy raw
/// decode. `expected_rate` is threaded through so every clip in the pack is
/// checked against a single pack-wide rate (the v1 format has no resampling).
/// Returns the clip plus its processing report (report is `None` on the
/// legacy path).
fn load_clip(
    root: &Path,
    relative: &str,
    key: PhysicalKey,
    variant: u16,
    processor: &AudioProcessor,
    processing: &ProcessingConfig,
    expected_rate: &mut Option<u32>,
) -> PackResult<(ClipData, Option<ProcessingReport>)> {
    let absolute =
        crate::manifest::resolve_source(root, relative).map_err(|e| PackError::BuildContext {
            key: key.to_string(),
            file: relative.to_string(),
            reason: e.to_string(),
        })?;

    let (pcm, report) = if processing.enabled {
        // Phase 5 path: f32 decode → DSP → dithered i16.
        // The dither seed derives from the *manifest-relative* path
        // so builds are reproducible across machines.
        let source = wav::decode_wav(&absolute).map_err(|e| PackError::BuildContext {
            key: key.to_string(),
            file: relative.to_string(),
            reason: e.to_string(),
        })?;
        let processed =
            processor
                .process(source.to_mono())
                .map_err(|e| PackError::BuildContext {
                    key: key.to_string(),
                    file: relative.to_string(),
                    reason: e.to_string(),
                })?;
        let seed = dither_seed_for(processing, key, variant, relative);
        let pcm = crate::processing::encode_i16(&processed.samples, processed.sample_rate, seed)
            .map_err(|e| PackError::BuildContext {
                key: key.to_string(),
                file: relative.to_string(),
                reason: e.to_string(),
            })?;

        (pcm, Some(processed.report))
    } else {
        // Legacy path: byte-identical to Phase 4.
        let pcm = wav::load_wav(&absolute).map_err(|e| PackError::BuildContext {
            key: key.to_string(),
            file: relative.to_string(),
            reason: e.to_string(),
        })?;
        (pcm, None)
    };

    // One pack-wide rate: report the offending file directly.
    let actual = pcm.sample_rate;
    match expected_rate {
        None => *expected_rate = Some(actual),
        Some(expected) if *expected != actual => {
            return Err(PackError::BuildContext {
                key: key.to_string(),
                file: relative.to_string(),
                reason: format!("sample rate mismatch: expected {expected} Hz, got {actual} Hz"),
            });
        }
        _ => {}
    }

    Ok((ClipData::from_pcm(pcm)?, report))
}

/// Builds a pack from a manifest file, reporting progress through `progress`.
///
/// The output is written atomically (temporary file → self-validate → rename),
/// so a failure never leaves a partial pack at `output`.
pub fn build_from_manifest<P: AsRef<Path>, O: AsRef<Path>>(
    manifest_path: P,
    output: O,
    mut progress: impl FnMut(BuildEvent),
) -> PackResult<BuildReport> {
    let start = Instant::now();
    let manifest_path = manifest_path.as_ref();
    let output = output.as_ref();

    let manifest = PackManifest::load(manifest_path)?;
    let keys = manifest.resolved_keys()?;
    let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let total_sources: usize = keys
        .iter()
        .map(|k| k.samples.len() + k.release_samples.len())
        .sum();

    let processing = manifest.processing_config()?;
    let processor = AudioProcessor::new(processing.clone());

    progress(BuildEvent::Start { total_sources });

    let mut builder = PackBuilder::new();
    builder.name = manifest.pack.name.clone();
    builder.author = manifest.pack.author.clone();
    builder.description = manifest.pack.description.clone();
    builder.license = manifest.pack.license.clone();
    builder.source = manifest.pack.source.clone();
    builder.category = manifest.pack.category.clone();

    if let Some(rate) = manifest.pack.sample_rate {
        builder.set_sample_rate(rate)?;
    }

    let mut expected_rate = manifest.pack.sample_rate;
    let mut index = 0usize;

    for key in &keys {
        for (variant, relative) in key.samples.iter().enumerate() {
            index += 1;
            progress(BuildEvent::Source {
                index,
                total: total_sources,
                path: PathBuf::from(relative),
            });

            let (clip, report) = load_clip(
                root,
                relative,
                key.physical_key,
                variant as u16,
                &processor,
                &processing,
                &mut expected_rate,
            )?;
            if let Some(report) = report {
                progress(BuildEvent::Processed {
                    key: key.physical_key,
                    variant: variant as u16,
                    report,
                });
            }
            builder.add_clip(key.physical_key, clip)?;
        }

        for (variant, relative) in key.release_samples.iter().enumerate() {
            index += 1;
            progress(BuildEvent::Source {
                index,
                total: total_sources,
                path: PathBuf::from(relative),
            });

            let (clip, report) = load_clip(
                root,
                relative,
                key.physical_key,
                variant as u16,
                &processor,
                &processing,
                &mut expected_rate,
            )?;
            if let Some(report) = report {
                progress(BuildEvent::Processed {
                    key: key.physical_key,
                    variant: variant as u16,
                    report,
                });
            }
            builder.add_release_clip(key.physical_key, clip)?;
        }
    }

    builder.write_with_progress(output, &mut progress)?;

    let plan = builder.plan()?;
    Ok(BuildReport {
        output: output.to_path_buf(),
        key_count: plan.key_count,
        clip_count: plan.clip_count,
        sample_rate: plan.sample_rate,
        file_size: plan.layout.file_size,
        sample_bytes: plan.layout.sample_data_size,
        elapsed: start.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_from_samples() {
        let samples = vec![100i16, 200, 300, 400, 500];
        let clip = ClipData::from_samples(samples, 48000).unwrap();

        assert_eq!(clip.logical_frames, 5);
        assert_eq!(clip.stored_frames(), 5 + GUARD_BEFORE + GUARD_AFTER);
        assert_eq!(clip.samples[0], 100); // First guard = first sample
        assert_eq!(clip.samples[GUARD_BEFORE as usize], 100); // Logical start
        assert_eq!(clip.samples[clip.samples.len() - 1], 500); // Last guard
    }

    #[test]
    fn test_clip_rejects_empty() {
        assert!(matches!(
            ClipData::from_samples(vec![], 48000),
            Err(PackError::ZeroLengthClip(_))
        ));
    }

    #[test]
    fn test_builder_empty_fails() {
        let builder = PackBuilder::new();
        let result = builder.write("/tmp/test_empty.kvpack");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PackError::EmptyPack));
    }

    #[test]
    fn test_builder_single_key() {
        let mut builder = PackBuilder::new();
        builder.name = "Test Pack".to_string();
        builder.author = "Test Author".to_string();

        let samples = vec![0i16; 1000];
        let clip = ClipData::from_samples(samples, 48000).unwrap();
        builder.add_clip(PhysicalKey::A, clip).unwrap();

        assert_eq!(builder.key_count(), 1);
        assert_eq!(builder.clip_count(), 1);
        let plan = builder.plan().unwrap();
        assert_eq!(plan.key_count, 1);
        assert_eq!(plan.sample_rate, 48000);
    }

    #[test]
    fn test_too_many_variants() {
        let mut builder = PackBuilder::new();

        for _ in 0..MAX_VARIANTS_PER_KEY {
            let samples = vec![0i16; 100];
            let clip = ClipData::from_samples(samples, 48000).unwrap();
            assert!(builder.add_clip(PhysicalKey::A, clip).is_ok());
        }

        let samples = vec![0i16; 100];
        let clip = ClipData::from_samples(samples, 48000).unwrap();
        assert!(builder.add_clip(PhysicalKey::A, clip).is_err());
    }

    #[test]
    fn test_sample_rate_mismatch_rejected() {
        let mut builder = PackBuilder::new();
        builder.name = "Mixed".to_string();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![1i16; 8], 48000).unwrap(),
            )
            .unwrap();
        builder
            .add_clip(
                PhysicalKey::B,
                ClipData::from_samples(vec![1i16; 8], 44100).unwrap(),
            )
            .unwrap();

        assert!(matches!(
            builder.plan(),
            Err(PackError::SampleRateMismatch {
                expected: 48000,
                actual: 44100,
                ..
            })
        ));
    }

    #[test]
    fn test_required_sample_rate_enforced() {
        let mut builder = PackBuilder::new();
        builder.name = "Rate".to_string();
        builder.set_sample_rate(48000).unwrap();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![1i16; 8], 44100).unwrap(),
            )
            .unwrap();

        assert!(matches!(
            builder.plan(),
            Err(PackError::SampleRateMismatch {
                expected: 48000,
                actual: 44100,
                ..
            })
        ));
    }

    #[test]
    fn test_plan_layout_is_consistent() {
        let mut builder = PackBuilder::new();
        builder.name = "Layout".to_string();
        builder
            .add_clip(
                PhysicalKey::Space,
                ClipData::from_samples(vec![0i16; 100], 48000).unwrap(),
            )
            .unwrap();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![1i16; 200], 48000).unwrap(),
            )
            .unwrap();

        let plan = builder.plan().unwrap();

        // Layout math.
        assert_eq!(plan.layout.sample_data_offset % SAMPLE_ALIGNMENT, 0);
        assert_eq!(
            plan.layout.file_size,
            plan.layout.sample_data_offset + plan.layout.sample_data_size
        );
        assert_eq!(
            plan.layout.sample_data_size,
            (100 + GUARD_BEFORE + GUARD_AFTER + 200 + GUARD_BEFORE + GUARD_AFTER) as u64 * 2
        );

        // Keys are sorted by discriminant (A has a smaller ID than Space).
        let first_key = u16::from_le_bytes([plan.key_table[0], plan.key_table[1]]);
        assert_eq!(first_key, PhysicalKey::A.as_u16());
    }

    #[test]
    fn test_build_round_trip_in_memory() {
        let mut builder = PackBuilder::new();
        builder.name = "RT".to_string();
        builder.author = "tests".to_string();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![10i16; 50], 48000).unwrap(),
            )
            .unwrap();

        let out = {
            let mut p = std::env::temp_dir();
            p.push(format!("kvpack-builder-rt-{}.kvpack", std::process::id()));
            p
        };

        let mut events = Vec::new();
        builder
            .write_with_progress(&out, |e| events.push(e))
            .unwrap();

        let pack = crate::loader::KvPack::open(&out).unwrap();
        assert_eq!(pack.header().key_count, 1);
        assert_eq!(pack.metadata().name, "RT");
        assert_eq!(
            pack.get_clip(0).unwrap().len(),
            50 + GUARD_BEFORE as usize + GUARD_AFTER as usize
        );
        drop(pack);

        assert!(events.iter().any(|e| matches!(e, BuildEvent::Writing)));
        assert!(events.iter().any(|e| matches!(e, BuildEvent::Validating)));
        assert!(matches!(events.last(), Some(BuildEvent::Finished)));

        let _ = std::fs::remove_file(&out);
    }
}
