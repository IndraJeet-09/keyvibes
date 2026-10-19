//! Regression tests for the "that key made no sound" diagnostics.
//!
//! A silent key has two possible causes - the event never mapped, or the
//! selected pack has no clip for it - and only the second one is a packaging
//! bug. These drive the shipped binary and check that each place a user
//! could ask the question answers it with the coverage number, the missing
//! keys, and the policy (silent, never substituted):

//! `pack list -v`, `pack inspect <pack>`, and `doctor`.

use kv_core::PhysicalKey;
use kv_pack::{build_from_manifest, ClipData, PackBuilder};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Scratch tree with a complete pack, a two-key pack, and both pack
/// directories the child processes are pointed at.
struct Fixture {
    root: PathBuf,
    full: PathBuf,
    partial: PathBuf,
    /// Directory holding both packs - for `pack list -v` / `pack inspect`.
    both: PathBuf,
    /// Directory holding only the incomplete pack - for `doctor`, so the
    /// selector cannot accidentally choose the complete one.
    only_partial: PathBuf,
    config: PathBuf,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("keyvibes-diagnostics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch root");

        let both = root.join("both");
        let only_partial = root.join("only-partial");
        std::fs::create_dir_all(&both).unwrap();
        std::fs::create_dir_all(&only_partial).unwrap();

        // A shipped pack: all 104 keys, two variants each.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/soundpacks/upstream-src/alpaca/pack.toml");
        let full = both.join("Full.kvpack");
        let report = build_from_manifest(&manifest, &full, |_| {})
            .expect("the committed manifest must build");
        assert_eq!(report.key_count, PhysicalKey::COUNT as u32);

        // A pack that deliberately carries two keys and nothing else.
        let partial = both.join("Partial.kvpack");
        let mut builder = PackBuilder::new();
        builder.name = "Partial".to_string();
        for (index, key) in [PhysicalKey::A, PhysicalKey::Space].into_iter().enumerate() {
            let samples: Vec<i16> = (0..512)
                .map(|i| (((i + index * 11) % 97) as i16 - 48) * 400)
                .collect();
            builder
                .add_clip(key, ClipData::from_samples(samples, 48_000).unwrap())
                .unwrap();
        }
        builder.write(&partial).expect("write the partial pack");
        std::fs::copy(&partial, only_partial.join("Partial.kvpack")).unwrap();

        let config = root.join("config.toml");
        std::fs::write(&config, "version = 1\nenabled = true\nvolume = 0.7\n").unwrap();

        Fixture {
            root,
            full,
            partial,
            both,
            only_partial,
            config,
        }
    })
}

/// Runs `keyvibes` with the fixture's environment: a config we own, pack
/// directories we own, and a home that cannot see this machine's real packs.
fn invoke(args: &[&str], pack_dir: &Path) -> (i32, String) {
    let fixture = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_keyvibes"))
        .args(args)
        .env("KEYVIBES_PACK_DIR", pack_dir)
        .env("KEYVIBES_CONFIG", &fixture.config)
        .env_remove("KEYVIBES_PACK")
        .env("HOME", &fixture.root)
        .env("XDG_DATA_HOME", &fixture.root)
        .env("XDG_CONFIG_HOME", &fixture.root)
        .current_dir(&fixture.root)
        .output();

    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            (output.status.code().unwrap_or(-1), text)
        }
        Err(error) => (-1, format!("could not run keyvibes: {error}")),
    }
}

/// One line worth of output, for failure messages.
fn line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn pack_list_verbose_reports_coverage_per_pack() {
    let fixture = fixture();
    let (code, text) = invoke(&["pack", "list", "-v"], &fixture.both);
    assert_eq!(code, 0, "pack list -v failed: {}", line(&text));

    assert!(
        text.contains("coverage 104/104"),
        "the complete pack must show full coverage. Output: {}",
        line(&text)
    );
    assert!(
        text.contains("coverage 2/104"),
        "the two-key pack must show 2/104, not 104/104. Output: {}",
        line(&text)
    );
    // The fixture directory really is the one being searched.
    assert!(
        text.contains(&fixture.both.display().to_string()),
        "the searched directory must be the one we set. Output: {}",
        line(&text)
    );
}

#[test]
fn pack_inspect_reports_full_coverage_for_the_shipped_pack() {
    let fixture = fixture();
    let pack = fixture.full.display().to_string();
    let (code, text) = invoke(&["pack", "inspect", &pack], &fixture.both);
    assert_eq!(code, 0, "pack inspect failed: {}", line(&text));

    // Compared on whitespace-collapsed text: the layout padding between the
    // label and the value is not part of the contract.
    let flat = line(&text);
    assert!(
        flat.contains(&format!(
            "coverage: {}/{} physical keys",
            PhysicalKey::COUNT,
            PhysicalKey::COUNT
        )),
        "expected full coverage. Output: {flat}"
    );
    assert!(
        !flat.contains("no sound for"),
        "a complete pack must not list missing keys. Output: {flat}"
    );
    assert!(
        flat.contains("source: rustyvibes-v2/kbsim/alpaca"),
        "provenance must be visible on inspect. Output: {flat}"
    );
}

#[test]
fn pack_inspect_lists_the_missing_keys_and_states_the_policy() {
    let fixture = fixture();
    let pack = fixture.partial.display().to_string();
    let (code, text) = invoke(&["pack", "inspect", &pack], &fixture.both);
    assert_eq!(code, 0, "pack inspect failed: {}", line(&text));

    let flat = line(&text);
    assert!(
        flat.contains("coverage: 2/104 physical keys"),
        "expected 2/104. Output: {flat}"
    );
    assert!(
        flat.contains("no sound for"),
        "the missing keys must be named. Output: {flat}"
    );
    assert!(
        flat.contains("never substituted with another key's clip"),
        "the policy must be stated, not implied. Output: {flat}"
    );
    // A and Space are carried; the keys that are not must be named.
    assert!(
        !flat.contains("no sound for A,"),
        "A is in this pack and must not be listed missing. Output: {flat}"
    );
}

#[test]
fn doctor_warns_that_a_partial_pack_leaves_keys_silent() {
    let fixture = fixture();
    let (code, text) = invoke(&["doctor"], &fixture.only_partial);

    assert!(
        text.contains("coverage") && text.contains("2/104"),
        "doctor must show pack coverage. Output: {}",
        line(&text)
    );
    assert!(
        text.contains("[warning] the selected sound pack does not cover every key"),
        "doctor must warn about incomplete coverage. Output: {}",
        line(&text)
    );
    assert!(
        text.contains("counted and stay silent"),
        "doctor must state the no-substitution policy. Output: {}",
        line(&text)
    );
    assert!(
        (0..=1).contains(&code),
        "doctor exits 0 or 1, got {code}: {}",
        line(&text)
    );
}

#[test]
fn doctor_is_happy_with_the_complete_pack() {
    let fixture = fixture();
    // Only the complete pack is visible to this run.
    let dir = fixture.root.join("only-full");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&fixture.full, dir.join("Full.kvpack")).unwrap();

    let (code, text) = invoke(&["doctor"], &dir);
    assert!(
        text.contains("104/104") && !text.contains("does not cover every key"),
        "a complete pack must not be diagnosed as incomplete. Output: {}",
        line(&text)
    );
    assert!(
        (0..=1).contains(&code),
        "doctor exits 0 or 1, got {code}: {}",
        line(&text)
    );
}
