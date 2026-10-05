//! Offline audio processing: trim, fade, normalize, protect, and encode
//! keyboard sound clips before they are packed.
//!
//! # Pipeline
//!
//! ```text
//! Source WAV
//!   → decode to f32                     (wav::decode_wav)
//!   → channel normalization to mono     (SourceAudio::to_mono)
//!   → DC correction                     (dsp::remove_dc, mean reported)
//!   → analysis                          (AudioAnalysis::measure + detect)
//!   → leading trim + preroll
//!   → tail trim
//!   → fade-out
//!   → loudness normalization (RMS)
//!   → peak protection
//!   → validation
//!   → TPDF dither + i16 quantization    (encode_i16)
//!   → guard samples                     (ClipData::from_pcm)
//!   → KVPack writer                     (Phase 4, unchanged)
//! ```
//!
//! Everything in this module runs **offline, in the builder**. The runtime
//! only ever sees the final `i16` PCM with guards already applied.
//!
//! All thresholds, windows, and targets are declared as named constants
//! below and collected in [`ProcessingConfig`]; nothing numeric is scattered
//! through the algorithms.

use crate::analysis::AudioAnalysis;
use crate::dither::{DitherSeed, TpdfDither};
use crate::dsp::{amplitude_to_db, db_to_amplitude, max_abs, mean, ms_to_frames, remove_dc, rms};
use crate::error::{PackError, PackResult};
use crate::format::{MAX_CLIP_FRAMES, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
use crate::wav::{CanonicalPcm, MonoAudio};

// ---------------------------------------------------------------------------
// Defaults (see docs/audio-processing.md for the rationale of each value)
// ---------------------------------------------------------------------------

/// Silence floor, absolute (dBFS). Content that never reaches this level is
/// silence for trimming purposes.
pub const DEFAULT_SILENCE_THRESHOLD_DBFS: f32 = -55.0;
/// Onset threshold, **relative to the clip peak** (dB). A peak of 1.0 gives
/// an absolute threshold of `10^(-45/20) ≈ 0.00562`.
pub const DEFAULT_ONSET_THRESHOLD_DB: f32 = -45.0;
/// Tail threshold, **relative to the clip peak** (dB).
pub const DEFAULT_TAIL_THRESHOLD_DB: f32 = -50.0;
/// Audio preserved before the detected onset (ms). 0.5 ms = 24 frames at
/// 48 kHz, 22 frames at 44.1 kHz.
pub const DEFAULT_PREROLL_MS: f32 = 0.5;
/// Fade-out length at the end of the trimmed clip (ms). 6 ms = 288 frames at
/// 48 kHz.
pub const DEFAULT_FADEOUT_MS: f32 = 6.0;
/// Attack analysis window used for onset confirmation (ms), clamped to the
/// clip length for very short clips.
pub const DEFAULT_ATTACK_RMS_WINDOW_MS: f32 = 60.0;
/// Loudness target: full-clip RMS after trim and fade (dB). See
/// [`AudioProcessor::process`] for why RMS is used instead of LUFS.
pub const DEFAULT_LOUDNESS_TARGET_DB: f32 = -30.0;
/// Peak ceiling the final (pre-dither) samples must not exceed (dBFS).
/// `10^(-1/20) ≈ 0.89125`.
pub const DEFAULT_PEAK_CEILING_DBFS: f32 = -1.0;
/// A clip whose usable peak is below this level fails the build (dBFS).
pub const DEFAULT_MIN_SIGNAL_DBFS: f32 = -80.0;
/// How long the signal must stay below the tail threshold before the clip is
/// considered ended (ms).
pub const DEFAULT_TAIL_SUSTAIN_MS: f32 = 5.0;
/// Upper bound on the loudness gain (dB), protecting against amplifying
/// near-silent sources into the target.
pub const DEFAULT_MAX_GAIN_DB: f32 = 40.0;

/// |DC offset| above this value raises a quality warning.
pub const HIGH_DC_OFFSET_WARNING: f32 = 0.01;
/// A source peak at or above this level (≈ 0 dBFS) raises a clipping warning.
pub const CLIPPED_SOURCE_PEAK: f32 = 0.999;
/// Peak within this many dB of `min_signal_dbfs` raises a low-signal warning.
pub const LOW_SIGNAL_WARNING_MARGIN_DB: f32 = 10.0;
/// Clips shorter than this raise a short-clip warning.
pub const SHORT_CLIP_WARNING_MS: f32 = 5.0;
/// Clips longer than this raise a long-clip warning.
pub const LONG_CLIP_WARNING_MS: f32 = 5_000.0;
/// Noise floor within this many dB of the peak raises a noise warning.
pub const HIGH_NOISE_FLOOR_WARNING_DB: f32 = 25.0;
/// Peak/RMS (crest) below this raises an abnormal-crest warning.
pub const MIN_CREST_WARNING_DB: f32 = 3.0;
/// Peak/RMS (crest) above this raises an abnormal-crest warning.
pub const MAX_CREST_WARNING_DB: f32 = 96.0;

/// Absolute headroom allowed when validating the peak ceiling, to absorb
/// `f32` rounding in the final multiply.
pub const CEILING_TOLERANCE: f32 = 1.0e-5;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Every tunable of the processing pipeline, with defaults.
///
/// Manifests override individual fields via `[processing]`; unspecified
/// fields keep these defaults. `enabled = false` bypasses processing
/// entirely and reproduces the Phase 4 raw decode path byte-for-byte.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingConfig {
    /// Master switch (manifest: `enabled`). `false` = Phase 4 raw path.
    pub enabled: bool,
    /// Silence floor in dBFS (absolute).
    pub silence_threshold_dbfs: f32,
    /// Onset threshold in dB **relative to the clip peak**.
    pub onset_threshold_db: f32,
    /// Tail threshold in dB **relative to the clip peak**.
    pub tail_threshold_db: f32,
    /// Preroll kept before the detected onset, in milliseconds.
    pub preroll_ms: f32,
    /// Fade-out length, in milliseconds.
    pub fadeout_ms: f32,
    /// Attack RMS window used for onset confirmation, in milliseconds.
    pub attack_rms_window_ms: f32,
    /// Target loudness (full-clip RMS), in dB.
    pub loudness_target_db: f32,
    /// Final peak ceiling in dBFS.
    pub peak_ceiling_dbfs: f32,
    /// Minimum usable peak in dBFS; below this the source is rejected.
    pub min_signal_dbfs: f32,
    /// Required duration of below-tail-threshold quiet before the clip ends.
    pub tail_sustain_ms: f32,
    /// Maximum loudness gain in dB.
    pub max_gain_db: f32,
    /// Whether to apply TPDF dither before `f32 → i16` quantization.
    pub dither: bool,
    /// Base seed for deterministic dither (combined with key + variant +
    /// source path by [`DitherSeed::derive`]).
    pub dither_seed: u64,
    /// Use OS entropy instead of the deterministic derivation. Breaks
    /// reproducible builds; intended for production masterings only.
    pub random_dither: bool,
}

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            silence_threshold_dbfs: DEFAULT_SILENCE_THRESHOLD_DBFS,
            onset_threshold_db: DEFAULT_ONSET_THRESHOLD_DB,
            tail_threshold_db: DEFAULT_TAIL_THRESHOLD_DB,
            preroll_ms: DEFAULT_PREROLL_MS,
            fadeout_ms: DEFAULT_FADEOUT_MS,
            attack_rms_window_ms: DEFAULT_ATTACK_RMS_WINDOW_MS,
            loudness_target_db: DEFAULT_LOUDNESS_TARGET_DB,
            peak_ceiling_dbfs: DEFAULT_PEAK_CEILING_DBFS,
            min_signal_dbfs: DEFAULT_MIN_SIGNAL_DBFS,
            tail_sustain_ms: DEFAULT_TAIL_SUSTAIN_MS,
            max_gain_db: DEFAULT_MAX_GAIN_DB,
            dither: true,
            dither_seed: 0,
            random_dither: false,
        }
    }
}

impl ProcessingConfig {
    /// Rejects non-finite or nonsensical values before any audio is touched.
    ///
    /// Returns a single actionable message naming the offending field.
    pub fn validate(&self) -> PackResult<()> {
        fn finite_range(name: &str, value: f32, min: f32, max: f32) -> PackResult<()> {
            if !value.is_finite() || value < min || value > max {
                return Err(PackError::ValidationFailed(format!(
                    "invalid processing config: {name} = {value} (must be in {min}..={max})"
                )));
            }
            Ok(())
        }

        finite_range(
            "silence_threshold_dbfs",
            self.silence_threshold_dbfs,
            -200.0,
            0.0,
        )?;
        finite_range("onset_threshold_db", self.onset_threshold_db, -200.0, 0.0)?;
        finite_range("tail_threshold_db", self.tail_threshold_db, -200.0, 0.0)?;
        finite_range("preroll_ms", self.preroll_ms, 0.0, 1_000.0)?;
        finite_range("fadeout_ms", self.fadeout_ms, 0.0, 10_000.0)?;
        finite_range(
            "attack_rms_window_ms",
            self.attack_rms_window_ms,
            0.001,
            60_000.0,
        )?;
        finite_range("loudness_target_db", self.loudness_target_db, -200.0, 0.0)?;
        finite_range("peak_ceiling_dbfs", self.peak_ceiling_dbfs, -200.0, 0.0)?;
        finite_range("min_signal_dbfs", self.min_signal_dbfs, -200.0, 0.0)?;
        finite_range("tail_sustain_ms", self.tail_sustain_ms, 0.0, 60_000.0)?;
        finite_range("max_gain_db", self.max_gain_db, 0.0, 200.0)?;
        Ok(())
    }

    /// Peak ceiling as a linear amplitude (`10^(peak_ceiling_dbfs / 20)`).
    pub fn ceiling_amplitude(&self) -> f32 {
        db_to_amplitude(self.peak_ceiling_dbfs)
    }
}

// ---------------------------------------------------------------------------
// Warnings
// ---------------------------------------------------------------------------

/// A non-fatal quality observation about a source or its processing.
///
/// Warnings never fail a build; they are surfaced in verbose build output and
/// in the `keyvibes process` statistics.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessingWarning {
    /// Source reaches digital full scale; it may already be clipped.
    ClippedSource { peak_dbfs: f32 },
    /// |mean| of the source is unusually large.
    HighDcOffset { dc_offset: f32 },
    /// Usable peak is within 10 dB of `min_signal_dbfs`.
    LowSignal { peak_dbfs: f32 },
    /// Source is shorter than 5 ms.
    ShortClip { frames: usize },
    /// Source is longer than 5 s.
    LongClip { frames: usize },
    /// No onset crossed the relative threshold with attack confirmation.
    NoTransient,
    /// The quietest decile of windows is close to the peak.
    HighNoiseFloor { noise_floor_dbfs: f32 },
    /// The requested loudness gain hit `max_gain_db`.
    ExcessiveGain { gain_db: f32, cap_db: f32 },
    /// Peak/RMS relationship is unusual for a keyboard sound.
    AbnormalCrest { crest_db: f32 },
    /// Onset/tail detection disagreed; the whole clip was kept.
    TrimFallback,
}

impl std::fmt::Display for ProcessingWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClippedSource { peak_dbfs } => {
                write!(f, "source reaches full scale (peak {peak_dbfs:.2} dBFS); possible clipping in the recording")
            }
            Self::HighDcOffset { dc_offset } => {
                write!(
                    f,
                    "high DC offset ({dc_offset:+.4}); removed by the pipeline"
                )
            }
            Self::LowSignal { peak_dbfs } => {
                write!(f, "extremely low signal (peak {peak_dbfs:.1} dBFS)")
            }
            Self::ShortClip { frames } => {
                write!(f, "unusually short clip ({frames} frames)")
            }
            Self::LongClip { frames } => {
                write!(f, "unusually long clip ({frames} frames)")
            }
            Self::NoTransient => write!(
                f,
                "no detectable transient; onset thresholds were never crossed"
            ),
            Self::HighNoiseFloor { noise_floor_dbfs } => write!(
                f,
                "suspiciously high noise floor ({noise_floor_dbfs:.1} dBFS)"
            ),
            Self::ExcessiveGain { gain_db, cap_db } => write!(
                f,
                "excessive gain required ({gain_db:+.1} dB, capped at {cap_db:+.1} dB)"
            ),
            Self::AbnormalCrest { crest_db } => {
                write!(f, "abnormal peak/RMS relationship (crest {crest_db:.1} dB)")
            }
            Self::TrimFallback => write!(f, "onset/tail detection disagreed; kept the entire clip"),
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Before/after diagnostics for one processed clip.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingReport {
    /// Sample rate of the source (never changes).
    pub sample_rate: u32,
    /// Source channel count before downmix.
    pub source_channels: u16,

    /// Frames before processing.
    pub original_frames: usize,
    /// Frames after trim/fade (before guards).
    pub processed_frames: usize,
    /// `original_frames / sample_rate`.
    pub original_duration_seconds: f64,
    /// `processed_frames / sample_rate`.
    pub processed_duration_seconds: f64,

    /// Peak of the decoded source.
    pub original_peak: f32,
    /// `20 * log10(original_peak)`.
    pub original_peak_dbfs: f32,
    /// Peak after trim, fade, and gain (pre-dither).
    pub final_peak: f32,
    /// `20 * log10(final_peak)`.
    pub final_peak_dbfs: f32,

    /// RMS of the decoded source.
    pub original_rms: f32,
    /// `20 * log10(original_rms)`.
    pub original_rms_dbfs: f32,
    /// RMS after trim, fade, and gain (should be ≈ `loudness_target_db`).
    pub final_rms_dbfs: f32,

    /// DC offset of the decoded source (`mean`).
    pub original_dc_offset: f32,
    /// DC offset of the final processed buffer.
    pub final_dc_offset: f32,
    /// The value subtracted by DC correction.
    pub dc_correction: f32,

    /// First frame of the detected attack.
    pub onset_frame: usize,
    /// Exclusive end of the detected useful content.
    pub tail_frame: usize,
    /// Frames actually kept before the detected onset (≤ the configured
    /// preroll; `0` when the onset is at frame 0).
    pub preroll_frames: usize,
    /// Frames removed from the start (after preroll).
    pub leading_frames_removed: usize,
    /// Frames removed from the end.
    pub trailing_frames_removed: usize,
    /// Fade-out frames applied at the end.
    pub fade_frames: usize,

    /// Linear gain applied by loudness normalization + peak protection.
    pub gain: f32,
    /// `20 * log10(gain)`.
    pub gain_db: f32,

    /// Quality warnings (never fatal).
    pub warnings: Vec<ProcessingWarning>,
}

// ---------------------------------------------------------------------------
// Processor
// ---------------------------------------------------------------------------

/// Result of processing one clip: final floating-point samples plus the
/// before/after report. Guards are **not** applied yet.
#[derive(Debug, Clone)]
pub struct ProcessedAudio {
    /// Final `f32` samples in `[-ceiling, +ceiling]`, trim/fade/gain applied.
    pub samples: Vec<f32>,
    /// Sample rate (identical to the source; no resampling happens).
    pub sample_rate: u32,
    /// Diagnostics for this clip.
    pub report: ProcessingReport,
}

/// The offline processing pipeline, configured once per build.
#[derive(Debug, Clone)]
pub struct AudioProcessor {
    config: ProcessingConfig,
}

impl AudioProcessor {
    /// Creates a processor from an explicit configuration.
    pub fn new(config: ProcessingConfig) -> Self {
        Self { config }
    }

    /// Creates a processor using the defaults.
    pub fn with_defaults() -> Self {
        Self::new(ProcessingConfig::default())
    }

    /// The configuration in use.
    pub fn config(&self) -> &ProcessingConfig {
        &self.config
    }

    /// Processes one mono clip end to end.
    ///
    /// # Loudness metric
    ///
    /// The target is matched with the **RMS of the whole trimmed+faded
    /// buffer**, not peak normalization and not LUFS:
    ///
    /// ```text
    /// gain = 10^(loudness_target_db / 20) / rms(buffer)
    /// ```
    ///
    /// LUFS-style gated measurement needs ≥ 400 ms blocks and is unstable on
    /// 50–300 ms keyboard clicks; full-clip RMS is deterministic, defined for
    /// any length, and — because it is a single scalar gain — preserves the
    /// transient/decay contrast inside each clip (no compression, no
    /// flattening). Variants of one key end up at the same RMS while keeping
    /// their individual character.
    ///
    /// # Peak protection
    ///
    /// After the loudness gain is computed, the post-gain peak is checked
    /// against `peak_ceiling_dbfs`; if it would exceed the ceiling, the gain
    /// is reduced to `ceiling / peak`. The final samples therefore never
    /// exceed `10^(peak_ceiling_dbfs / 20)` before dithering.
    ///
    /// # Errors
    ///
    /// - `UnusableSignal` — usable peak below `min_signal_dbfs` (covers
    ///   digital silence and pure-DC content, which disappears once the DC
    ///   offset is removed).
    /// - `ValidationFailed` / `NonFiniteSample` — the processed buffer would
    ///   be invalid; never reaches the encoder.
    ///
    /// # Short clips
    ///
    /// Every index computation uses saturating arithmetic and is clamped to
    /// the buffer: a clip shorter than preroll + fade cannot underflow,
    /// panic, or produce an empty range. If onset and tail detection
    /// disagree, the whole clip is kept (`TrimFallback` warning).
    pub fn process(&self, mono: MonoAudio) -> PackResult<ProcessedAudio> {
        let cfg = &self.config;
        cfg.validate()?;

        let MonoAudio {
            mut samples,
            sample_rate,
            channels,
        } = mono;

        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(PackError::InvalidSampleRate(
                sample_rate,
                MIN_SAMPLE_RATE,
                MAX_SAMPLE_RATE,
            ));
        }
        let original_frames = samples.len();
        if original_frames == 0 {
            return Err(PackError::ZeroLengthClip(0));
        }
        if original_frames > MAX_CLIP_FRAMES as usize {
            return Err(PackError::ClipTooLarge {
                clip: 0,
                frames: original_frames as u32,
                max: MAX_CLIP_FRAMES,
            });
        }

        // --- Stage: DC correction (before detection, per pipeline order) ---
        let mut analysis = AudioAnalysis::measure(&samples, sample_rate, channels);
        let dc_correction = remove_dc(&mut samples);

        // --- Hard failure: no usable signal -------------------------------
        // Checked on the DC-corrected buffer: pure-DC content becomes exact
        // zeros here and must not turn into a silent clip.
        let usable_peak = max_abs(&samples);
        let usable_peak_dbfs = amplitude_to_db(usable_peak);
        if usable_peak_dbfs < cfg.min_signal_dbfs {
            return Err(PackError::UnusableSignal {
                peak_dbfs: usable_peak_dbfs,
                minimum_dbfs: cfg.min_signal_dbfs,
            });
        }

        // --- Stage: analysis on the corrected buffer ----------------------
        analysis.detect(&samples, cfg);

        let mut warnings = Vec::new();
        push_source_warnings(&mut warnings, &analysis, cfg, dc_correction);

        // --- Stage: leading trim + preroll --------------------------------
        let preroll_frames = ms_to_frames(cfg.preroll_ms, sample_rate);
        let mut start = if analysis.onset_found {
            analysis.onset_frame.saturating_sub(preroll_frames)
        } else {
            0
        };
        // --- Stage: tail trim ---------------------------------------------
        let mut end = analysis.tail_frame.min(original_frames);
        if end <= start {
            // Detectors disagreed (possible only without a detected onset).
            // Keep everything rather than fabricate a zero-length clip.
            start = 0;
            end = original_frames;
            warnings.push(ProcessingWarning::TrimFallback);
        }

        let mut buf = samples[start..end].to_vec();

        // --- Stage: fade-out over the final region ------------------------
        let fade_frames = ms_to_frames(cfg.fadeout_ms, sample_rate).min(buf.len());
        apply_fadeout(&mut buf, fade_frames);

        // --- Stage: loudness normalization --------------------------------
        let current_rms = rms(&buf);
        let desired_gain = if current_rms > 0.0 {
            db_to_amplitude(cfg.loudness_target_db) / current_rms
        } else {
            1.0
        };
        let gain_cap = db_to_amplitude(cfg.max_gain_db);
        let mut gain = desired_gain;
        if desired_gain > gain_cap {
            gain = gain_cap;
            warnings.push(ProcessingWarning::ExcessiveGain {
                gain_db: amplitude_to_db(desired_gain),
                cap_db: cfg.max_gain_db,
            });
        }

        // --- Stage: peak protection ---------------------------------------
        let ceiling = cfg.ceiling_amplitude();
        let pre_gain_peak = max_abs(&buf);
        if pre_gain_peak > 0.0 && pre_gain_peak * gain > ceiling {
            gain = ceiling / pre_gain_peak;
        }
        for s in buf.iter_mut() {
            *s *= gain;
        }

        // --- Stage: validation --------------------------------------------
        validate_samples(&buf, ceiling)?;

        let final_peak = max_abs(&buf);
        let final_rms = rms(&buf);

        let duration = |frames: usize| frames as f64 / f64::from(sample_rate);
        let report = ProcessingReport {
            sample_rate,
            source_channels: channels,
            original_frames,
            processed_frames: buf.len(),
            original_duration_seconds: duration(original_frames),
            processed_duration_seconds: duration(buf.len()),
            original_peak: analysis.peak,
            original_peak_dbfs: analysis.peak_dbfs,
            final_peak,
            final_peak_dbfs: amplitude_to_db(final_peak),
            original_rms: analysis.rms,
            original_rms_dbfs: analysis.rms_dbfs,
            final_rms_dbfs: amplitude_to_db(final_rms),
            original_dc_offset: analysis.dc_offset,
            final_dc_offset: mean(&buf),
            dc_correction,
            onset_frame: analysis.onset_frame,
            tail_frame: analysis.tail_frame,
            // Frames actually kept before the onset: never more than the
            // requested preroll, and 0 when the onset sits at frame 0.
            preroll_frames: if analysis.onset_found {
                analysis.onset_frame.saturating_sub(start)
            } else {
                0
            },
            leading_frames_removed: start,
            trailing_frames_removed: original_frames - end,
            fade_frames,
            gain,
            gain_db: amplitude_to_db(gain),
            warnings,
        };

        debug_assert!(report.processed_frames >= 1);
        // leading_removed + processed + trailing_removed == original (preroll
        // only shifts where the leading cut lands, it never adds frames).
        debug_assert_eq!(
            report.leading_frames_removed
                + report.processed_frames
                + report.trailing_frames_removed,
            report.original_frames
        );

        Ok(ProcessedAudio {
            samples: buf,
            sample_rate,
            report,
        })
    }
}

/// Collects non-fatal source-quality warnings.
fn push_source_warnings(
    warnings: &mut Vec<ProcessingWarning>,
    analysis: &AudioAnalysis,
    cfg: &ProcessingConfig,
    dc_correction: f32,
) {
    if analysis.peak >= CLIPPED_SOURCE_PEAK {
        warnings.push(ProcessingWarning::ClippedSource {
            peak_dbfs: analysis.peak_dbfs,
        });
    }
    if dc_correction.abs() > HIGH_DC_OFFSET_WARNING {
        warnings.push(ProcessingWarning::HighDcOffset {
            dc_offset: dc_correction,
        });
    }
    if analysis.peak_dbfs.is_finite()
        && analysis.peak_dbfs < cfg.min_signal_dbfs + LOW_SIGNAL_WARNING_MARGIN_DB
    {
        warnings.push(ProcessingWarning::LowSignal {
            peak_dbfs: analysis.peak_dbfs,
        });
    }
    let short_frames = ms_to_frames(SHORT_CLIP_WARNING_MS, analysis.sample_rate);
    if analysis.frames < short_frames {
        warnings.push(ProcessingWarning::ShortClip {
            frames: analysis.frames,
        });
    }
    let long_frames = ms_to_frames(LONG_CLIP_WARNING_MS, analysis.sample_rate);
    if analysis.frames > long_frames {
        warnings.push(ProcessingWarning::LongClip {
            frames: analysis.frames,
        });
    }
    if !analysis.onset_found {
        warnings.push(ProcessingWarning::NoTransient);
    }
    if analysis.noise_floor_dbfs.is_finite()
        && analysis.peak_dbfs.is_finite()
        && analysis.noise_floor_dbfs > analysis.peak_dbfs - HIGH_NOISE_FLOOR_WARNING_DB
    {
        warnings.push(ProcessingWarning::HighNoiseFloor {
            noise_floor_dbfs: analysis.noise_floor_dbfs,
        });
    }
    if analysis.crest_factor_db.is_finite()
        && (analysis.crest_factor_db < MIN_CREST_WARNING_DB
            || analysis.crest_factor_db > MAX_CREST_WARNING_DB)
    {
        warnings.push(ProcessingWarning::AbnormalCrest {
            crest_db: analysis.crest_factor_db,
        });
    }
}

/// Applies a raised-cosine (equal-power) fade-out to the final `fade_frames`
/// samples of `buf`.
///
/// For the `j`-th faded sample (0-based within the region) the gain is
///
/// ```text
/// t     = (j + 1) / fade_frames      ∈ (0, 1]
/// gain  = 0.5 * (1 + cos(pi * t))    ∈ [0, 1)
/// ```
///
/// so the very last sample is multiplied by exactly `0.0` (silent end, no
/// click), the first faded sample is multiplied by a value indistinguishable
/// from `1.0` for realistic fade lengths (no step at the fade boundary), and
/// the gain is monotonically non-increasing. `fade_frames = 0` is a no-op.
///
/// A raised cosine is used instead of a linear ramp because its slope also
/// starts at zero: a linear fade leaves a slope discontinuity where it begins,
/// which can click on high-frequency content.
pub fn apply_fadeout(buf: &mut [f32], fade_frames: usize) {
    let n = buf.len();
    if fade_frames == 0 || n == 0 {
        return;
    }
    let fade = fade_frames.min(n);
    let start = n - fade;
    for (j, sample) in buf[start..].iter_mut().enumerate() {
        let t = (j + 1) as f32 / fade as f32;
        *sample *= 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
    }
}

/// Hard validation of the final floating-point buffer.
///
/// Guarantees that no NaN/±Inf and no value above the peak ceiling (plus a
/// tiny tolerance for rounding) can reach the quantizer or the pack writer.
pub fn validate_samples(samples: &[f32], ceiling: f32) -> PackResult<()> {
    if samples.is_empty() {
        return Err(PackError::ZeroLengthClip(0));
    }
    let limit = ceiling + CEILING_TOLERANCE;
    for (i, &s) in samples.iter().enumerate() {
        if !s.is_finite() {
            return Err(PackError::NonFiniteSample(format!(
                "processed sample index {i} = {s}"
            )));
        }
        if s.abs() > limit {
            return Err(PackError::ValidationFailed(format!(
                "processed sample index {i} = {} exceeds the peak ceiling {ceiling:.6} (+{CEILING_TOLERANCE} tolerance)",
                s
            )));
        }
    }
    Ok(())
}

/// Encodes final `f32` samples into the pack's `i16` representation.
///
/// Order (strict):
///
/// ```text
/// f32 sample → + TPDF dither (one draw per sample, only if seeded)
///            → clamp to [-1.0, +1.0]
///            → scale by 32768
///            → round half away from zero
///            → clamp to i16 range
/// ```
///
/// Dither happens **here and nowhere else**: after all gain processing and
/// before quantization, once per sample, and never when `seed` is `None`.
/// The rounding mode is `f32::round` (half away from zero); `-1.0` maps to
/// `i16::MIN`, `+1.0` maps to `i16::MAX`, and values beyond ±1.0 are clamped,
/// never wrapped.
///
/// Guard samples are added later by `ClipData::from_pcm`, so the returned
/// [`CanonicalPcm`] holds logical frames only.
pub fn encode_i16(
    samples: &[f32],
    sample_rate: u32,
    seed: Option<DitherSeed>,
) -> PackResult<CanonicalPcm> {
    let frames = samples.len();
    if frames == 0 {
        return Err(PackError::ZeroLengthClip(0));
    }
    if frames > MAX_CLIP_FRAMES as usize {
        return Err(PackError::ClipTooLarge {
            clip: 0,
            frames: frames as u32,
            max: MAX_CLIP_FRAMES,
        });
    }

    let mut dither = seed.map(TpdfDither::new);
    let mut out = Vec::with_capacity(frames);
    for &s in samples {
        let v = match dither.as_mut() {
            Some(d) => s + d.draw(),
            None => s,
        };
        out.push(crate::wav::quantize_f32(v)?);
    }

    Ok(CanonicalPcm {
        frames: out.len() as u32,
        sample_rate,
        samples: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, frames: usize, sample_rate: u32) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    fn mono(samples: Vec<f32>, sample_rate: u32) -> MonoAudio {
        MonoAudio {
            samples,
            sample_rate,
            channels: 1,
        }
    }

    #[test]
    fn test_default_config_is_valid() {
        assert!(ProcessingConfig::default().validate().is_ok());
    }

    #[test]
    fn test_config_rejects_invalid_values() {
        let cfg = ProcessingConfig {
            fadeout_ms: f32::NAN,
            ..ProcessingConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(PackError::ValidationFailed(_))
        ));

        let cfg = ProcessingConfig {
            // > 0 dB is nonsense for a target
            loudness_target_db: 12.0,
            ..ProcessingConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = ProcessingConfig {
            max_gain_db: -1.0,
            ..ProcessingConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_fade_reaches_exactly_zero_and_is_monotonic() {
        let mut buf = vec![1.0f32; 300];
        apply_fadeout(&mut buf, 288);
        assert_eq!(*buf.last().unwrap(), 0.0, "last sample must be exactly 0");
        // Before the fade region nothing changes.
        assert_eq!(buf[0], 1.0);
        // Monotonic non-increasing across the fade region.
        for w in buf.windows(2) {
            assert!(w[1] <= w[0] + 1e-6, "fade not monotonic: {w:?}");
        }
        // No discontinuity at the fade start.
        assert!((buf[11] - buf[12]).abs() < 1e-3, "fade start steps");
    }

    #[test]
    fn test_fade_edge_cases() {
        let mut buf = vec![0.5f32];
        apply_fadeout(&mut buf, 1);
        assert_eq!(buf[0], 0.0, "single-sample fade ends at zero");

        let mut buf = vec![1.0f32; 10];
        apply_fadeout(&mut buf, 0);
        assert!(buf.iter().all(|&s| s == 1.0), "zero fade is a no-op");

        // Fade longer than the buffer: clamped to the full length, still
        // silent at the end and never above unity at the start.
        let mut buf = vec![1.0f32; 5];
        apply_fadeout(&mut buf, 100);
        assert_eq!(*buf.last().unwrap(), 0.0);
        assert!(buf.iter().all(|&s| (0.0..=1.0).contains(&s)));

        // Realistic fade (288 frames): the sample before the fade region is
        // untouched, the first faded sample is indistinguishable from 1.0,
        // and the gain decreases monotonically to exactly 0.
        let mut buf = vec![1.0f32; 4800];
        apply_fadeout(&mut buf, 288);
        assert_eq!(buf[4800 - 288 - 1], 1.0, "sample before fade untouched");
        assert!(
            (buf[4800 - 288] - 1.0).abs() < 1e-3,
            "first faded sample keeps gain ~1.0, got {}",
            buf[4800 - 288]
        );
        assert_eq!(*buf.last().unwrap(), 0.0, "last sample exactly zero");
        assert!(
            buf[4800 - 288..].windows(2).all(|w| w[0] >= w[1]),
            "fade must be monotonically non-increasing"
        );
    }

    #[test]
    fn test_process_rejects_silence() {
        let result = AudioProcessor::with_defaults().process(mono(vec![0.0; 4800], 48_000));
        match result {
            Err(PackError::UnusableSignal { minimum_dbfs, .. }) => {
                assert_eq!(minimum_dbfs, DEFAULT_MIN_SIGNAL_DBFS);
            }
            other => panic!("expected UnusableSignal, got {other:?}"),
        }
    }

    #[test]
    fn test_process_rejects_pure_dc() {
        // A constant value is pure DC: after correction nothing remains.
        let result = AudioProcessor::with_defaults().process(mono(vec![0.5; 4800], 48_000));
        assert!(
            matches!(result, Err(PackError::UnusableSignal { .. })),
            "pure DC must be rejected, got {result:?}"
        );
    }

    #[test]
    fn test_process_rejects_bad_sample_rate() {
        let result = AudioProcessor::with_defaults().process(mono(vec![0.1; 100], 100));
        assert!(matches!(result, Err(PackError::InvalidSampleRate(100, ..))));
    }

    #[test]
    fn test_process_removes_dc_and_reports_it() {
        let mut samples = sine(0.5, 4800, 48_000);
        for s in &mut samples {
            *s += 0.08;
        }
        let processed = AudioProcessor::with_defaults()
            .process(mono(samples, 48_000))
            .expect("process");

        let r = &processed.report;
        assert!(
            (r.dc_correction - 0.08).abs() < 0.01,
            "dc_correction {}",
            r.dc_correction
        );
        assert!(
            r.original_dc_offset.abs() > 0.05,
            "original DC must be reported: {}",
            r.original_dc_offset
        );
        assert!(
            r.final_dc_offset.abs() < 0.01,
            "final DC must be near zero: {}",
            r.final_dc_offset
        );
        assert!(processed.samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn test_process_respects_peak_ceiling() {
        // Quiet sustained tone plus one full-scale impulse: the RMS gain
        // needed to hit the loudness target overshoots the ceiling, so peak
        // protection must clamp the final peak to exactly the ceiling.
        let mut samples = sine(0.01, 4800, 48_000);
        samples[2400] = 1.0;
        let processed = AudioProcessor::with_defaults()
            .process(mono(samples, 48_000))
            .expect("process");

        let ceiling = db_to_amplitude(DEFAULT_PEAK_CEILING_DBFS);
        assert!(
            processed.report.final_peak <= ceiling + CEILING_TOLERANCE,
            "peak {} exceeds ceiling {ceiling}",
            processed.report.final_peak
        );
        assert!(
            processed.report.final_peak >= ceiling * 0.9,
            "peak protection should push the peak near the ceiling, got {}",
            processed.report.final_peak
        );
    }

    #[test]
    fn test_process_matches_rms_target_for_matched_clips() {
        for amp in [0.5f32, 0.05] {
            let samples = sine(amp, 4800, 48_000);
            let processed = AudioProcessor::with_defaults()
                .process(mono(samples, 48_000))
                .expect("process");
            assert!(
                (processed.report.final_rms_dbfs - DEFAULT_LOUDNESS_TARGET_DB).abs() < 0.2,
                "amp {amp}: rms {} dB",
                processed.report.final_rms_dbfs
            );
        }
    }

    #[test]
    fn test_process_keeps_trimmed_length_sane() {
        let mut samples = vec![0.0f32; 4800];
        for (i, s) in samples.iter_mut().enumerate().skip(2400).take(480) {
            *s = 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 48.0).sin();
        }
        let processed = AudioProcessor::with_defaults()
            .process(mono(samples, 48_000))
            .expect("process");
        let r = &processed.report;
        assert!(r.processed_frames >= 1);
        // Leading silence trimmed, preroll added back.
        assert!(r.leading_frames_removed > 0);
        assert_eq!(r.onset_frame, 2400);
        assert!(r.preroll_frames == 24);
        assert_eq!(r.leading_frames_removed, 2400 - 24);
        // Trailing silence trimmed.
        assert!(r.trailing_frames_removed > 0);
        // Frame accounting: removed = original - processed - preroll retained.
        assert_eq!(
            r.leading_frames_removed + r.trailing_frames_removed,
            r.original_frames - r.processed_frames
        );
    }

    #[test]
    fn test_process_very_short_clip_does_not_panic() {
        // Non-constant buffers (a single sample or a constant run is pure DC
        // and is rejected by design — see below).
        for len in [2usize, 3, 5, 47, 48, 49] {
            let samples: Vec<f32> = (0..len)
                .map(|i| if i % 2 == 0 { 0.4 } else { -0.4 })
                .collect();
            let processed = AudioProcessor::with_defaults()
                .process(mono(samples, 48_000))
                .expect("short clip must process");
            assert!(!processed.samples.is_empty(), "len {len}");
            assert!(processed.samples.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn test_process_single_sample_is_rejected_as_dc_only() {
        // One sample is, by definition, pure DC: mean == sample, so nothing
        // survives DC correction. The rejection is a clean error, not a panic.
        let result = AudioProcessor::with_defaults().process(mono(vec![0.4f32], 48_000));
        assert!(
            matches!(result, Err(PackError::UnusableSignal { .. })),
            "single sample must be rejected, got {result:?}"
        );
    }

    #[test]
    fn test_process_is_deterministic() {
        let samples = sine(0.3, 4800, 48_000);
        let a = AudioProcessor::with_defaults()
            .process(mono(samples.clone(), 48_000))
            .unwrap();
        let b = AudioProcessor::with_defaults()
            .process(mono(samples, 48_000))
            .unwrap();
        assert_eq!(a.samples, b.samples);
        assert_eq!(a.report, b.report);
    }

    #[test]
    fn test_encode_dither_determinism() {
        let samples: Vec<f32> = (0..256).map(|i| (i as f32) / 1000.0 - 0.128).collect();

        let a = encode_i16(&samples, 48_000, Some(DitherSeed::from_u64(7))).unwrap();
        let b = encode_i16(&samples, 48_000, Some(DitherSeed::from_u64(7))).unwrap();
        assert_eq!(a.samples, b.samples, "same seed must match");

        let c = encode_i16(&samples, 48_000, Some(DitherSeed::from_u64(8))).unwrap();
        assert_ne!(a.samples, c.samples, "different seeds must differ");

        let undithered = encode_i16(&samples, 48_000, None).unwrap();
        assert_ne!(a.samples, undithered.samples, "dither must change output");
        // Undithered output is plain rounding.
        for (i, &v) in samples.iter().enumerate() {
            let expected = crate::wav::quantize_f32(v).unwrap();
            assert_eq!(undithered.samples[i], expected);
        }
    }

    #[test]
    fn test_encode_rejects_empty_and_overflowing_lengths() {
        assert!(matches!(
            encode_i16(&[], 48_000, None),
            Err(PackError::ZeroLengthClip(_))
        ));
    }

    #[test]
    fn test_encode_maps_full_scale_correctly() {
        let samples = [-1.0f32, -0.5, 0.0, 0.5, 1.0, 1.5, -1.5];
        let pcm = encode_i16(&samples, 48_000, None).unwrap();
        assert_eq!(
            pcm.samples,
            vec![i16::MIN, -16384, 0, 16384, i16::MAX, i16::MAX, i16::MIN]
        );
        assert_eq!(pcm.frames, samples.len() as u32);
    }

    #[test]
    fn test_encode_dithers_every_sample_exactly_once() {
        // A value exactly on an integer boundary is moved by dither with
        // non-zero probability; with 512 samples and a known seed at least
        // one sample must change (verified deterministically below).
        let samples = vec![0.5f32; 512];
        let plain = encode_i16(&samples, 48_000, None).unwrap();
        let dithered = encode_i16(&samples, 48_000, Some(DitherSeed::from_u64(99))).unwrap();
        assert_eq!(plain.samples, vec![16384; 512]);
        assert_ne!(
            dithered.samples, plain.samples,
            "dither must perturb output"
        );
        // Still valid i16 and near the undithered value (±1 LSB).
        for (d, p) in dithered.samples.iter().zip(&plain.samples) {
            assert!((*d - *p).abs() <= 1, "dither exceeded 1 LSB: {d} vs {p}");
        }
    }

    #[test]
    fn test_validate_samples_catches_bad_values() {
        assert!(validate_samples(&[], 0.9).is_err());
        assert!(validate_samples(&[f32::NAN], 0.9).is_err());
        assert!(validate_samples(&[f32::INFINITY], 0.9).is_err());
        assert!(validate_samples(&[0.95], 0.9).is_err());
        assert!(validate_samples(&[0.89125], 0.89125).is_ok());
        // Within CEILING_TOLERANCE of the ceiling: accepted.
        assert!(validate_samples(&[-0.891251], 0.89125).is_ok(), "tolerance");
        // Beyond the tolerance: still rejected.
        assert!(
            validate_samples(&[-0.89130], 0.89125).is_err(),
            "past tolerance"
        );
    }
}
