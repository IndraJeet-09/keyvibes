//! End-to-end builder tests: manifest → WAV decode → layout → write → load.
//!
//! These exercise the public API only, exactly the way `keyvibes pack build`
//! and the runtime loader do.

use kv_pack::error::PackError;
use kv_pack::format::{GUARD_AFTER, GUARD_BEFORE};
use kv_pack::{build_from_manifest, ClipData, KvPack, PackBuilder};
use std::path::{Path, PathBuf};

/// Directory containing committed fixtures.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Unique scratch directory for one test (tests run in parallel).
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("kvpack-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

/// Writes a mono 16-bit WAV (48 kHz).
fn write_wav(path: &Path, samples: &[i16]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
}

fn write_manifest(root: &Path, text: &str) -> PathBuf {
    let path = root.join("pack.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn manifest_with_samples(samples: &[&str]) -> String {
    let mut text =
        String::from("[pack]\nname = \"Temp Pack\"\nauthor = \"tests\"\nsample_rate = 48000\n\n");
    text.push_str("[[keys]]\nphysical_key = \"A\"\nsamples = [");
    for (i, s) in samples.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        text.push('"');
        text.push_str(s);
        text.push('"');
    }
    text.push_str("]\n");
    text
}

// ---------------------------------------------------------------------------
// Golden fixture
// ---------------------------------------------------------------------------

#[test]
fn golden_fixture_is_reproducible() {
    let manifest = fixtures().join("golden-pack/pack.toml");
    let golden = fixtures().join("golden.kvpack");
    assert!(
        golden.exists(),
        "golden.kvpack fixture missing at {}",
        golden.display()
    );

    let out = temp_root("golden").join("rebuilt.kvpack");
    let report = build_from_manifest(&manifest, &out, |_| {}).expect("build fixture pack");

    let expected = std::fs::read(&golden).unwrap();
    let actual = std::fs::read(&out).unwrap();

    assert_eq!(
        expected, actual,
        "builder output must be byte-identical to the committed golden pack"
    );
    assert_eq!(report.key_count, 3);
    assert_eq!(report.clip_count, 4);
    assert_eq!(report.sample_rate, 48000);

    let _ = std::fs::remove_dir_all(out.parent().unwrap());
}

#[test]
fn golden_pack_opens_and_exposes_expected_content() {
    let pack = KvPack::open(fixtures().join("golden.kvpack")).expect("open golden pack");
    let stats = pack.stats();

    assert_eq!(stats.name, "Golden Test Pack");
    assert_eq!(stats.keys, 3);
    assert_eq!(stats.clips, 4);
    assert_eq!(stats.sample_rate, 48000);
    assert_eq!(stats.channels, 1);
    assert_eq!(stats.sample_frames, 160);

    // Metadata is parsed eagerly at open time.
    assert_eq!(pack.metadata().author, "KeyVibes Tests");
    assert!(!pack.metadata().license.is_empty());

    // Guards are present on every clip.
    for clip in pack.clips() {
        assert_eq!(clip.guard_before, GUARD_BEFORE);
        assert_eq!(clip.guard_after, GUARD_AFTER);
        assert_eq!(
            clip.stored_frames,
            clip.sample_frames + GUARD_BEFORE + GUARD_AFTER
        );
        assert!(clip.sample_frames > 0);
    }

    // Key table is sorted by physical key ID.
    let ids: Vec<u16> = pack
        .keys()
        .iter()
        .map(|k| k.physical_key.as_u16())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "key table must be sorted");
}

// ---------------------------------------------------------------------------
// Round-trip: source WAVs → pack → loader
// ---------------------------------------------------------------------------

#[test]
fn round_trip_preserves_samples_guards_and_rates() {
    let root = temp_root("roundtrip");
    let logical: Vec<i16> = (0..64).map(|i| (i * 300) - 9000).collect();
    write_wav(&root.join("sounds/a.wav"), &logical);
    write_manifest(&root, &manifest_with_samples(&["sounds/a.wav"]));

    let out = root.join("out.kvpack");
    build_from_manifest(root.join("pack.toml"), &out, |_| {}).expect("build");

    let pack = KvPack::open(&out).expect("open built pack");

    // Clip contains guards + logical frames, in order.
    let clip = pack.get_clip(0).expect("clip 0");
    assert_eq!(
        clip.len(),
        logical.len() + GUARD_BEFORE as usize + GUARD_AFTER as usize
    );
    let guarded = &clip[GUARD_BEFORE as usize..GUARD_BEFORE as usize + logical.len()];
    assert_eq!(
        guarded,
        logical.as_slice(),
        "logical samples must round-trip"
    );

    // Edge-extension guards copy the boundary frames.
    assert_eq!(clip[0], logical[0]);
    assert_eq!(clip[1], logical[0]);
    assert_eq!(clip[clip.len() - 1], *logical.last().unwrap());
    assert_eq!(clip[clip.len() - 2], *logical.last().unwrap());
    assert_eq!(clip[clip.len() - 3], *logical.last().unwrap());

    // play_command points at the first logical frame with the full length.
    let mut state = kv_pack::VariantState::default();
    let cmd = pack
        .play_command(kv_core::PhysicalKey::A, &mut state, 48000, 1.0, 1.0)
        .expect("play command");
    assert_eq!(cmd.sample_len, logical.len() as u32);
    assert_eq!(cmd.source_rate, 48000);
    assert_eq!(cmd.pitch_step, 1u64 << 32);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn round_trip_downmixes_stereo_to_mono() {
    let root = temp_root("stereo");

    // (L, R) pairs whose average is exactly known.
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    std::fs::create_dir_all(root.join("sounds")).unwrap();
    let mut writer = hound::WavWriter::create(root.join("sounds/s.wav"), spec).unwrap();
    for pair in [(1000i16, 2000i16), (-3000, 1000), (30000, 30000)] {
        writer.write_sample(pair.0).unwrap();
        writer.write_sample(pair.1).unwrap();
    }
    writer.finalize().unwrap();

    write_manifest(&root, &manifest_with_samples(&["sounds/s.wav"]));
    let out = root.join("out.kvpack");
    build_from_manifest(root.join("pack.toml"), &out, |_| {}).expect("build");

    let pack = KvPack::open(&out).unwrap();
    let clip = pack.get_clip(0).unwrap();
    let logical = &clip[GUARD_BEFORE as usize..clip.len() - GUARD_AFTER as usize];
    assert_eq!(logical, &[1500, -1000, 30000]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn variant_order_follows_the_manifest() {
    let root = temp_root("variants");
    write_wav(&root.join("sounds/one.wav"), &[111; 8]);
    write_wav(&root.join("sounds/two.wav"), &[222; 8]);

    let manifest = "[pack]\nname = \"V\"\nsample_rate = 48000\n\n[[keys]]\nphysical_key = \"A\"\nsamples = [\"sounds/one.wav\", \"sounds/two.wav\"]\n";
    write_manifest(&root, manifest);

    let out = root.join("out.kvpack");
    build_from_manifest(root.join("pack.toml"), &out, |_| {}).unwrap();

    let pack = KvPack::open(&out).unwrap();
    assert_eq!(pack.get_clip(0).unwrap()[GUARD_BEFORE as usize], 111);
    assert_eq!(pack.get_clip(1).unwrap()[GUARD_BEFORE as usize], 222);

    // Repeated presses alternate between the two variants.
    let mut state = kv_pack::VariantState::default();
    let first = pack.play_command(kv_core::PhysicalKey::A, &mut state, 48000, 1.0, 1.0);
    let second = pack.play_command(kv_core::PhysicalKey::A, &mut state, 48000, 1.0, 1.0);
    let first = first.expect("first command");
    let second = second.expect("second command");
    let a = unsafe { *first.sample_ptr };
    let b = unsafe { *second.sample_ptr };
    assert_ne!(a, b, "variants must rotate");

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn repeated_builds_are_byte_identical() {
    let manifest = fixtures().join("golden-pack/pack.toml");
    let dir = temp_root("determinism");

    let first = dir.join("first.kvpack");
    let second = dir.join("second.kvpack");

    build_from_manifest(&manifest, &first, |_| {}).unwrap();
    build_from_manifest(&manifest, &second, |_| {}).unwrap();

    let a = std::fs::read(&first).unwrap();
    let b = std::fs::read(&second).unwrap();
    assert_eq!(a, b, "same inputs must produce identical packs");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Failure handling
// ---------------------------------------------------------------------------

#[test]
fn missing_source_reports_key_and_file() {
    let root = temp_root("missing");
    write_manifest(&root, &manifest_with_samples(&["sounds/nope.wav"]));

    let out = root.join("out.kvpack");
    let err = build_from_manifest(root.join("pack.toml"), &out, |_| {}).unwrap_err();

    match err {
        PackError::BuildContext { key, file, .. } => {
            assert_eq!(key, "A");
            assert_eq!(file, "sounds/nope.wav");
        }
        other => panic!("expected BuildContext, got {other:?}"),
    }
    assert!(!out.exists(), "failed build must not create output");
    assert!(!root.join("out.kvpack.tmp").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sample_rate_mismatch_reports_offending_file() {
    let root = temp_root("rate-mismatch");

    // First source is 48 kHz, second is declared 44.1 kHz by rewriting spec.
    write_wav(&root.join("sounds/ok.wav"), &[0; 16]);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(root.join("sounds/other.wav"), spec).unwrap();
    writer.write_sample(0i16).unwrap();
    writer.finalize().unwrap();

    let manifest = "[pack]\nname = \"Mixed\"\n\n[[keys]]\nphysical_key = \"A\"\nsamples = [\"sounds/ok.wav\", \"sounds/other.wav\"]\n";
    write_manifest(&root, manifest);

    let err =
        build_from_manifest(root.join("pack.toml"), root.join("out.kvpack"), |_| {}).unwrap_err();

    match err {
        PackError::BuildContext { key, file, reason } => {
            assert_eq!(key, "A");
            assert_eq!(file, "sounds/other.wav");
            assert!(reason.contains("sample rate mismatch"), "reason: {reason}");
            assert!(reason.contains("44100"), "reason: {reason}");
        }
        other => panic!("expected BuildContext, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn path_traversal_is_rejected() {
    let root = temp_root("traversal");
    write_manifest(&root, &manifest_with_samples(&["../outside.wav"]));

    let err =
        build_from_manifest(root.join("pack.toml"), root.join("out.kvpack"), |_| {}).unwrap_err();

    match err {
        PackError::BuildContext { key, file, reason } => {
            assert_eq!(key, "A");
            assert_eq!(file, "../outside.wav");
            assert!(reason.contains("escapes pack root"), "reason: {reason}");
        }
        other => panic!("expected BuildContext wrapping PathTraversal, got {other:?}"),
    }
    assert!(!root.join("out.kvpack").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_physical_key_is_rejected() {
    let root = temp_root("unknown-key");
    let manifest =
        "[pack]\nname = \"X\"\n\n[[keys]]\nphysical_key = \"NotAKey\"\nsamples = [\"a.wav\"]\n";
    write_manifest(&root, manifest);

    let err =
        build_from_manifest(root.join("pack.toml"), root.join("out.kvpack"), |_| {}).unwrap_err();
    assert!(
        matches!(err, PackError::UnknownPhysicalKey(_)),
        "expected UnknownPhysicalKey, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn duplicate_physical_key_is_rejected() {
    let root = temp_root("dup-key");
    let manifest = "[pack]\nname = \"X\"\n\n[[keys]]\nphysical_key = \"A\"\nsamples = [\"a.wav\"]\n\n[[keys]]\nphysical_key = \"a\"\nsamples = [\"b.wav\"]\n";
    write_manifest(&root, manifest);

    let err =
        build_from_manifest(root.join("pack.toml"), root.join("out.kvpack"), |_| {}).unwrap_err();
    assert!(
        matches!(err, PackError::DuplicatePhysicalKey(_)),
        "expected DuplicatePhysicalKey, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Loader rejects corrupt packs
// ---------------------------------------------------------------------------

#[test]
fn corrupt_magic_is_rejected() {
    let path = temp_root("bad-magic").join("bad.kvpack");

    let mut bytes = std::fs::read(fixtures().join("golden.kvpack")).unwrap();
    bytes[0] = 0xFF;
    std::fs::write(&path, &bytes).unwrap();

    assert!(matches!(KvPack::open(&path), Err(PackError::InvalidMagic)));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn truncated_pack_is_rejected() {
    let path = temp_root("truncated").join("short.kvpack");

    let bytes = std::fs::read(fixtures().join("golden.kvpack")).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

    let err = match KvPack::open(&path) {
        Err(e) => e,
        Ok(_) => panic!("truncated pack must not open"),
    };
    assert!(
        !matches!(err, PackError::InvalidMagic),
        "truncated pack should fail on structure, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// Programmatic builder (no manifest)
// ---------------------------------------------------------------------------

#[test]
fn programmatic_builder_round_trip() {
    let root = temp_root("programmatic");

    let mut builder = PackBuilder::new();
    builder.name = "Programmatic".to_string();
    builder.author = "tests".to_string();
    builder
        .add_clip(
            kv_core::PhysicalKey::Space,
            ClipData::from_samples(vec![7i16; 40], 48000).unwrap(),
        )
        .unwrap();

    let out = root.join("pack.kvpack");
    builder.write(&out).unwrap();

    let pack = KvPack::open(&out).unwrap();
    assert_eq!(pack.metadata().name, "Programmatic");
    let clip = pack.get_clip(0).unwrap();
    let logical = &clip[GUARD_BEFORE as usize..clip.len() - GUARD_AFTER as usize];
    assert_eq!(logical, &[7i16; 40]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn programmatic_builder_rejects_mixed_sample_rates() {
    let mut builder = PackBuilder::new();
    builder.name = "Mixed".to_string();
    builder
        .add_clip(
            kv_core::PhysicalKey::A,
            ClipData::from_samples(vec![0i16; 4], 48000).unwrap(),
        )
        .unwrap();
    builder
        .add_clip(
            kv_core::PhysicalKey::B,
            ClipData::from_samples(vec![0i16; 4], 22050).unwrap(),
        )
        .unwrap();

    assert!(matches!(
        builder.plan(),
        Err(PackError::SampleRateMismatch { .. })
    ));
}
