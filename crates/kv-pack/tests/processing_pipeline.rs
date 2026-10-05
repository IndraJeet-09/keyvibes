//! End-to-end tests for the Phase 5 processing pipeline:
//! manifest → f32 decode → DSP → dithered i16 → guards → pack → loader.
//!
//! The legacy raw path (`enabled = false`) is covered by
//! `builder_roundtrip.rs`; this file exercises the enabled path only.

use kv_pack::error::PackError;
use kv_pack::format::{GUARD_AFTER, GUARD_BEFORE};
use kv_pack::processing::DEFAULT_LOUDNESS_TARGET_DB;
use kv_pack::{build_from_manifest, BuildEvent, KvPack};
use std::path::{Path, PathBuf};

/// Directory containing committed fixtures.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Unique scratch directory for one test (tests run in parallel).
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("kvpack-p5-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

/// Writes a mono 16-bit WAV (48 kHz) from normalized f32 samples.
fn write_wav_f32(path: &Path, samples: &[f32], sample_rate: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
}

/// 1 kHz sine of `amp` amplitude.
fn sine(amp: f32, frames: usize, sample_rate: u32) -> Vec<f32> {
    (0..frames)
        .map(|i| amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
        .collect()
}

fn write_manifest(root: &Path, body: &str) -> PathBuf {
    let path = root.join("pack.toml");
    std::fs::write(&path, body).unwrap();
    path
}

/// Manifest with one key (`A`), one sample, and a `[processing]` body.
fn manifest_one_sample(processing: &str, sample: &str) -> String {
    format!(
        "[pack]\nname = \"P5\"\nauthor = \"tests\"\nsample_rate = 48000\n\n\
         [processing]\n{processing}\n\n\
         [[keys]]\nphysical_key = \"A\"\nsamples = [\"{sample}\"]\n"
    )
}

/// Builds and collects the per-clip processing reports.
fn build_with_reports(
    manifest: &Path,
    out: &Path,
) -> (kv_pack::BuildReport, Vec<kv_pack::ProcessingReport>) {
    let mut reports = Vec::new();
    let report = build_from_manifest(manifest, out, |e| {
        if let BuildEvent::Processed { report, .. } = e {
            reports.push(report);
        }
    })
    .expect("build");
    (report, reports)
}

// ---------------------------------------------------------------------------
// Basic enabled-path build
// ---------------------------------------------------------------------------

#[test]
fn processing_build_produces_valid_pack() {
    let root = temp_root("valid");
    write_wav_f32(&root.join("sounds/a.wav"), &sine(0.5, 4800, 48_000), 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let (report, reports) = build_with_reports(&root.join("pack.toml"), &out);

    assert_eq!(report.key_count, 1);
    assert_eq!(report.clip_count, 1);
    assert_eq!(report.sample_rate, 48000);
    assert_eq!(reports.len(), 1, "one Processed event per clip");

    let pack = KvPack::open(&out).expect("open processed pack");
    let clip = pack.get_clip(0).expect("clip 0");
    assert_eq!(clip[0], clip[GUARD_BEFORE as usize], "guard = first frame");
    let logical = clip.len() - GUARD_BEFORE as usize - GUARD_AFTER as usize;
    assert!(logical >= 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn processed_report_matches_loudness_target_and_ceiling() {
    let root = temp_root("loudness");
    write_wav_f32(&root.join("sounds/a.wav"), &sine(0.2, 4800, 48_000), 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let (_, reports) = build_with_reports(&root.join("pack.toml"), &out);
    let r = &reports[0];

    // Full-clip RMS lands on the configured target...
    assert!(
        (r.final_rms_dbfs - DEFAULT_LOUDNESS_TARGET_DB).abs() < 0.5,
        "rms {} dB",
        r.final_rms_dbfs
    );
    // ...while the peak never exceeds the ceiling.
    let ceiling = kv_pack::processing::ProcessingConfig::default().ceiling_amplitude();
    assert!(
        r.final_peak <= ceiling + kv_pack::processing::CEILING_TOLERANCE,
        "peak {} exceeds ceiling {ceiling}",
        r.final_peak
    );
    // A pure sine keeps its crest: peak ≈ rms + 3 dB.
    assert!(
        r.final_peak_dbfs - r.final_rms_dbfs > 2.5,
        "crest {} dB",
        r.final_peak_dbfs - r.final_rms_dbfs
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn manifest_loudness_override_is_honored() {
    let root = temp_root("override");
    write_wav_f32(&root.join("sounds/a.wav"), &sine(0.3, 4800, 48_000), 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true\nloudness_target_db = -20.0", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let (_, reports) = build_with_reports(&root.join("pack.toml"), &out);
    assert!(
        (reports[0].final_rms_dbfs - (-20.0)).abs() < 0.5,
        "rms {} dB",
        reports[0].final_rms_dbfs
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Trim behavior
// ---------------------------------------------------------------------------

#[test]
fn processing_trims_silent_padding() {
    let root = temp_root("trim");
    let mut samples = vec![0.0f32; 4800];
    samples.extend(sine(0.5, 2400, 48_000));
    samples.extend(vec![0.0f32; 4800]);
    write_wav_f32(&root.join("sounds/a.wav"), &samples, 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let (_, reports) = build_with_reports(&root.join("pack.toml"), &out);
    let r = &reports[0];

    assert_eq!(r.original_frames, 12_000);
    assert!(r.leading_frames_removed > 0, "leading silence trimmed");
    assert!(r.trailing_frames_removed > 0, "trailing silence trimmed");
    assert!(
        r.processed_frames < r.original_frames,
        "processed {} < original {}",
        r.processed_frames,
        r.original_frames
    );
    // The whole trimmed+faded buffer is kept consistent with the accounting.
    assert_eq!(
        r.leading_frames_removed + r.processed_frames + r.trailing_frames_removed,
        r.original_frames
    );
    // Onset detected near the start of the sine (window granularity = 48).
    assert!(
        (r.onset_frame as i64 - 4800).abs() <= 48,
        "onset {} vs expected ~4800",
        r.onset_frame
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Determinism & legacy bypass
// ---------------------------------------------------------------------------

#[test]
fn processing_builds_are_byte_identical() {
    let root = temp_root("determinism");
    write_wav_f32(&root.join("sounds/a.wav"), &sine(0.4, 4800, 48_000), 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true", "sounds/a.wav"),
    );

    let first = root.join("first.kvpack");
    let second = root.join("second.kvpack");
    build_from_manifest(root.join("pack.toml"), &first, |_| {}).unwrap();
    build_from_manifest(root.join("pack.toml"), &second, |_| {}).unwrap();

    let a = std::fs::read(&first).unwrap();
    let b = std::fs::read(&second).unwrap();
    assert_eq!(a, b, "processed builds (dither included) must be identical");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn processing_disabled_keeps_raw_frames_and_emits_no_reports() {
    let root = temp_root("disabled");
    // Padding that processing would trim: the raw path must keep it all.
    let mut samples = vec![0.0f32; 4800];
    samples.extend(sine(0.5, 2400, 48_000));
    samples.extend(vec![0.0f32; 4800]);
    write_wav_f32(&root.join("sounds/a.wav"), &samples, 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = false", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let (_, reports) = build_with_reports(&root.join("pack.toml"), &out);

    assert!(reports.is_empty(), "no Processed events when disabled");
    let pack = KvPack::open(&out).unwrap();
    let clip = pack.get_clip(0).unwrap();
    let logical = clip.len() - GUARD_BEFORE as usize - GUARD_AFTER as usize;
    assert_eq!(logical, 12_000, "raw path keeps every frame");

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Failure handling
// ---------------------------------------------------------------------------

#[test]
fn silence_source_fails_with_actionable_error() {
    let root = temp_root("silence");
    write_wav_f32(&root.join("sounds/a.wav"), &[0.0; 4800], 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true", "sounds/a.wav"),
    );

    let out = root.join("out.kvpack");
    let err = build_from_manifest(root.join("pack.toml"), &out, |_| {}).unwrap_err();

    match err {
        PackError::BuildContext { key, file, reason } => {
            assert_eq!(key, "A");
            assert_eq!(file, "sounds/a.wav");
            assert!(
                reason.contains("Unusable signal"),
                "reason should name the failure: {reason}"
            );
        }
        other => panic!("expected BuildContext, got {other:?}"),
    }
    assert!(!out.exists(), "failed build must not create output");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invalid_processing_config_fails_at_manifest_load() {
    let root = temp_root("bad-config");
    write_wav_f32(&root.join("sounds/a.wav"), &sine(0.5, 4800, 48_000), 48_000);
    write_manifest(
        &root,
        &manifest_one_sample("enabled = true\nfadeout_ms = -5.0", "sounds/a.wav"),
    );

    let err =
        build_from_manifest(root.join("pack.toml"), root.join("out.kvpack"), |_| {}).unwrap_err();
    assert!(
        matches!(
            err,
            PackError::ManifestError(_) | PackError::ValidationFailed(_)
        ),
        "expected config validation error, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Processed golden fixture (byte-reproducible build through the pipeline)
// ---------------------------------------------------------------------------

#[test]
fn processed_golden_fixture_is_reproducible() {
    let manifest = fixtures().join("golden-pack/pack-processed.toml");
    let golden = fixtures().join("processed.kvpack");
    assert!(
        golden.exists(),
        "processed.kvpack fixture missing at {}",
        golden.display()
    );

    let out = temp_root("golden-processed").join("rebuilt.kvpack");
    let (report, reports) = build_with_reports(&manifest, &out);

    assert_eq!(report.key_count, 3);
    assert_eq!(report.clip_count, 4);
    assert_eq!(reports.len(), 4, "every clip went through the pipeline");

    let expected = std::fs::read(&golden).unwrap();
    let actual = std::fs::read(&out).unwrap();
    assert_eq!(
        expected, actual,
        "processed build must be byte-identical to the committed fixture"
    );

    let _ = std::fs::remove_dir_all(out.parent().unwrap());
}
