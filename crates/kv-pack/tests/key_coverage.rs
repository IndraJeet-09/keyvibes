//! Key coverage and the "never borrow another key's sound" policy.
//!
//! The product's contract for a key the selected pack does not carry is:
//! look it up, find nothing, return `None`, count the press, stay silent.
//! These tests pin each half of that contract down at the pack layer, where
//! the decision is made.

use kv_core::{PhysicalKey, VariantState};
use kv_pack::{build_from_manifest, ClipData, KvPack, PackBuilder};
use std::path::PathBuf;

/// Unique scratch directory for one test (tests run in parallel).
fn temp_root(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "kvpack-coverage-{}-{name}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp root");
    root
}

/// A pack that carries `A` and `Space` and nothing else.
fn pack_with(keys: &[PhysicalKey]) -> (PathBuf, KvPack) {
    let root = temp_root("partial");
    let mut builder = PackBuilder::new();
    builder.name = "Partial".to_string();
    for (index, key) in keys.iter().enumerate() {
        let samples: Vec<i16> = (0..256)
            .map(|i| (((i + index * 7) % 64) as i16 - 32) * 512)
            .collect();
        builder
            .add_clip(*key, ClipData::from_samples(samples, 48000).unwrap())
            .unwrap();
    }
    let path = root.join("partial.kvpack");
    builder.write(&path).unwrap();
    let pack = KvPack::open(&path).expect("open the pack we just wrote");
    (path, pack)
}

#[test]
fn a_key_the_pack_does_not_carry_resolves_to_nothing() {
    let (_path, pack) = pack_with(&[PhysicalKey::A, PhysicalKey::Space]);

    assert!(pack.lookup(PhysicalKey::A).is_some());
    assert!(pack.lookup(PhysicalKey::Space).is_some());
    for key in [
        PhysicalKey::Q,
        PhysicalKey::Z,
        PhysicalKey::Digit1,
        PhysicalKey::Enter,
        PhysicalKey::Backspace,
        PhysicalKey::ArrowUp,
    ] {
        assert!(
            pack.lookup(key).is_none(),
            "{key:?} is not in this pack and must not resolve"
        );
        let mut state = VariantState::default();
        assert!(
            pack.play_command(key, &mut state, 48_000, 1.0, 1.0)
                .is_none(),
            "{key:?} must produce no PlayCommand"
        );
    }
}

#[test]
fn a_missing_key_never_borrows_a_clip() {
    let (_path, pack) = pack_with(&[PhysicalKey::A, PhysicalKey::Space]);
    let mut state = VariantState::default();
    let a = pack
        .play_command(PhysicalKey::A, &mut state, 48_000, 1.0, 1.0)
        .expect("A is in the pack");
    let a_ptr = a.sample_ptr as usize;

    for id in 0..PhysicalKey::COUNT as u16 {
        let key = PhysicalKey::from_u16(id).unwrap();
        if key == PhysicalKey::A || key == PhysicalKey::Space {
            continue;
        }
        let mut other = VariantState::default();
        match pack.play_command(key, &mut other, 48_000, 1.0, 1.0) {
            None => {}
            Some(command) => panic!(
                "{key:?} is not in the pack but produced a command; A's clip starts at {a_ptr:#x}, \
                 this one at {:#x}",
                command.sample_ptr as usize
            ),
        }
    }
    // And A itself still resolves to A's own clip.
    let mut again = VariantState::default();
    let a_again = pack
        .play_command(PhysicalKey::A, &mut again, 48_000, 1.0, 1.0)
        .unwrap();
    assert_eq!(a_again.sample_ptr as usize, a_ptr);
}

#[test]
fn coverage_reports_exactly_which_keys_are_missing() {
    let (_path, pack) = pack_with(&[PhysicalKey::A, PhysicalKey::Space]);

    let missing = pack.missing_keys();
    assert_eq!(missing.len(), PhysicalKey::COUNT - 2);
    assert!(!missing.contains(&PhysicalKey::A));
    assert!(!missing.contains(&PhysicalKey::Space));
    assert!(missing.contains(&PhysicalKey::Q));
    assert!(missing.contains(&PhysicalKey::Enter));

    assert_eq!(pack.key_coverage(), 2);
    assert!(!pack.covers_all_keys());
}

#[test]
fn the_committed_fixture_reports_its_own_gaps_instead_of_claiming_coverage() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden.kvpack");
    let pack = KvPack::open(&fixture).expect("open the golden fixture");

    assert_eq!(pack.stats().name, "Golden Test Pack");
    assert!(
        !pack.covers_all_keys(),
        "a 3-key fixture must not report full coverage"
    );
    assert_eq!(pack.key_coverage(), pack.keys().len() as u32);
    assert!(pack.missing_keys().contains(&PhysicalKey::Q));
    // ...and it must never serve those missing keys from A.
    let mut state = VariantState::default();
    assert!(pack
        .play_command(PhysicalKey::Q, &mut state, 48_000, 1.0, 1.0)
        .is_none());
}

#[test]
fn a_pack_built_from_a_manifest_covers_every_key_it_lists() {
    let root = temp_root("manifest");
    std::fs::create_dir_all(root.join("sounds")).unwrap();

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    for name in ["a", "enter", "digit1"] {
        let mut writer =
            hound::WavWriter::create(root.join(format!("sounds/{name}.wav")), spec).unwrap();
        for i in 0..600i32 {
            writer.write_sample((i % 50) as i16 - 25).unwrap();
        }
        writer.finalize().unwrap();
    }

    let manifest = root.join("pack.toml");
    std::fs::write(
        &manifest,
        r#"[pack]
name = "Manifest Cover"
sample_rate = 48000

[processing]
enabled = false

[[keys]]
physical_key = "A"
samples = ["sounds/a.wav"]

[[keys]]
physical_key = "Enter"
samples = ["sounds/enter.wav"]

[[keys]]
physical_key = "Digit1"
samples = ["sounds/digit1.wav"]
"#,
    )
    .unwrap();

    let out = root.join("out.kvpack");
    build_from_manifest(&manifest, &out, |_| {}).expect("build");
    let pack = KvPack::open(&out).unwrap();

    assert_eq!(pack.key_coverage(), 3);
    assert_eq!(
        pack.missing_keys().len(),
        PhysicalKey::COUNT - 3,
        "every other physical key is reported missing"
    );
    assert!(pack.lookup(PhysicalKey::Enter).is_some());
    assert!(pack.lookup(PhysicalKey::Backspace).is_none());

    let _ = std::fs::remove_dir_all(&root);
}
