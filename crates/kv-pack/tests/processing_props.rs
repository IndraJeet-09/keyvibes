//! Property tests for the Phase 5 processing pipeline.
//!
//! These assert the invariants that must hold for *any* input: no panics,
//! no NaN/Inf, output within the peak ceiling, deterministic results, and
//! consistent frame accounting. Concrete behaviors are covered by
//! `processing_pipeline.rs` and the unit tests in `src/processing.rs`.

use kv_pack::processing::{apply_fadeout, encode_i16, AudioProcessor, ProcessingConfig};
use kv_pack::wav::MonoAudio;
use proptest::prelude::*;

/// A buffer of finite samples with a sane sample rate.
fn audio_strategy() -> impl Strategy<Value = (Vec<f32>, u32)> {
    (
        proptest::collection::vec(-1.0f32..1.0, 1..2048),
        prop_oneof![Just(44_100u32), Just(48_000u32), Just(22_050u32)],
    )
}

fn process(samples: Vec<f32>, rate: u32) -> kv_pack::error::PackResult<kv_pack::ProcessedAudio> {
    AudioProcessor::with_defaults().process(MonoAudio {
        samples,
        sample_rate: rate,
        channels: 1,
    })
}

proptest! {
    /// The pipeline either rejects a clip cleanly or produces a non-empty,
    /// finite buffer whose peak never exceeds the ceiling.
    #[test]
    fn output_stays_valid_and_within_ceiling((samples, rate) in audio_strategy()) {
        let ceiling = ProcessingConfig::default().ceiling_amplitude();
        if let Ok(processed) = process(samples, rate) {
            prop_assert!(!processed.samples.is_empty());
            for (i, &s) in processed.samples.iter().enumerate() {
                prop_assert!(s.is_finite(), "sample {} = {}", i, s);
                prop_assert!(
                    s.abs() <= ceiling + kv_pack::processing::CEILING_TOLERANCE,
                    "sample {} = {} exceeds ceiling {}",
                    i,
                    s,
                    ceiling
                );
            }
            prop_assert!(processed.report.processed_frames >= 1);
            prop_assert!(processed.report.final_peak.is_finite());
        }
    }

    /// Same input → same output (the pipeline is a pure function).
    #[test]
    fn processing_is_deterministic((samples, rate) in audio_strategy()) {
        let a = process(samples.clone(), rate);
        let b = process(samples, rate);
        match (a, b) {
            (Ok(a), Ok(b)) => prop_assert_eq!(a.samples, b.samples),
            (Err(a), Err(b)) => prop_assert_eq!(format!("{a:?}"), format!("{b:?}")),
            (a, b) => prop_assert!(false, "mixed results: {:?} vs {:?}", a, b),
        }
    }

    /// Frame accounting always balances: leading + processed + trailing
    /// equals the original length, and neither trim count goes negative
    /// (they are `usize`, so the real invariant is the sum).
    #[test]
    fn frame_accounting_is_consistent((samples, rate) in audio_strategy()) {
        let original = samples.len();
        if let Ok(processed) = process(samples, rate) {
            let r = &processed.report;
            prop_assert_eq!(
                r.leading_frames_removed + r.processed_frames + r.trailing_frames_removed,
                original
            );
            prop_assert!(r.leading_frames_removed <= original);
            prop_assert!(r.trailing_frames_removed <= original);
            prop_assert!(r.onset_frame <= original);
            prop_assert!(r.preroll_frames <= original);
        }
    }

    /// Processing never changes the sample rate (no resampling).
    #[test]
    fn sample_rate_is_preserved((samples, rate) in audio_strategy()) {
        if let Ok(processed) = process(samples, rate) {
            prop_assert_eq!(processed.sample_rate, rate);
            prop_assert_eq!(processed.report.sample_rate, rate);
        }
    }

    /// The fade-out never increases a sample's magnitude and always ends
    /// the buffer at exactly zero.
    #[test]
    fn fade_is_a_true_fade(
        mut buf in proptest::collection::vec(-1.0f32..1.0, 1..512),
        fade in 0usize..600,
    ) {
        let before = buf.clone();
        apply_fadeout(&mut buf, fade);
        prop_assert_eq!(buf.len(), before.len());
        let start = buf.len().saturating_sub(fade.min(buf.len()));
        for i in start..buf.len() {
            prop_assert!(
                buf[i].abs() <= before[i].abs() + 1e-6,
                "faded sample {} grew: {} -> {}",
                i,
                before[i],
                buf[i]
            );
        }
        if fade > 0 && !buf.is_empty() {
            prop_assert_eq!(*buf.last().unwrap(), 0.0, "fade must end silent");
        }
        for (i, &s) in buf.iter().enumerate() {
            prop_assert!(s.is_finite(), "sample {} = {}", i, s);
        }
    }

    /// Encoding preserves length and is deterministic for a fixed seed.
    #[test]
    fn encode_preserves_length_and_is_deterministic(
        samples in proptest::collection::vec(-1.2f32..1.2, 1..1024),
        seed in 0u64..u64::MAX,
    ) {
        use kv_pack::dither::DitherSeed;
        let seed = Some(DitherSeed::from_u64(seed));
        let a = encode_i16(&samples, 48_000, seed).expect("encode");
        let b = encode_i16(&samples, 48_000, seed).expect("encode");
        prop_assert_eq!(a.samples.len(), samples.len());
        prop_assert_eq!(a.frames as usize, samples.len());
        prop_assert_eq!(a.sample_rate, 48_000);
        prop_assert_eq!(a.samples, b.samples, "same seed must dither identically");
    }

    /// Encoding without a seed performs no dither: values are exactly the
    /// clamp→scale→round→clamp quantization of the input.
    #[test]
    fn encode_without_seed_is_plain_quantization(
        samples in proptest::collection::vec(-1.0f32..1.0, 1..1024),
    ) {
        let pcm = encode_i16(&samples, 44_100, None).expect("encode");
        prop_assert_eq!(pcm.samples.len(), samples.len());
        for (i, (&s, &q)) in samples.iter().zip(pcm.samples.iter()).enumerate() {
            let expected = (s.clamp(-1.0, 1.0) * 32768.0).round();
            let expected = expected.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            prop_assert_eq!(q, expected, "sample {}", i);
        }
    }
}
