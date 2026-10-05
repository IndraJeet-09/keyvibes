//! Audio analysis: measurable, explainable properties of a source clip.
//!
//! Analysis is a **pure** stage (`&[f32]` in, [`AudioAnalysis`] out). It never
//! mutates the buffer and never decides how audio is modified — it only
//! measures, so processing decisions become testable and the `keyvibes
//! analyze` CLI reports exactly what the pipeline sees.
//!
//! # Two steps
//!
//! 1. [`AudioAnalysis::measure`] — statistics of the buffer as given
//!    (peak, RMS, DC offset, noise floor, crest factor).
//! 2. [`AudioAnalysis::detect`] — onset/tail/trim detection, run on the
//!    **DC-corrected** buffer (a DC offset above the silence floor would
//!    otherwise make silence look like signal).
//!
//! [`AudioAnalysis::analyze`] chains both for callers that already hold a
//! DC-free buffer.
//!
//! # Detection method (documented once, used everywhere)
//!
//! The buffer is cut into non-overlapping **1 ms windows**
//! ([`ENVELOPE_WINDOW_MS`]). For each window we record
//!
//! ```text
//! peak_env[k] = max |x|  over the window
//! rms_env[k]  = sqrt(mean(x^2)) over the window
//! ```
//!
//! - **Silence** = windows whose `peak_env` never reaches
//!   `10^(silence_threshold_dbfs / 20)` (absolute, dBFS).
//! - **Onset** = the first window whose `peak_env` reaches the
//!   peak-relative onset threshold *and* whose following attack window
//!   (60 ms, clamped) has an RMS reaching the same threshold scaled by
//!   `1/sqrt(2)` — the RMS a steady tone at that peak would have. This
//!   rejects isolated one-window blips while still accepting single-sample
//!   transients (an impulse's 60 ms RMS is far above the threshold).
//! - **Tail** = the start of the trailing run of below-threshold windows,
//!   but only when that run lasts at least `tail_sustain_ms`; otherwise the
//!   clip is kept to its end. A brief dip therefore never truncates a
//!   following resonance.
//!
//! Thresholds are clamped to at least the silence floor, so no detector ever
//! reacts to content below the silence threshold.

use crate::dsp::{amplitude_to_db, max_abs, ms_to_frames, rms};
use crate::processing::ProcessingConfig;

/// Granularity of the detection envelope, in milliseconds. Small enough that
/// a transient is localized within ~1 ms, large enough that a single noisy
/// sample cannot cross a threshold on its own amplitude.
pub const ENVELOPE_WINDOW_MS: f32 = 1.0;

/// Quantile of the 1 ms window RMS levels reported as the noise floor.
/// The quietest 10 % of windows characterize the recording's noise/silence
/// level even when the clip contains loud content.
pub const NOISE_FLOOR_PERCENTILE: f32 = 0.10;

/// What the analysis measured about one clip.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioAnalysis {
    /// Sample rate of the source.
    pub sample_rate: u32,
    /// Source channel count (1 = mono, 2 = stereo; the analysis itself runs
    /// on mono).
    pub channels: u16,
    /// Number of analyzed (mono) frames.
    pub frames: usize,
    /// `frames / sample_rate`.
    pub duration_seconds: f64,

    /// Minimum sample value.
    pub min_sample: f32,
    /// Maximum sample value.
    pub max_sample: f32,
    /// `max(|min|, |max|)`.
    pub peak: f32,
    /// `20 * log10(peak)`; `f32::NEG_INFINITY` for digital silence.
    pub peak_dbfs: f32,
    /// Root mean square over the whole clip.
    pub rms: f32,
    /// `20 * log10(rms)`; `f32::NEG_INFINITY` for digital silence.
    pub rms_dbfs: f32,
    /// Arithmetic mean of the samples (`sample mean != 0` ⇒ DC offset).
    pub dc_offset: f32,
    /// Quietest-decile window RMS, in dBFS (see [`NOISE_FLOOR_PERCENTILE`]).
    pub noise_floor_dbfs: f32,
    /// `peak_dbfs - rms_dbfs`; `0.0` for silence, `f32::INFINITY` for an
    /// infinitely sparse signal (RMS 0 with non-zero peak).
    pub crest_factor_db: f32,

    /// First frame of the detected attack (before preroll). Meaningful only
    /// when [`Self::onset_found`] is true.
    pub onset_frame: usize,
    /// Whether a threshold-crossing with sufficient attack energy was found.
    pub onset_found: bool,
    /// Exclusive end of the useful content (after tail detection).
    pub tail_frame: usize,
    /// RMS of `[onset, onset + attack window)` (clamped to the clip), in dBFS.
    /// This is the same measurement used to confirm the onset.
    pub attack_rms_dbfs: f32,
    /// Frames before [`Self::onset_frame`] (`== onset_frame`).
    pub leading_trim_frames: usize,
    /// Frames after [`Self::tail_frame`] (`== frames - tail_frame`).
    pub trailing_trim_frames: usize,
}

impl AudioAnalysis {
    /// Computes the scalar statistics of `samples` (does not run detection).
    ///
    /// Runs in O(n): one pass for min/max/sum/sum-of-squares, one pass for
    /// the windowed noise-floor levels.
    pub fn measure(samples: &[f32], sample_rate: u32, channels: u16) -> Self {
        let frames = samples.len();

        let mut min_sample = 0.0f32;
        let mut max_sample = 0.0f32;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        for (i, &s) in samples.iter().enumerate() {
            if i == 0 {
                min_sample = s;
                max_sample = s;
            } else {
                min_sample = min_sample.min(s);
                max_sample = max_sample.max(s);
            }
            sum += f64::from(s);
            sum_sq += f64::from(s) * f64::from(s);
        }

        let (peak, rms_value) = if frames == 0 {
            (0.0, 0.0)
        } else {
            (
                max_sample.abs().max(min_sample.abs()),
                (sum_sq / frames as f64).sqrt() as f32,
            )
        };

        let peak_dbfs = amplitude_to_db(peak);
        let rms_dbfs = amplitude_to_db(rms_value);
        let dc_offset = if frames == 0 {
            0.0
        } else {
            (sum / frames as f64) as f32
        };

        let crest_factor_db = if rms_value > 0.0 {
            amplitude_to_db(peak / rms_value)
        } else if peak > 0.0 {
            f32::INFINITY
        } else {
            0.0
        };

        let noise_floor_dbfs = if frames == 0 {
            f32::NEG_INFINITY
        } else {
            let window_frames = ms_to_frames(ENVELOPE_WINDOW_MS, sample_rate).max(1);
            let (_, rms_levels) = window_levels(samples, window_frames);
            percentile_db(&rms_levels, NOISE_FLOOR_PERCENTILE)
        };

        Self {
            sample_rate,
            channels,
            frames,
            duration_seconds: if sample_rate > 0 {
                frames as f64 / f64::from(sample_rate)
            } else {
                0.0
            },
            min_sample,
            max_sample,
            peak,
            peak_dbfs,
            rms: rms_value,
            rms_dbfs,
            dc_offset,
            noise_floor_dbfs,
            crest_factor_db,
            onset_frame: 0,
            onset_found: false,
            tail_frame: 0,
            attack_rms_dbfs: f32::NEG_INFINITY,
            leading_trim_frames: 0,
            trailing_trim_frames: 0,
        }
    }

    /// Runs onset/tail/trim detection on `samples`.
    ///
    /// `samples` must be the DC-corrected version of the buffer that was
    /// measured (same length); relative thresholds are recomputed from this
    /// buffer's own peak so detection is independent of the raw DC offset.
    ///
    /// Must be called on a buffer whose length equals [`Self::frames`].
    pub fn detect(&mut self, samples: &[f32], cfg: &ProcessingConfig) {
        let n = samples.len();
        debug_assert_eq!(
            n, self.frames,
            "detect() must analyze the same buffer measure() did"
        );
        if n == 0 {
            self.onset_found = false;
            self.onset_frame = 0;
            self.tail_frame = 0;
            self.leading_trim_frames = 0;
            self.trailing_trim_frames = 0;
            return;
        }

        let window_frames = ms_to_frames(ENVELOPE_WINDOW_MS, self.sample_rate).max(1);
        let (peak_env, _) = window_levels(samples, window_frames);

        // Detection peak: the buffer actually being scanned (post-DC).
        let peak = max_abs(samples);
        let silence_lin = crate::dsp::db_to_amplitude(cfg.silence_threshold_dbfs);
        let onset_lin =
            (crate::dsp::db_to_amplitude(cfg.onset_threshold_db) * peak).max(silence_lin);
        let tail_lin = (crate::dsp::db_to_amplitude(cfg.tail_threshold_db) * peak).max(silence_lin);

        // Attack window: 60 ms, clamped to the available samples (a keyboard
        // clip can be far shorter than 60 ms).
        let attack_frames = ms_to_frames(cfg.attack_rms_window_ms, self.sample_rate)
            .max(1)
            .min(n);

        // Confirmation threshold: the RMS a steady tone with peak ==
        // onset_lin would have is onset_lin / sqrt(2).
        let confirm_lin = onset_lin * std::f32::consts::FRAC_1_SQRT_2;

        let mut onset_frame = 0usize;
        let mut onset_found = false;
        let mut attack_rms = f32::NEG_INFINITY;
        for (k, &env) in peak_env.iter().enumerate() {
            if env < onset_lin {
                continue;
            }
            let start = k * window_frames;
            let end = (start + attack_frames).min(n);
            let confirm = rms(&samples[start..end]);
            if confirm >= confirm_lin {
                onset_frame = start;
                onset_found = true;
                attack_rms = confirm;
                break;
            }
        }

        if !onset_found {
            // Nothing crossed the threshold (or nothing survived the attack
            // confirmation). Keep the whole clip from frame 0 and report the
            // attack energy of the opening window for diagnostics.
            attack_rms = rms(&samples[..attack_frames]);
        }

        // Tail: only declare the sound ended once the signal has stayed
        // below the tail threshold for `tail_sustain_ms`.
        let sustain_frames = ms_to_frames(cfg.tail_sustain_ms, self.sample_rate);
        let sustain_windows = sustain_frames.div_ceil(window_frames);
        let mut quiet_run = 0usize;
        for &env in peak_env.iter().rev() {
            if env < tail_lin {
                quiet_run += 1;
            } else {
                break;
            }
        }
        // Trailing quiet only counts as "the sound has ended" once it lasts
        // at least `tail_sustain_ms` (sustain_windows == 0 disables the
        // sustain and trims any trailing quiet immediately).
        let tail_frame = if quiet_run >= sustain_windows {
            ((peak_env.len() - quiet_run) * window_frames).min(n)
        } else {
            n
        };

        self.onset_frame = onset_frame;
        self.onset_found = onset_found;
        self.tail_frame = tail_frame.max(onset_frame.min(n));
        self.attack_rms_dbfs = amplitude_to_db(attack_rms);
        self.leading_trim_frames = self.onset_frame;
        self.trailing_trim_frames = n - self.tail_frame;
    }

    /// Convenience: [`Self::measure`] followed by [`Self::detect`] on the
    /// same buffer. The buffer must already be DC-corrected for detection to
    /// behave like the processing pipeline.
    pub fn analyze(
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        cfg: &ProcessingConfig,
    ) -> Self {
        let mut analysis = Self::measure(samples, sample_rate, channels);
        analysis.detect(samples, cfg);
        analysis
    }
}

/// Splits `samples` into `window_frames`-sized windows and returns
/// `(peak envelope, RMS envelope)`. The last window may be partial.
fn window_levels(samples: &[f32], window_frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut peaks = Vec::with_capacity(samples.len().div_ceil(window_frames.max(1)));
    let mut levels = Vec::with_capacity(peaks.capacity());
    for window in samples.chunks(window_frames.max(1)) {
        peaks.push(max_abs(window));
        levels.push(rms(window));
    }
    (peaks, levels)
}

/// The `quantile`-th smallest level of `levels`, converted to dBFS.
///
/// `quantile = 0.0` returns the minimum, `1.0` the maximum. Empty input
/// yields `f32::NEG_INFINITY`.
fn percentile_db(levels: &[f32], quantile: f32) -> f32 {
    if levels.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mut sorted = levels.to_vec();
    sorted.sort_by(f32::total_cmp);
    let idx = ((sorted.len() as f32 * quantile.clamp(0.0, 1.0)) as usize).min(sorted.len() - 1);
    amplitude_to_db(sorted[idx])
}

// Keep `mean` referenced from this module's docs/tests without a dead-code
// warning when the feature set changes.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::db_to_amplitude;

    fn default_cfg() -> ProcessingConfig {
        ProcessingConfig::default()
    }

    /// Builds a buffer: `leading` frames of silence, then `signal`, then
    /// `trailing` frames of silence.
    fn clip(leading: usize, signal: &[f32], trailing: usize) -> Vec<f32> {
        let mut v = vec![0.0; leading];
        v.extend_from_slice(signal);
        v.extend(std::iter::repeat(0.0).take(trailing));
        v
    }

    fn sine(freq: f32, amp: f32, frames: usize, sample_rate: u32) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    #[test]
    fn test_measure_silence() {
        let a = AudioAnalysis::measure(&[0.0; 100], 48_000, 1);
        assert_eq!(a.frames, 100);
        assert_eq!(a.peak, 0.0);
        assert_eq!(a.peak_dbfs, f32::NEG_INFINITY);
        assert_eq!(a.rms, 0.0);
        assert_eq!(a.dc_offset, 0.0);
        assert_eq!(a.crest_factor_db, 0.0);
        assert_eq!(a.duration_seconds, 100.0 / 48_000.0);
    }

    #[test]
    fn test_measure_stats() {
        // Peak 0.5, RMS 0.5/sqrt(2) for a sine.
        let s = sine(1000.0, 0.5, 4800, 48_000);
        let a = AudioAnalysis::measure(&s, 48_000, 1);
        assert!((a.peak - 0.5).abs() < 0.01, "peak {}", a.peak);
        assert!((a.rms - 0.5 / 2.0f32.sqrt()).abs() < 0.01, "rms {}", a.rms);
        assert!(a.dc_offset.abs() < 0.01, "dc {}", a.dc_offset);
        assert!(
            (a.crest_factor_db - 3.01).abs() < 0.2,
            "crest {}",
            a.crest_factor_db
        );
        assert!(a.min_sample < -0.49 && a.max_sample > 0.49);
        // A steady tone has no quiet section: its 10th-percentile window
        // sits right at the signal level, never above it.
        assert!(
            a.noise_floor_dbfs <= a.rms_dbfs + 1.0,
            "floor {}",
            a.noise_floor_dbfs
        );
    }

    #[test]
    fn test_noise_floor_finds_quiet_section() {
        // Loud tone followed by a quiet tone: the 10th-percentile window
        // must land in the quiet section, far below the peak.
        let mut s = sine(1000.0, 0.5, 4800, 48_000);
        s.extend(sine(1000.0, 0.001, 2400, 48_000));
        let a = AudioAnalysis::measure(&s, 48_000, 1);
        assert!(a.noise_floor_dbfs < -30.0, "floor {}", a.noise_floor_dbfs);
        assert!(
            a.noise_floor_dbfs < a.peak_dbfs - 40.0,
            "floor {} must sit far below the peak",
            a.noise_floor_dbfs
        );
    }

    #[test]
    fn test_measure_dc_offset() {
        let s = vec![0.08f32; 200];
        let a = AudioAnalysis::measure(&s, 48_000, 1);
        assert!((a.dc_offset - 0.08).abs() < 1e-6);
        assert!((a.peak - 0.08).abs() < 1e-6);
    }

    #[test]
    fn test_detect_leading_and_trailing_silence() {
        let cfg = default_cfg();
        let signal = sine(500.0, 0.5, 2400, 48_000); // 50 ms
        let buf = clip(2400, &signal, 2400); // 50 ms lead/trail

        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found);
        // Onset lands within 1 ms of the true signal start (window granularity)
        // and preroll is NOT part of the analysis value.
        assert!(
            a.onset_frame >= 2400 && a.onset_frame <= 2400 + 48,
            "onset {}",
            a.onset_frame
        );
        // Tail: trailing silence (50 ms) exceeds sustain (5 ms) and gets cut,
        // but only down to the last activity window (within 1 ms of signal end).
        assert!(
            a.tail_frame >= 4800 && a.tail_frame <= 4800 + 48,
            "tail {}",
            a.tail_frame
        );
        assert_eq!(a.leading_trim_frames, a.onset_frame);
        assert_eq!(a.trailing_trim_frames, buf.len() - a.tail_frame);
    }

    #[test]
    fn test_detect_impulse() {
        let cfg = default_cfg();
        let mut buf = vec![0.0f32; 4800];
        buf[2400] = 1.0;

        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found, "impulse must be detected");
        assert!(
            a.onset_frame >= 2352 && a.onset_frame <= 2400,
            "onset {}",
            a.onset_frame
        );
    }

    #[test]
    fn test_detect_is_below_threshold_ignores_noise_floor() {
        let cfg = default_cfg();
        // -70 dBFS noise (below the -55 dBFS silence floor) then a real signal.
        let noise_amp = db_to_amplitude(-70.0);
        let mut buf: Vec<f32> = (0..2400)
            .map(|i| noise_amp * if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        buf.extend(sine(440.0, 0.4, 2400, 48_000));

        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found);
        assert!(
            a.onset_frame >= 2400 - 48 && a.onset_frame <= 2400 + 48,
            "noise must not trigger onset, got {}",
            a.onset_frame
        );
        assert!(a.noise_floor_dbfs < -60.0, "floor {}", a.noise_floor_dbfs);
    }

    #[test]
    fn test_detect_keeps_brief_dip_before_resonance() {
        let cfg = default_cfg();
        // sound (10 ms) → 3 ms dip (below tail threshold) → resonance (10 ms)
        // → 20 ms silence. The dip is shorter than tail_sustain_ms, and the
        // resonance is above the threshold, so nothing may be cut between.
        let mut buf = sine(440.0, 0.5, 480, 48_000); // 10 ms
        buf.extend(std::iter::repeat(0.0).take(144)); // 3 ms dip
        buf.extend(sine(220.0, 0.1, 480, 48_000)); // 10 ms resonance
        buf.extend(std::iter::repeat(0.0).take(960)); // 20 ms silence

        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found);
        assert!(
            a.tail_frame >= 1104,
            "resonance after a brief dip must survive, tail={}",
            a.tail_frame
        );
        assert!(
            a.tail_frame <= 1104 + 48,
            "silence after resonance must be trimmed, tail={}",
            a.tail_frame
        );
    }

    #[test]
    fn test_detect_keeps_short_trailing_quiet_when_under_sustain() {
        let mut cfg = default_cfg();
        cfg.tail_sustain_ms = 5.0;

        // Signal, then only 2 ms of quiet (< 5 ms sustain): keep to the end.
        let mut buf = sine(440.0, 0.5, 960, 48_000); // 20 ms
        buf.extend(std::iter::repeat(0.0).take(96)); // 2 ms
        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert_eq!(a.tail_frame, buf.len(), "short trailing quiet is kept");

        // With sustain below the quiet length it is trimmed instead.
        let mut cfg2 = cfg.clone();
        cfg2.tail_sustain_ms = 1.0;
        let mut a2 = AudioAnalysis::measure(&buf, 48_000, 1);
        a2.detect(&buf, &cfg2);
        assert_eq!(a2.tail_frame, 960, "quiet longer than sustain is trimmed");
    }

    #[test]
    fn test_detect_pure_silence_finds_nothing() {
        let cfg = default_cfg();
        let buf = vec![0.0f32; 4800];
        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(!a.onset_found);
        assert_eq!(a.onset_frame, 0);
        assert_eq!(a.tail_frame, 0);
    }

    #[test]
    fn test_detect_very_short_clip() {
        let cfg = default_cfg();
        let buf = vec![0.5f32, -0.5, 0.25];
        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found, "constant signal must be detected");
        assert_eq!(a.onset_frame, 0);
        assert_eq!(a.tail_frame, buf.len());
        assert!(a.attack_rms_dbfs.is_finite());
    }

    #[test]
    fn test_detect_attack_window_clamped_for_short_clips() {
        // 60 ms attack window on a 5 ms clip must not panic or read out of
        // bounds; the RMS confirmation uses whatever is available.
        let cfg = default_cfg();
        let buf = sine(1000.0, 0.5, 240, 48_000); // 5 ms
        let mut a = AudioAnalysis::measure(&buf, 48_000, 1);
        a.detect(&buf, &cfg);
        assert!(a.onset_found);
        assert_eq!(a.onset_frame, 0);
    }

    #[test]
    fn test_analyze_convenience_matches_measure_plus_detect() {
        let cfg = default_cfg();
        let mut buf = clip(480, &sine(800.0, 0.4, 960, 48_000), 480);
        let dc = crate::dsp::remove_dc(&mut buf);

        let combined = AudioAnalysis::analyze(&buf, 48_000, 1, &cfg);
        let mut stepwise = AudioAnalysis::measure(&buf, 48_000, 1);
        stepwise.detect(&buf, &cfg);

        assert_eq!(combined, stepwise);
        assert!(dc.abs() < 0.1);
        assert!(combined.onset_found);
    }

    #[test]
    fn test_empty_buffer_is_safe() {
        let cfg = default_cfg();
        let a = AudioAnalysis::analyze(&[], 48_000, 1, &cfg);
        assert_eq!(a.frames, 0);
        assert!(!a.onset_found);
        assert_eq!(a.duration_seconds, 0.0);
    }
}
