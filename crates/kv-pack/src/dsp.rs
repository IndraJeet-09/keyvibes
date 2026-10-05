//! Shared DSP helpers for the offline audio processing pipeline.
//!
//! Builder-only: nothing in here is reachable from the real-time path.
//! All functions operate on `f32` samples in the conceptual range
//! `[-1.0, +1.0]`. Every helper is documented with its exact formula so the
//! implementation and `docs/audio-processing.md` cannot drift apart silently.

/// Converts decibels relative to full scale (dBFS) or relative decibels to a
/// linear amplitude:
///
/// ```text
/// amplitude = 10^(db / 20)
/// ```
pub(crate) fn db_to_amplitude(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Converts a non-negative linear amplitude to decibels:
///
/// ```text
/// db = 20 * log10(amplitude)
/// ```
///
/// Returns `f32::NEG_INFINITY` for exactly zero (log10(0) is undefined). The
/// input must not be negative; callers always pass absolute amplitudes or RMS
/// values.
pub(crate) fn amplitude_to_db(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return f32::NEG_INFINITY;
    }
    20.0 * amplitude.log10()
}

/// Converts a millisecond duration to a frame count:
///
/// ```text
/// frames = round(ms * sample_rate / 1000)
/// ```
///
/// The result is clamped to at least 1 frame for positive durations (a
/// positive sub-frame duration must not silently become "nothing") and at
/// least 0 for zero/negative durations. Callers that need "as many as are
/// available" clamp against the buffer length themselves.
pub(crate) fn ms_to_frames(ms: f32, sample_rate: u32) -> usize {
    if ms.is_nan() || ms <= 0.0 {
        return 0;
    }
    let frames = (f64::from(ms) * f64::from(sample_rate) / 1000.0).round();
    if !frames.is_finite() || frames <= 0.0 {
        1
    } else {
        // Frames can never exceed the clip limits we accept anywhere else
        // (MAX_CLIP_FRAMES is ~10M); saturating casts protect the conversion.
        frames as usize
    }
}

/// Arithmetic mean of the samples (`sum / n`), or `0.0` for an empty slice.
pub(crate) fn mean(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    // f64 accumulation keeps the error negligible even for very long clips.
    let sum: f64 = samples.iter().map(|&s| f64::from(s)).sum();
    (sum / samples.len() as f64) as f32
}

/// Root mean square of the samples:
///
/// ```text
/// rms = sqrt(sum(x^2) / n)
/// ```
///
/// Returns `0.0` for an empty slice. Accumulation happens in `f64`.
pub(crate) fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    ((sum / samples.len() as f64).sqrt()) as f32
}

/// Largest absolute sample value (`max |x|`), or `0.0` for an empty slice.
pub(crate) fn max_abs(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()))
}

/// Removes the global DC offset in place and returns the removed mean:
///
/// ```text
/// dc  = mean(samples)
/// x  -= dc
/// ```
///
/// This is a single global-mean pass. Keyboard clips are short (tens to
/// hundreds of milliseconds), so one mean over the whole clip is both cheap
/// and appropriate; a long clip would warrant a high-pass filter instead, and
/// that limitation is documented in `docs/audio-processing.md`.
pub(crate) fn remove_dc(samples: &mut [f32]) -> f32 {
    let dc = mean(samples);
    if dc != 0.0 && !samples.is_empty() {
        for s in samples.iter_mut() {
            *s -= dc;
        }
    }
    dc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_amplitude_round_trip() {
        assert!((db_to_amplitude(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_amplitude(-6.0) - 0.501187).abs() < 1e-5);
        assert!((db_to_amplitude(-20.0) - 0.1).abs() < 1e-6);
        assert!((db_to_amplitude(-45.0) - 0.0056234).abs() < 1e-6);
        assert!((db_to_amplitude(-55.0) - 0.0017783).abs() < 1e-6);
        assert!((db_to_amplitude(-1.0) - 0.891251).abs() < 1e-5);
        assert!((db_to_amplitude(-30.0) - 0.0316228).abs() < 1e-6);

        for db in [-120.0f32, -60.0, -20.0, -3.0, 0.0] {
            let amp = db_to_amplitude(db);
            assert!((amplitude_to_db(amp) - db).abs() < 1e-3, "db={db}");
        }
    }

    #[test]
    fn test_amplitude_to_db_edge_cases() {
        assert_eq!(amplitude_to_db(0.0), f32::NEG_INFINITY);
        assert!((amplitude_to_db(1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_ms_to_frames() {
        assert_eq!(ms_to_frames(0.5, 48_000), 24);
        assert_eq!(ms_to_frames(0.5, 44_100), 22); // 22.05 rounds to 22
        assert_eq!(ms_to_frames(6.0, 48_000), 288);
        assert_eq!(ms_to_frames(60.0, 48_000), 2880);
        assert_eq!(ms_to_frames(0.0, 48_000), 0);
        assert_eq!(ms_to_frames(-3.0, 48_000), 0);
        // A positive sub-frame duration must not collapse to zero.
        assert_eq!(ms_to_frames(0.0001, 1_000), 1);
    }

    #[test]
    fn test_mean_rms_max_abs() {
        assert_eq!(mean(&[]), 0.0);
        assert!((mean(&[1.0, -1.0, 2.0, -2.0]) - 0.0).abs() < 1e-6);
        assert!((mean(&[0.5, 0.5]) - 0.5).abs() < 1e-6);

        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[1.0f32, -1.0]) - 1.0).abs() < 1e-6);
        // RMS of a full-scale square is 1.0; of values {0,0,0,4}: sqrt(16/4)=2
        assert!((rms(&[0.0, 0.0, 0.0, 4.0]) - 2.0).abs() < 1e-6);

        assert_eq!(max_abs(&[]), 0.0);
        assert_eq!(max_abs(&[-0.3, 0.2, -0.8]), 0.8);
    }

    #[test]
    fn test_remove_dc() {
        let mut samples = vec![0.08f32; 100];
        let removed = remove_dc(&mut samples);
        assert!((removed - 0.08).abs() < 1e-6);
        for s in &samples {
            assert!(s.abs() < 1e-6, "residual DC: {s}");
        }

        // No-op on an already-centred buffer.
        let mut centered = vec![-0.5f32, 0.5];
        let removed = remove_dc(&mut centered);
        assert!(removed.abs() < 1e-6);
        assert_eq!(centered, vec![-0.5, 0.5]);

        assert_eq!(remove_dc(&mut []), 0.0);
    }

    #[test]
    fn test_remove_dc_finite_input_stays_finite() {
        let mut samples = vec![0.025f32, 0.035, 0.02, 0.03];
        remove_dc(&mut samples);
        assert!(samples.iter().all(|s| s.is_finite()));
    }
}
