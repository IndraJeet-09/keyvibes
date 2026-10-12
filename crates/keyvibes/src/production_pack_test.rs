//! Cargo tests for the sound pack this repository ships.
//!
//! The two defects these exist for:
//!
//! * a key the selected pack does not carry must resolve to *nothing*, never
//!   to some other key's sound - so coverage and non-silence are asserted
//!   per key, over every physical key, not sampled,
//! * an asset's identity and provenance are part of the product - so the
//!   pack must say what it is and the PCM must be real, distinct audio.

use crate::pack_locate;
use kv_core::{PhysicalKey, VariantState};
use kv_pack::format::{GUARD_AFTER, GUARD_BEFORE};
use kv_pack::manifest::resolve_source;
use kv_pack::{build_from_manifest, KvPack, PackManifest};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The source manifest committed to this repository.
fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/soundpacks/default-src/pack.toml")
}

/// Builds the production pack once per test binary; every test reads that
/// one file, so the build cost is paid a single time.
fn built_pack() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let out =
            std::env::temp_dir().join(format!("keyvibes-production-{}.kvpack", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let report = build_from_manifest(manifest_path(), &out, |_| {})
            .expect("the committed manifest must build");
        assert_eq!(report.key_count, PhysicalKey::COUNT as u32);
        out
    })
}

fn open_pack() -> KvPack {
    KvPack::open(built_pack()).expect("the pack we just built must open")
}

/// Hash of a clip's logical samples - identity, not loudness.
fn clip_digest(pcm: &[i16]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    pcm.hash(&mut hasher);
    hasher.finish()
}

fn logical(pcm: &[i16]) -> &[i16] {
    let start = GUARD_BEFORE as usize;
    let end = pcm.len().saturating_sub(GUARD_AFTER as usize);
    &pcm[start..end.max(start)]
}

#[test]
fn every_physical_key_is_listed_with_sample_files_that_exist() {
    let source = manifest_path();
    let manifest = PackManifest::load(&source).expect("load the committed manifest");
    let root = source.parent().expect("manifest has a directory");
    let keys = manifest.resolved_keys().expect("resolve every key name");

    assert_eq!(
        keys.len(),
        PhysicalKey::COUNT,
        "the manifest lists {} key(s)",
        keys.len()
    );

    let mut seen: HashSet<PhysicalKey> = HashSet::new();
    for entry in &keys {
        assert!(
            seen.insert(entry.physical_key),
            "{} is listed twice",
            entry.physical_key
        );
        assert!(
            !entry.samples.is_empty(),
            "{} has no samples",
            entry.physical_key
        );
        for relative in &entry.samples {
            let path = resolve_source(root, relative)
                .unwrap_or_else(|e| panic!("{}: {relative}: {e}", entry.physical_key));
            assert!(
                path.is_file(),
                "{}: sample file missing on disk: {}",
                entry.physical_key,
                path.display()
            );
        }
    }

    assert_eq!(
        seen.len(),
        PhysicalKey::COUNT,
        "every one of the {} physical keys must be covered",
        PhysicalKey::COUNT
    );
}

#[test]
fn the_built_pack_serves_every_physical_key() {
    let pack = open_pack();
    let stats = pack.stats();

    assert_eq!(stats.keys, PhysicalKey::COUNT as u32);
    assert_eq!(pack.key_coverage(), PhysicalKey::COUNT as u32);
    assert!(pack.covers_all_keys(), "coverage: {}", pack.key_coverage());
    assert!(
        pack.missing_keys().is_empty(),
        "missing: {:?}",
        pack.missing_keys()
    );

    for id in 0..PhysicalKey::COUNT as u16 {
        let key = PhysicalKey::from_u16(id).expect("id in range");
        assert!(
            pack.lookup(key).is_some(),
            "{} is in the key table but does not resolve",
            key
        );
        let mut state = VariantState::default();
        let command = pack
            .play_command(key, &mut state, 48_000, 1.0, 1.0)
            .unwrap_or_else(|| panic!("{} produced no PlayCommand", key));
        assert!(command.sample_len > 0, "{} resolved to an empty clip", key);
    }
}

#[test]
fn every_key_resolves_to_its_own_audible_clip() {
    let pack = open_pack();
    let mut digests: HashSet<u64> = HashSet::new();
    let mut clips = 0u32;

    for id in 0..PhysicalKey::COUNT as u16 {
        let key = PhysicalKey::from_u16(id).expect("id in range");
        let sounds = pack.lookup(key).expect("every key resolves");
        assert!(sounds.variant_count > 0, "{} has no variants", key);

        for variant in 0..sounds.variant_count {
            let index = sounds.first_clip + u32::from(variant);
            let pcm = pack
                .get_clip(index)
                .unwrap_or_else(|| panic!("{} clip {index}", key));
            let samples = logical(pcm);

            assert!(
                samples.len() > 64,
                "{} variant {variant} is only {} frames",
                key,
                samples.len()
            );

            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            assert!(
                peak >= 1638,
                "{} variant {variant} peaks at {peak}/32767 - that is silence, \
                 not a key sound",
                key
            );

            let rms = {
                let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
                (sum / samples.len() as f64).sqrt()
            };
            assert!(rms > 0.0, "{} variant {variant} has zero energy", key);
            let crest = 20.0 * (f64::from(peak) / rms).log10();
            assert!(
                crest >= 6.0,
                "{} variant {variant} crest factor is {crest:.1} dB - a flat tone, \
                 not a struck switch",
                key
            );

            assert!(
                digests.insert(clip_digest(samples)),
                "{} variant {variant} repeats a clip another key already owns - \
                 all keys must have distinct audio",
                key
            );
            clips += 1;
        }
    }

    assert_eq!(
        clips,
        pack.stats().clips,
        "the key table and the clip table disagree"
    );
    assert_eq!(
        clips, 208,
        "the default pack is 104 keys x 2 variants, found {clips}"
    );
}

#[test]
fn the_pack_states_where_its_audio_came_from() {
    let pack = open_pack();
    let metadata = pack.metadata();
    let stats = pack.stats();

    assert_eq!(stats.name, "Default");
    assert_eq!(
        metadata.source, "keyvibes-synth-switch-v1",
        "the pack must name its generator, not inherit a stale label"
    );
    let description = metadata.description.to_lowercase();
    assert!(
        description.contains("synthesized"),
        "provenance must say the audio is synthesized, got: {}",
        metadata.description
    );
    assert!(
        !description.contains("field recording") || description.contains("not a field recording"),
        "the description must not imply field recordings, got: {}",
        metadata.description
    );
    assert_eq!(metadata.license, "MIT OR Apache-2.0");
    assert!(!metadata.author.trim().is_empty());
}

#[test]
fn the_installed_default_pack_matches_the_committed_one() {
    let production = open_pack();

    // Whatever is installed for this user must be the pack built from the
    // committed manifest, not a leftover from an older layout.
    let installed = match pack_locate::resolve(None) {
        Ok(path) => path,
        // Nothing installed (fresh checkout): the coverage above still holds.
        Err(_) => return,
    };
    if installed.file_name().and_then(|n| n.to_str()) != Some("Default.kvpack") {
        return;
    }
    let installed = match KvPack::open(&installed) {
        Ok(pack) => pack,
        // Selected by name/path from elsewhere; not ours to compare.
        Err(_) => return,
    };

    assert_eq!(
        installed.metadata().source,
        production.metadata().source,
        "the installed Default.kvpack is stale: rebuild it with \
         `keyvibes pack build` and reinstall it"
    );
    assert_eq!(
        installed.key_coverage(),
        production.key_coverage(),
        "the installed Default.kvpack covers {} of {} keys",
        installed.key_coverage(),
        PhysicalKey::COUNT
    );
}
