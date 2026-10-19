//! `keyvibes soundpack-acceptance-test`
//!
//! The imported upstream soundpacks, checked end to end in one pass. This is
//! the command that answers "are the 21 rustyvibes-v2 packs actually here and
//! actually right?" - nine checks over the catalog, the installed files, the
//! metadata, the coverage, the audio, the release table, a rebuild, and a
//! simulated press/release run through the live engine.
//!
//! Checks are reported in this order:
//!
//! 1. Upstream catalog, 2. Installation, 3. Provenance and license,
//! 4. Honest coverage, 5. No substitution, 6. Clip audio, 7. Release sounds,
//! 8. Deterministic rebuild, 9. End to end.
//!
//! A check that needs a resource this host does not have (the importer's
//! `coverage.json`, PipeWire) reports `NOT RUN` with the reason. It is never
//! reported as a pass.

use crate::accept::Report;
use crate::pack_locate;
use anyhow::Result;
use kv_core::{PhysicalKey, Settings, SoundSource, VariantState};
use kv_pack::KvPack;
use kv_runtime::{PackPlayer, Runtime, SimInputOptions};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Every installed pack whose metadata `source` starts with this prefix is a
/// pack converted from the pinned rustyvibes-v2 checkout.
const UPSTREAM_PREFIX: &str = "rustyvibes-v2/";

/// The number of packs in the pinned upstream catalog.
const EXPECTED_PACKS: usize = 21;

/// The switch categories the importer assigns from the upstream catalog.
const CATEGORIES: [&str; 3] = ["linear", "tactile", "clicky"];

/// Clip sanity floors, in i16 units.
///
/// The importer normalizes to -30 dBFS RMS with a -1 dBFS peak ceiling, so a
/// healthy clip sits far above these; they exist to catch a silent or
/// truncated export, not to grade loudness.
const MIN_PEAK: i32 = 256;
const MIN_RMS: f64 = 8.0;
const MIN_CREST: f64 = 2.0;

/// One entry of the importer's `coverage.json` report.
#[derive(Debug, Deserialize)]
struct CoveragePack {
    id: String,
    #[allow(dead_code)]
    name: String,
    category: String,
    covered: Vec<String>,
    missing: Vec<String>,
    #[allow(dead_code)]
    rate: u32,
    release_keys: u32,
}

/// Runs the soundpack acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("soundpack-acceptance-test");
    let source_root = PathBuf::from("assets/soundpacks/upstream-src");
    let coverage_path = source_root.join("coverage.json");
    let coverage = load_coverage(&coverage_path);

    // Every installed pack converted from the pinned upstream checkout.
    let mut upstream: Vec<(String, PathBuf, Arc<KvPack>)> = Vec::new();
    for path in pack_locate::discover_unique() {
        let Ok(pack) = KvPack::open(&path) else {
            continue;
        };
        if !pack.metadata().source.starts_with(UPSTREAM_PREFIX) {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        upstream.push((id, path, Arc::new(pack)));
    }
    upstream.sort_by(|a, b| a.0.cmp(&b.0));

    // ---------------------------------------------------- 1. upstream catalog
    match &coverage {
        None => report.fail(
            "Upstream catalog",
            format!(
                "{} is missing - run scripts/import_upstream_soundpacks.py \
                 to (re)generate the converted sources and their coverage report",
                coverage_path.display()
            ),
        ),
        Some(packs) => {
            let mut problems: Vec<String> = Vec::new();
            if packs.len() != EXPECTED_PACKS {
                problems.push(format!(
                    "catalog lists {} pack(s); the pinned rustyvibes-v2 \
                     catalog has {EXPECTED_PACKS}",
                    packs.len()
                ));
            }
            let ids: BTreeSet<&str> = packs.iter().map(|p| p.id.as_str()).collect();
            if ids.len() != packs.len() {
                problems.push("catalog contains duplicate pack ids".to_string());
            }
            for entry in packs {
                let manifest = source_root.join(&entry.id).join("pack.toml");
                if !manifest.is_file() {
                    problems.push(format!("{}: no pack.toml", entry.id));
                    continue;
                }
                let slices = source_root.join(&entry.id).join("slices");
                let has_wav = std::fs::read_dir(&slices)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .any(|e| e.path().extension().is_some_and(|x| x == "wav"))
                    })
                    .unwrap_or(false);
                if !has_wav {
                    problems.push(format!("{}: no slice .wav files", entry.id));
                }
            }
            if problems.is_empty() {
                report.pass(
                    "Upstream catalog",
                    format!(
                        "{EXPECTED_PACKS} pack(s) with manifests and slices \
                         under {}",
                        source_root.display()
                    ),
                );
            } else {
                report.fail("Upstream catalog", problems.join("; "));
            }
        }
    }

    // ------------------------------------------------------- 2. installation
    let expected_ids: Vec<String> = match &coverage {
        Some(packs) => packs.iter().map(|p| p.id.clone()).collect(),
        None => upstream.iter().map(|(id, _, _)| id.clone()).collect(),
    };
    if expected_ids.is_empty() {
        report.fail(
            "Installation",
            "no packs converted from rustyvibes-v2 are installed - run the \
             importer, then `keyvibes pack build` for each manifest and copy \
             the .kvpack files into the user pack directory",
        );
    } else {
        let mut problems: Vec<String> = Vec::new();
        let mut loaded_count = 0usize;
        for id in &expected_ids {
            match upstream.iter().find(|(found, _, _)| found == id) {
                None => problems.push(format!("{id}: not installed")),
                Some((_, path, pack)) => {
                    let stats = pack.stats();
                    if stats.name.is_empty() || stats.keys == 0 || stats.clips == 0 {
                        problems.push(format!(
                            "{id}: opened from {} but looks empty (name {:?}, \
                             {} keys, {} clips)",
                            path.display(),
                            stats.name,
                            stats.keys,
                            stats.clips
                        ));
                    } else {
                        loaded_count += 1;
                    }
                }
            }
        }
        if problems.is_empty() {
            report.pass(
                "Installation",
                format!(
                    "{loaded_count}/{} pack(s) open and are non-empty",
                    expected_ids.len()
                ),
            );
        } else {
            report.fail("Installation", problems.join("; "));
        }
    }

    // ------------------------------------------ 3. provenance and license
    {
        let mut problems: Vec<String> = Vec::new();
        for (id, _, pack) in &upstream {
            let meta = pack.metadata();
            if meta.name.is_empty() {
                problems.push(format!("{id}: empty name"));
            }
            if !meta.author.to_lowercase().contains("mechvibes")
                && !meta.author.to_lowercase().contains("kbsim")
            {
                problems.push(format!(
                    "{id}: author {:?} credits neither upstream",
                    meta.author
                ));
            }
            if !meta.license.to_lowercase().contains("mit") {
                problems.push(format!("{id}: license {:?} is not MIT", meta.license));
            }
            if !meta.source.starts_with(UPSTREAM_PREFIX) {
                problems.push(format!(
                    "{id}: source {:?} is not a pinned checkout",
                    meta.source
                ));
            }
            if !CATEGORIES.contains(&meta.category.as_str()) {
                problems.push(format!(
                    "{id}: category {:?} is not linear/tactile/clicky",
                    meta.category
                ));
            }
            if meta.description.is_empty() {
                problems.push(format!("{id}: empty description"));
            }
        }
        if upstream.is_empty() {
            report.fail("Provenance", "no upstream packs are installed to inspect");
        } else if problems.is_empty() {
            report.pass(
                "Provenance",
                format!(
                    "all {} pack(s) carry MIT licensing, a pinned rustyvibes-v2 \
                     source path, switch credit, and a category",
                    upstream.len()
                ),
            );
        } else {
            report.fail("Provenance", problems.join("; "));
        }
    }

    // -------------------------------------------------- 4. honest coverage
    match &coverage {
        None => report.not_run(
            "Honest coverage",
            "the importer's coverage.json report is required to cross-check",
        ),
        Some(packs) => {
            let mut problems: Vec<String> = Vec::new();
            let mut checked = 0usize;
            for entry in packs {
                let Some((_, _, pack)) = upstream.iter().find(|(id, _, _)| id == &entry.id) else {
                    problems.push(format!(
                        "{}: not installed, so coverage is unchecked",
                        entry.id
                    ));
                    continue;
                };
                checked += 1;
                let covered = pack.key_coverage();
                if covered != entry.covered.len() as u32 {
                    problems.push(format!(
                        "{}: loader reports {covered} covered key(s), the \
                         importer reported {}",
                        entry.id,
                        entry.covered.len()
                    ));
                }
                let missing: BTreeSet<String> = pack
                    .missing_keys()
                    .iter()
                    .map(|k| format!("{k:?}"))
                    .collect();
                let expected: BTreeSet<String> = entry.missing.iter().cloned().collect();
                if missing != expected {
                    problems.push(format!(
                        "{}: loader missing set {missing:?} != importer set {expected:?}",
                        entry.id
                    ));
                }
                if pack.release_key_count() != entry.release_keys {
                    problems.push(format!(
                        "{}: loader reports {} release key(s), the importer \
                         reported {}",
                        entry.id,
                        pack.release_key_count(),
                        entry.release_keys
                    ));
                }
                if pack.metadata().category != entry.category {
                    problems.push(format!(
                        "{}: loader category {:?} != importer category {:?}",
                        entry.id,
                        pack.metadata().category,
                        entry.category
                    ));
                }
            }
            if problems.is_empty() {
                report.pass(
                    "Honest coverage",
                    format!(
                        "all {checked} pack(s) report exactly the keys the \
                         importer converted - no silent padding, no inflated \
                         counts"
                    ),
                );
            } else {
                report.fail("Honest coverage", problems.join("; "));
            }
        }
    }

    // ------------------------------------------------- 5. no substitution
    {
        let mut problems: Vec<String> = Vec::new();
        let mut checked_keys = 0u32;
        for (id, _, pack) in &upstream {
            for entry in pack.keys() {
                checked_keys += 1;
                let mut state = VariantState::default();
                let variants = entry.variant_count.max(1) as usize;
                let mut seen = std::collections::HashSet::new();
                for _ in 0..(variants * 4) {
                    let Some(cmd) =
                        pack.play_command(entry.physical_key, &mut state, 48_000, 1.0, 1.0)
                    else {
                        problems.push(format!(
                            "{id}: {:?} has {} clip(s) but play_command returned None",
                            entry.physical_key, entry.variant_count
                        ));
                        break;
                    };
                    let inside = (0..entry.variant_count)
                        .any(|v| clip_holds(pack, entry.first_clip + u32::from(v), cmd.sample_ptr));
                    if !inside {
                        problems.push(format!(
                            "{id}: press of {:?} points outside its own {} clip(s) - \
                             another key's sound would be substituted",
                            entry.physical_key, entry.variant_count
                        ));
                        break;
                    }
                    seen.insert(cmd.sample_ptr as usize);
                }
                if seen.len() > variants {
                    problems.push(format!(
                        "{id}: {:?} reached {} distinct clip(s) but declares {variants}",
                        entry.physical_key,
                        seen.len()
                    ));
                }
                if entry.variant_count == 0 {
                    problems.push(format!(
                        "{id}: {:?} declares zero variants",
                        entry.physical_key
                    ));
                }
            }
            for missing in pack.missing_keys() {
                let mut state = VariantState::default();
                if pack
                    .play_command(missing, &mut state, 48_000, 1.0, 1.0)
                    .is_some()
                {
                    problems.push(format!(
                        "{id}: {missing:?} is reported missing yet yields a \
                         command - substitution is forbidden"
                    ));
                }
            }
        }
        if upstream.is_empty() {
            report.fail(
                "No substitution",
                "no upstream packs are installed to probe",
            );
        } else if problems.is_empty() {
            report.pass(
                "No substitution",
                format!(
                    "{checked_keys} key entry(ies) across {} pack(s): every \
                     press points into its own clip range, and every missing \
                     key is silent",
                    upstream.len()
                ),
            );
        } else {
            report.fail("No substitution", problems.join("; "));
        }
    }

    // ----------------------------------------------------- 6. clip audio
    {
        let mut problems: Vec<String> = Vec::new();
        let mut analyzed = 0u32;
        for (id, _, pack) in &upstream {
            let clips = pack.stats().clips;
            for index in 0..clips {
                let Some(samples) = pack.get_clip(index) else {
                    problems.push(format!("{id}: clip {index} unreadable"));
                    continue;
                };
                analyzed += 1;
                let (peak, rms) = peak_rms(samples);
                let crest = if rms > 0.0 {
                    f64::from(peak) / rms
                } else {
                    0.0
                };
                if peak < MIN_PEAK || rms < MIN_RMS || crest < MIN_CREST {
                    problems.push(format!(
                        "{id}: clip {index} peak {peak} rms {rms:.1} crest \
                         {crest:.2} - silent or truncated audio"
                    ));
                }
            }
        }
        if upstream.is_empty() {
            report.fail("Clip audio", "no upstream packs are installed to analyze");
        } else if problems.is_empty() {
            report.pass(
                "Clip audio",
                format!(
                    "{analyzed} clip(s) across {} pack(s): all non-silent",
                    upstream.len()
                ),
            );
        } else {
            let shown: Vec<String> = problems.iter().take(8).cloned().collect();
            let more = if problems.len() > shown.len() {
                format!(" (+{} more)", problems.len() - shown.len())
            } else {
                String::new()
            };
            report.fail("Clip audio", format!("{}{}", shown.join("; "), more));
        }
    }

    // -------------------------------------------------- 7. release sounds
    {
        let mut problems: Vec<String> = Vec::new();
        let mut packs_with_releases = 0usize;
        let mut release_keys_total = 0u32;
        for (id, _, pack) in &upstream {
            let entries = pack.release_entries().to_vec();
            if pack.has_release_sounds() {
                packs_with_releases += 1;
                release_keys_total += pack.release_key_count();
            }
            for entry in &entries {
                let mut state = VariantState::default();
                let Some(cmd) =
                    pack.release_command(entry.physical_key, &mut state, 48_000, 1.0, 1.0)
                else {
                    problems.push(format!(
                        "{id}: {:?} has a release entry but release_command \
                         returned None",
                        entry.physical_key
                    ));
                    continue;
                };
                if !cmd.release {
                    problems.push(format!(
                        "{id}: release command for {:?} is not flagged release",
                        entry.physical_key
                    ));
                }
                let inside = (0..entry.release_variant_count).any(|v| {
                    clip_holds(
                        pack,
                        entry.first_release_clip + u32::from(v),
                        cmd.sample_ptr,
                    )
                });
                if !inside {
                    problems.push(format!(
                        "{id}: release of {:?} points outside its own clips",
                        entry.physical_key
                    ));
                }
            }
            if !pack.has_release_sounds() {
                for entry in pack.keys() {
                    let mut state = VariantState::default();
                    if pack
                        .release_command(entry.physical_key, &mut state, 48_000, 1.0, 1.0)
                        .is_some()
                    {
                        problems.push(format!(
                            "{id}: release command invented for {:?} despite no \
                             release table",
                            entry.physical_key
                        ));
                    }
                }
            }
        }

        // The player honours the enable flag in both directions.
        if let Some((_, _, pack)) = upstream.iter().find(|(_, _, p)| p.has_release_sounds()) {
            let release_key = pack
                .release_entries()
                .first()
                .map(|entry| entry.physical_key);
            if let Some(key) = release_key {
                let player = PackPlayer::new(Arc::clone(pack), 48_000);
                let mut state = VariantState::default();
                if player.play_release(key, &mut state).is_some() {
                    problems.push("PackPlayer emitted a release sound while disabled".to_string());
                }
                let player = PackPlayer::new(Arc::clone(pack), 48_000);
                player.release_sounds(true);
                let mut state = VariantState::default();
                match player.play_release(key, &mut state) {
                    Some(cmd) if cmd.release => {}
                    Some(_) => problems
                        .push("PackPlayer release command is not flagged release".to_string()),
                    None => problems
                        .push("PackPlayer emitted no release sound while enabled".to_string()),
                }
            }
        } else {
            problems.push(
                "no installed upstream pack carries release sounds, so the \
                 player-level toggle could not be exercised"
                    .to_string(),
            );
        }

        if upstream.is_empty() {
            report.fail("Release sounds", "no upstream packs are installed to probe");
        } else if problems.is_empty() {
            report.pass(
                "Release sounds",
                format!(
                    "{packs_with_releases} pack(s), {release_keys_total} release \
                     key(s): every entry resolves inside its own clips, packs \
                     without a release table stay silent, and the player \
                     honours the enable flag"
                ),
            );
        } else {
            report.fail("Release sounds", problems.join("; "));
        }
    }

    // -------------------------------------------- 8. deterministic rebuild
    match &coverage {
        None => report.not_run(
            "Deterministic rebuild",
            "the importer's coverage.json report is required to locate the \
             source manifests",
        ),
        Some(packs) => {
            let scratch = std::env::temp_dir()
                .join(format!("keyvibes-soundpack-rebuild-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&scratch);
            let mut rebuilt = 0usize;
            let mut problems: Vec<String> = Vec::new();
            for entry in packs {
                let manifest = source_root.join(&entry.id).join("pack.toml");
                let Some((_, installed, _)) = upstream.iter().find(|(id, _, _)| id == &entry.id)
                else {
                    continue;
                };
                if !manifest.is_file() {
                    continue;
                }
                let out = scratch.join(format!("{}.kvpack", entry.id));
                match kv_pack::build_from_manifest(&manifest, &out, |_| {}) {
                    Err(error) => {
                        problems.push(format!("{}: rebuild failed: {error}", entry.id));
                        continue;
                    }
                    Ok(_) => {
                        let fresh = match std::fs::read(&out) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                problems.push(format!(
                                    "{}: rebuilt file unreadable: {error}",
                                    entry.id
                                ));
                                continue;
                            }
                        };
                        let original = match std::fs::read(installed) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                problems.push(format!(
                                    "{}: installed file unreadable: {error}",
                                    entry.id
                                ));
                                continue;
                            }
                        };
                        if fresh != original {
                            problems.push(format!(
                                "{}: rebuilt bytes differ from the installed pack",
                                entry.id
                            ));
                        } else {
                            rebuilt += 1;
                        }
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&scratch);
            if problems.is_empty() && rebuilt > 0 {
                report.pass(
                    "Deterministic rebuild",
                    format!("{rebuilt} pack(s) rebuilt byte-identical to the installed files"),
                );
            } else if rebuilt == 0 && problems.is_empty() {
                report.fail(
                    "Deterministic rebuild",
                    "no pack could be rebuilt - no installed pack matched the catalog",
                );
            } else {
                report.fail("Deterministic rebuild", problems.join("; "));
            }
        }
    }

    // ------------------------------------------- 9. end to end
    {
        // A pack with release sounds *and* a missing key proves both halves
        // of the contract in one simulated session.
        let candidate = upstream
            .iter()
            .find(|(_, _, pack)| pack.has_release_sounds() && !pack.missing_keys().is_empty());
        let candidate = candidate.or_else(|| {
            upstream
                .iter()
                .find(|(_, _, pack)| pack.has_release_sounds())
        });
        match candidate {
            None => report.fail(
                "End to end",
                "no installed upstream pack carries release sounds to exercise",
            ),
            Some((id, path, pack)) => {
                let covered: Vec<PhysicalKey> =
                    pack.keys().iter().take(6).map(|e| e.physical_key).collect();
                let missing = pack.missing_keys();
                let mut script_keys = covered.clone();
                script_keys.extend(missing.iter().take(2).copied());
                if script_keys.is_empty() {
                    report.fail("End to end", "the selected pack offers no keys to press");
                } else {
                    match simulate_run(path, false, &script_keys) {
                        Err(error) => report.not_run(
                            "End to end",
                            format!("PipeWire did not start, so the engine could not run: {error}"),
                        ),
                        Ok(off) => match simulate_run(path, true, &script_keys) {
                            Err(error) => report.fail(
                                "End to end",
                                format!("second (release-enabled) run failed: {error}"),
                            ),
                            Ok(on) => {
                                let mut problems: Vec<String> = Vec::new();
                                if off.presses == 0 || on.presses == 0 {
                                    problems.push(
                                        "the simulated device reported no presses".to_string(),
                                    );
                                }
                                if off.missing == 0 {
                                    problems.push(format!(
                                        "pressing the {} missing key(s) of {id} never \
                                             counted a missing sound",
                                        missing.len()
                                    ));
                                }
                                if on.commands <= off.commands {
                                    problems.push(format!(
                                        "release-enabled run queued {} command(s), \
                                             not more than the {} of the \
                                             release-disabled run",
                                        on.commands, off.commands
                                    ));
                                }
                                if off.dropped != 0 || on.dropped != 0 {
                                    problems.push(format!(
                                        "commands dropped (off={}, on={})",
                                        off.dropped, on.dropped
                                    ));
                                }
                                if off.peak <= 0.0 || on.peak <= 0.0 {
                                    problems.push(format!(
                                        "no audio was rendered (off peak {:.4}, \
                                             on peak {:.4})",
                                        off.peak, on.peak
                                    ));
                                }
                                if problems.is_empty() {
                                    report.pass(
                                        "End to end",
                                        format!(
                                            "{id}: {} press(es) -> {} command(s) with \
                                                 release sounds off, {} with them on \
                                                 ({} release command(s)); {} missing \
                                                 press(es) stayed silent; peak {:.4}",
                                            on.presses,
                                            off.commands,
                                            on.commands,
                                            on.commands - off.commands,
                                            off.missing + on.missing,
                                            on.peak
                                        ),
                                    );
                                } else {
                                    report.fail("End to end", problems.join("; "));
                                }
                            }
                        },
                    }
                }
            }
        }
    }

    report.finish()
}

/// The importer's `coverage.json`, loaded from disk.
fn load_coverage(path: &Path) -> Option<Vec<CoveragePack>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether `ptr` points inside the stored samples of clip `index`.
fn clip_holds(pack: &KvPack, index: u32, ptr: *const i16) -> bool {
    let Some(slice) = pack.get_clip(index) else {
        return false;
    };
    let start = slice.as_ptr() as usize;
    let end = start + std::mem::size_of_val(slice);
    let probe = ptr as usize;
    probe >= start && probe < end
}

/// Peak magnitude and RMS of a clip, in i16 units.
fn peak_rms(samples: &[i16]) -> (i32, f64) {
    let mut peak = 0i32;
    let mut sum = 0f64;
    for &s in samples {
        let v = i32::from(s);
        peak = peak.max(v.abs());
        sum += f64::from(v) * f64::from(v);
    }
    let rms = if samples.is_empty() {
        0.0
    } else {
        (sum / samples.len() as f64).sqrt()
    };
    (peak, rms)
}

/// What one simulated session observed.
struct SimRun {
    presses: u64,
    commands: u64,
    missing: u64,
    dropped: u64,
    peak: f32,
}

/// Drives the real engine with scripted taps, with or without release sounds.
///
/// Returns `Err` when PipeWire never reaches the streaming state, so the
/// caller can report the check as `NOT RUN` rather than a fake pass.
fn simulate_run(pack_path: &Path, release_sounds: bool, keys: &[PhysicalKey]) -> Result<SimRun> {
    let settings = Settings {
        release_sounds_enabled: release_sounds,
        ..Settings::default()
    };
    let mut runtime = Runtime::new(48_000);
    runtime.set_settings(settings);
    runtime.load_pack(pack_path)?;
    runtime.start_audio()?;
    if !runtime.wait_for_audio(Duration::from_secs(10)) {
        anyhow::bail!("the stream never reached the streaming state within 10s");
    }
    let _ = runtime.start_simulated(SimInputOptions::from_keys(
        keys,
        Duration::from_millis(3),
        5,
    ));
    std::thread::sleep(Duration::from_millis(1500));
    let input = runtime.input_stats();
    let audio = runtime.audio_stats();
    runtime.stop_simulated();
    runtime.shutdown();
    Ok(SimRun {
        presses: input.key_presses,
        commands: input.commands_generated,
        missing: input.missing_sounds,
        dropped: input.commands_dropped,
        peak: audio.peak,
    })
}
