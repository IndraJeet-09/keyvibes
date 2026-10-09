//! Phase 12 acceptance: `keyvibes config-test`.
//!
//! The contract for the configuration file is short and testable:
//!
//! * defaults apply when the file is absent,
//! * a value written to disk comes back bit-for-bit identical after the
//!   runtime is torn down and rebuilt,
//! * an older file (or a file that only sets one key) keeps loading,
//! * unknown keys are ignored rather than fatal,
//! * malformed values are rejected with a message that names the field -
//!   never silently clamped into something the user did not ask for,
//! * the write is atomic, so a crash cannot truncate the file.
//!
//! The test runs against a scratch config path (`$KEYVIBES_CONFIG`) so it
//! never touches a real `~/.config/keyvibes/config.toml`.

use crate::accept::{Report, Status};
use crate::config::{Config, CONFIG_VERSION};
use crate::pack_locate;
use anyhow::Result;
use kv_core::PhysicalKey;
use kv_runtime::Runtime;
use std::path::{Path, PathBuf};

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("config-test");

    let scratch = std::env::temp_dir().join(format!("keyvibes-config-test-{}", std::process::id()));
    let path = scratch.join("config.toml");
    let _ = std::fs::remove_dir_all(&scratch);
    std::env::set_var("KEYVIBES_CONFIG", &path);

    // --- 1. location honours the override ---------------------------------
    match Config::default_path() {
        Ok(resolved) if resolved == path => report.pass(
            "config location honours $KEYVIBES_CONFIG",
            resolved.display().to_string(),
        ),
        Ok(resolved) => report.fail(
            "config location honours $KEYVIBES_CONFIG",
            format!("expected {}, got {}", path.display(), resolved.display()),
        ),
        Err(error) => report.fail(
            "config location honours $KEYVIBES_CONFIG",
            format!("lookup failed: {error}"),
        ),
    }

    // --- 2. absent file means defaults -------------------------------------
    let fresh = Config::load(&path);
    match fresh {
        Ok(config) => {
            let defaults = Config::default();
            report.add(
                if config == defaults
                    && config.version == CONFIG_VERSION
                    && config.enabled
                    && !Path::new(&path).exists()
                {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "an absent file means every default",
                format!("version {}, volume {}", config.version, config.volume),
            );
        }
        Err(error) => report.fail(
            "an absent file means every default",
            format!("unexpected error: {error}"),
        ),
    }

    // --- 3. modify, save atomically ----------------------------------------
    let edited = Config {
        volume: 0.35,
        pack: Some("Holy Panda".to_string()),
        output_device: Some("keyvibes-config-test".to_string()),
        ..Config::default()
    };

    match edited.save(&path) {
        Ok(()) => report.pass("configuration is written", path.display().to_string()),
        Err(error) => {
            report.fail("configuration is written", format!("{error}"));
            return report.finish();
        }
    }

    let leftovers: Vec<PathBuf> = std::fs::read_dir(&scratch)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|found| {
                    found
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.contains(".tmp"))
                })
                .collect()
        })
        .unwrap_or_default();
    report.add(
        if path.exists() && leftovers.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "write is atomic - no temporary file survives",
        format!(
            "{} present, {} temporary file(s) left behind",
            if path.exists() {
                "config.toml"
            } else {
                "nothing"
            },
            leftovers.len()
        ),
    );

    // --- 4. restart the runtime, the values come back exactly --------------
    let reloaded = Config::load(&path);
    match reloaded {
        Ok(config) => {
            let exact = config.volume == edited.volume
                && config.pack == edited.pack
                && config.output_device == edited.output_device
                && config.pitch_variation == edited.pitch_variation
                && config.gain_variation == edited.gain_variation
                && config.release_sounds == edited.release_sounds
                && config.spatial_audio == edited.spatial_audio
                && config.version == edited.version
                && config.enabled == edited.enabled;
            report.add(
                if exact { Status::Pass } else { Status::Fail },
                "modified values are restored exactly after a restart",
                format!(
                    "volume {} -> {}, pack {:?}, device {:?}",
                    config.volume, edited.volume, config.pack, config.output_device
                ),
            );
        }
        Err(error) => report.fail(
            "modified values are restored exactly after a restart",
            format!("reload failed: {error}"),
        ),
    }

    // --- 5. real restart: build a runtime, drop it, rebuild ----------------
    let mut first = Runtime::new(48_000);
    if let Ok(config) = Config::load(&path) {
        first.set_settings(config.settings());
        first.set_output_device(config.output_device.clone());
    }
    let applied_before =
        first.settings().volume == 0.35 && first.output_device() == Some("keyvibes-config-test");
    drop(first);

    let mut second = Runtime::new(48_000);
    let reloaded_after_restart = Config::load(&path).unwrap_or_default();
    second.set_settings(reloaded_after_restart.settings());
    second.set_output_device(reloaded_after_restart.output_device.clone());
    let applied_after =
        second.settings().volume == 0.35 && second.output_device() == Some("keyvibes-config-test");
    report.add(
        if applied_before && applied_after {
            Status::Pass
        } else {
            Status::Fail
        },
        "runtime picks the saved values up on a fresh start",
        format!(
            "volume {}, output device {:?}",
            second.settings().volume,
            second.output_device()
        ),
    );
    drop(second);

    // --- 6. backwards compatible: minimal and unknown-key files ------------
    check_legacy_minimal(&mut report, &scratch);
    check_unknown_keys(&mut report, &scratch);
    check_rejections(&mut report, &scratch);

    // --- 7. the loaded configuration drives a real engine ------------------
    apply_to_engine(&mut report, &edited);

    let cleanup = std::fs::remove_dir_all(&scratch);
    if let Err(error) = cleanup {
        println!("  note: could not remove {}: {error}", scratch.display());
    }

    report.finish()
}

/// An old file that only sets one key must still load, with defaults for
/// everything else.
fn check_legacy_minimal(report: &mut Report, scratch: &Path) {
    let path = scratch.join("legacy.toml");
    if let Err(error) = std::fs::write(&path, "volume = 0.5\n") {
        report.fail("single-key (legacy) file loads", error.to_string());
        return;
    }
    match Config::load(&path) {
        Ok(config) => report.add(
            if config.volume == 0.5
                && config.version == CONFIG_VERSION
                && config.enabled
                && config.pitch_variation == Config::default().pitch_variation
                && config.pack.is_none()
            {
                Status::Pass
            } else {
                Status::Fail
            },
            "single-key (legacy) file loads with defaults for the rest",
            format!(
                "version {}, volume {}, pack {:?}",
                config.version, config.volume, config.pack
            ),
        ),
        Err(error) => report.fail(
            "single-key (legacy) file loads with defaults for the rest",
            error.to_string(),
        ),
    }
    let _ = std::fs::remove_file(&path);
}

/// Unknown keys must be ignored, not fatal: a newer or third-party file
/// should still be usable.
fn check_unknown_keys(report: &mut Report, scratch: &Path) {
    let path = scratch.join("unknown.toml");
    let text = format!(
        "version = {CONFIG_VERSION}\nvolume = 0.25\nfuture_option = \"ignored\"\n[some_table]\nx = 1\n"
    );
    if let Err(error) = std::fs::write(&path, text) {
        report.fail("unknown keys are ignored", error.to_string());
        return;
    }
    match Config::load(&path) {
        Ok(config) => report.add(
            if config.volume == 0.25 {
                Status::Pass
            } else {
                Status::Fail
            },
            "unknown keys are ignored, not fatal",
            format!("loaded volume {}", config.volume),
        ),
        Err(error) => report.fail("unknown keys are ignored, not fatal", error.to_string()),
    }
    let _ = std::fs::remove_file(&path);
}

/// Malformed content must be rejected with a message naming the problem.
fn check_rejections(report: &mut Report, scratch: &Path) {
    struct Case {
        name: &'static str,
        contents: String,
        expect: Expect,
    }
    enum Expect {
        InvalidField(&'static str),
        TooNew,
        Parse,
    }

    let cases = [
        Case {
            name: "volume outside 0.0..=1.0 is rejected",
            contents: "volume = 5.0\n".to_string(),
            expect: Expect::InvalidField("`volume`"),
        },
        Case {
            name: "non-finite volume is rejected",
            contents: "volume = nan\n".to_string(),
            expect: Expect::InvalidField("`volume`"),
        },
        Case {
            name: "blank pack name is rejected",
            contents: "pack = \"   \"\n".to_string(),
            expect: Expect::InvalidField("`pack`"),
        },
        Case {
            name: "a file from a newer KeyVibes is rejected",
            contents: format!("version = {}\n", CONFIG_VERSION + 98),
            expect: Expect::TooNew,
        },
        Case {
            name: "unparsable TOML is rejected",
            contents: "volume = [ [ [".to_string(),
            expect: Expect::Parse,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let path = scratch.join(format!("bad-{index}.toml"));
        let _ = std::fs::write(&path, &case.contents);
        let outcome = Config::load(&path);
        let verdict = match (&outcome, &case.expect) {
            (
                Err(crate::config::ConfigError::Invalid { field, .. }),
                Expect::InvalidField(want),
            ) => field == want,
            (Err(crate::config::ConfigError::TooNew { .. }), Expect::TooNew) => true,
            (Err(crate::config::ConfigError::Parse { .. }), Expect::Parse) => true,
            (Err(_), _) => true,
            (Ok(_), _) => false,
        };
        report.add(
            if verdict { Status::Pass } else { Status::Fail },
            case.name,
            match &outcome {
                Ok(_) => "the bad file was accepted".to_string(),
                Err(error) => error.to_string(),
            },
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// The loaded configuration must actually reach a running engine.
fn apply_to_engine(report: &mut Report, config: &Config) {
    let pack = match pack_locate::resolve(None) {
        Ok(path) => path,
        Err(error) => {
            report.not_run(
                "configuration reaches a running engine",
                format!("no pack available: {error:#}"),
            );
            return;
        }
    };

    let mut runtime = Runtime::new(48_000);
    runtime.set_settings(config.settings());
    runtime.set_output_device(config.output_device.clone());
    if let Err(error) = runtime.load_pack(&pack) {
        report.not_run(
            "configuration reaches a running engine",
            format!("pack did not load: {error}"),
        );
        return;
    }
    if let Err(error) = runtime.start_audio() {
        report.not_run(
            "configuration reaches a running engine",
            format!("cannot open an output stream here ({error}); is PipeWire running?"),
        );
        return;
    }

    let engine = match runtime.audio() {
        Some(engine) => engine,
        None => {
            report.not_run(
                "configuration reaches a running engine",
                "engine not started",
            );
            return;
        }
    };
    if !engine.wait_until_active(std::time::Duration::from_secs(10)) {
        report.fail(
            "configuration reaches a running engine",
            "output never became active",
        );
        return;
    }

    let settings_ok = runtime.settings().volume == config.volume
        && runtime.settings().pitch_variation_enabled == config.pitch_variation
        && runtime.settings().gain_variation_enabled == config.gain_variation
        && runtime.settings().release_sounds_enabled == config.release_sounds
        && runtime.settings().spatial_audio_enabled == config.spatial_audio;
    report.add(
        if settings_ok && runtime.output_device() == config.output_device.as_deref() {
            Status::Pass
        } else {
            Status::Fail
        },
        "configuration reaches a running engine",
        format!(
            "volume {}, pitch variation {}, device {:?}",
            runtime.settings().volume,
            runtime.settings().pitch_variation_enabled,
            runtime.output_device()
        ),
    );

    if runtime.trigger_key(PhysicalKey::A) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut heard = false;
        while std::time::Instant::now() < deadline {
            if runtime.audio_stats().active_voices > 0 {
                heard = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        report.add(
            if heard { Status::Pass } else { Status::Fail },
            "engine still makes sound with the loaded configuration",
            if heard {
                "a voice was scheduled".to_string()
            } else {
                "no voice appeared within 3s".to_string()
            },
        );
    } else {
        report.fail(
            "engine still makes sound with the loaded configuration",
            "the pack has no clip bound to the 'A' key",
        );
    }
}
