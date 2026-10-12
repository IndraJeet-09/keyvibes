//! Phase 13 acceptance: `keyvibes pack-test`.
//!
//! Installing more than one sound pack has to be enough to use them: the user
//! picks one by the name printed by `keyvibes pack list`, not by a path they
//! have to look up. The test proves discovery, name/path/stem selection, the
//! precedence rules between `--pack` and the config file, that the global
//! flag really is accepted on either side of the subcommand, and - when a
//! PipeWire session is reachable - that each installed pack actually plays.
//!
//! It adapts to whatever is installed: any set of packs works, and the
//! selection checks need at least two.
//!
//! Steps that need PipeWire are reported as `NOT RUN` when there is none -
//! never as a pass.

use crate::accept::{Report, Status};
use crate::config::Config;
use crate::pack_locate;
use crate::select_pack;
use anyhow::Result;
use clap::Parser;
use kv_runtime::{Runtime, SimInputOptions};
use std::path::Path;
use std::time::Duration;

use crate::cli::Cli;

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("pack-test");

    // --- 1. discovery ------------------------------------------------------
    let found = pack_locate::discover();
    let names = pack_locate::available_names();
    let unique = {
        let mut seen = std::collections::HashSet::new();
        names.iter().all(|name| seen.insert(name.to_lowercase()))
    };
    report.add(
        if !found.is_empty() && !names.is_empty() && unique {
            Status::Pass
        } else {
            Status::Fail
        },
        "`pack list` finds every installed pack",
        if !found.is_empty() && !names.is_empty() && unique {
            format!("{} pack(s): {}", names.len(), names.join(", "))
        } else if names.is_empty() {
            "no packs are installed - build some with `keyvibes pack build` \
             (see docs/soundpacks.md for the upstream loop and \
             assets/soundpacks/default-src for the synthesized demos)"
                .to_string()
        } else {
            format!(
                "installed names are not unique (a later pack would shadow \
                 an earlier one): {}",
                names.join(", ")
            )
        },
    );

    if names.is_empty() {
        return report.finish();
    }

    let first = &names[0];
    let first_path = match pack_locate::resolve_selector(first) {
        Ok(path) => path,
        Err(error) => {
            report.fail("selection by pack name", error.to_string());
            return report.finish();
        }
    };
    report.pass(
        "selection by pack name",
        format!("{}", first_path.display()),
    );

    let lower = pack_locate::resolve_selector(&first.to_lowercase());
    report.add(
        if lower.as_deref().ok() == Some(first_path.as_path()) {
            Status::Pass
        } else {
            Status::Fail
        },
        "pack names match case-insensitively",
        match lower {
            Ok(path) => format!("`{}` -> {}", first.to_lowercase(), path.display()),
            Err(error) => format!("unexpected error: {error}"),
        },
    );

    // --- 2. two packs prove the rest of the selection contract --------------
    let second = names.get(1).cloned();
    let second_path = match &second {
        Some(name) => match pack_locate::resolve_selector(name) {
            Ok(path) => Some(path),
            Err(error) => {
                report.fail("selection by pack name", error.to_string());
                return report.finish();
            }
        },
        None => None,
    };
    match (&second, &second_path) {
        (Some(name), Some(path)) => {
            report.add(
                if path != &first_path {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "each name selects a different pack",
                format!(
                    "{} vs {}",
                    first_path.file_name().unwrap_or_default().to_string_lossy(),
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
            );

            // The file stem is what a user types when the name has spaces.
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let by_stem = pack_locate::resolve_selector(&stem);
            report.add(
                if by_stem.as_deref().ok() == Some(path.as_path()) {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "a file stem selects its pack",
                match by_stem {
                    Ok(resolved) => format!("{stem} -> {}", resolved.display()),
                    Err(error) => format!("unexpected error: {error}"),
                },
            );

            let by_path = pack_locate::resolve_selector(&first_path.to_string_lossy());
            report.add(
                if by_path.as_deref().ok() == Some(first_path.as_path()) {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "a path works as a selector too",
                match by_path {
                    Ok(path) => format!("{} -> {}", first_path.display(), path.display()),
                    Err(error) => format!("unexpected error: {error}"),
                },
            );

            // --- 3. precedence: --pack beats config, config beats default ---
            let config = Config {
                pack: Some(first.clone()),
                ..Config::default()
            };

            let precedence = select_pack(Some(name), &config);
            report.add(
                if precedence.as_deref().ok() == Some(path.as_path()) {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "`--pack` overrides the configured pack",
                match precedence {
                    Ok(resolved) => format!("--pack {name} -> {}", resolved.display()),
                    Err(error) => format!("unexpected error: {error}"),
                },
            );

            let from_config = select_pack(None, &config);
            report.add(
                if from_config.as_deref().ok() == Some(first_path.as_path()) {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "the configured pack is used when `--pack` is absent",
                match from_config {
                    Ok(resolved) => format!("config pack {first} -> {}", resolved.display()),
                    Err(error) => format!("unexpected error: {error}"),
                },
            );
        }
        _ => {
            report.not_run(
                "each name selects a different pack",
                "only one pack is installed, so name/stem/path distinction \
                 and `--pack`-vs-config precedence have nothing to prove \
                 against",
            );
        }
    }

    let default = select_pack(None, &Config::default());
    report.add(
        if default.is_ok() {
            Status::Pass
        } else {
            Status::Fail
        },
        "a fresh install falls back to a default pack",
        match default {
            Ok(path) => format!("default -> {}", path.display()),
            Err(error) => error.to_string(),
        },
    );

    let unknown = pack_locate::resolve_selector("Definitely Not Installed");
    let message = unknown.as_ref().err().map(ToString::to_string);
    report.add(
        if unknown.is_err()
            && message
                .as_deref()
                .is_some_and(|m| m.contains("pack not found"))
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "an unknown pack is rejected with the installed list",
        message.unwrap_or_else(|| "an unknown pack resolved".to_string()),
    );

    let empty = pack_locate::resolve_selector("   ");
    report.add(
        if empty.is_err() {
            Status::Pass
        } else {
            Status::Fail
        },
        "an empty selector is rejected",
        if empty.is_err() {
            "`--pack \"\"` does not silently fall back".to_string()
        } else {
            "an empty selector resolved".to_string()
        },
    );

    // --- 4. the global flag parses on either side of the subcommand ---------
    // Pure parsing: the name need not be installed for clap to accept it.
    for before in [
        vec!["keyvibes", "--pack", "Linear", "run", "--simulate"],
        vec!["keyvibes", "run", "--pack", "Linear", "--simulate"],
        vec!["keyvibes", "stress", "--pack", "Linear", "--duration", "1"],
    ] {
        let label = before.join(" ");
        match Cli::try_parse_from(&before) {
            Ok(cli) if cli.sound_pack.as_deref() == Some("Linear") => {
                report.pass("`--pack` is accepted wherever the user types it", label);
            }
            Ok(cli) => report.fail(
                "`--pack` is accepted wherever the user types it",
                format!("{label}: parsed as {:?}", cli.sound_pack),
            ),
            Err(error) => report.fail(
                "`--pack` is accepted wherever the user types it",
                format!("{label}: {error}"),
            ),
        }
    }

    // --- 5. every installed pack really plays -------------------------------
    // A throwaway stream tells us whether PipeWire is reachable at all; if it
    // is not, the live half is `NOT RUN` rather than a pass.
    let mut probe = Runtime::new(48_000);
    let probe_result = probe.start_audio();
    probe.shutdown();

    if let Err(error) = probe_result {
        report.not_run(
            "every installed pack renders sound",
            format!("cannot open an output stream here ({error}); is PipeWire running?"),
        );
        return report.finish();
    }

    let mut details = Vec::new();
    let mut failed = Vec::new();
    for name in &names {
        match pack_locate::resolve_selector(name) {
            Ok(path) => match renders(&path) {
                Ok(detail) => details.push(format!("{name}: {detail}")),
                Err(error) => failed.push(format!("{name}: {error}")),
            },
            Err(error) => failed.push(format!("{name}: {error}")),
        }
    }
    if failed.is_empty() {
        report.pass("every installed pack renders sound", details.join("; "));
    } else {
        report.fail("every installed pack renders sound", failed.join("; "));
    }

    report.finish()
}

/// Loads a pack, starts the stream, drives it with scripted keys, and returns
/// what came out. Used to prove the pack is playable end to end.
fn renders(path: &Path) -> Result<String, String> {
    let mut runtime = Runtime::new(48_000);
    runtime.load_pack(path).map_err(|error| error.to_string())?;
    runtime
        .start_audio()
        .map_err(|error| format!("no output stream: {error}"))?;

    let active = runtime
        .audio()
        .map(|engine| engine.wait_until_active(Duration::from_secs(10)))
        .unwrap_or(false);
    if !active {
        runtime.shutdown();
        return Err("stream never became active".to_string());
    }

    let keys = [
        kv_core::PhysicalKey::A,
        kv_core::PhysicalKey::S,
        kv_core::PhysicalKey::D,
        kv_core::PhysicalKey::F,
        kv_core::PhysicalKey::Space,
    ];
    runtime
        .start_simulated(SimInputOptions::stress(&keys, 40.0, 0x9A35))
        .map_err(|error| error.to_string())?;

    std::thread::sleep(Duration::from_millis(700));
    let stats = runtime.audio_stats();
    runtime.shutdown();

    if stats.frames_rendered == 0 {
        return Err("the stream rendered no frames".to_string());
    }
    if stats.drained_commands == 0 {
        return Err("no key press reached the mixer".to_string());
    }
    if stats.deadline_misses > 0 {
        return Err(format!(
            "{} deadline miss(es) while rendering",
            stats.deadline_misses
        ));
    }
    Ok(format!(
        "{} frame(s), {} command(s), peak {:.3}",
        stats.frames_rendered, stats.drained_commands, stats.peak
    ))
}

/// The commands that produce the synthesized demo packs from the committed
/// sources.
///
/// The upstream packs are built the same way from
/// `assets/soundpacks/upstream-src/<id>/pack.toml`; see docs/soundpacks.md.
#[cfg(test)]
fn build_commands() -> Vec<String> {
    // The output path is quoted whole, so a pack whose name contains a space
    // is copy-pasteable as printed.
    [
        ("pack.toml", "assets/soundpacks/Default.kvpack"),
        ("holy-panda.toml", "\"assets/soundpacks/Holy Panda.kvpack\""),
        ("linear.toml", "assets/soundpacks/Linear.kvpack"),
    ]
    .iter()
    .map(|(manifest, output)| {
        format!(
            "cargo run --quiet --bin keyvibes -- pack build \
             assets/soundpacks/default-src/{manifest} -o {output}"
        )
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::build_commands;

    #[test]
    fn build_commands_cover_every_shipped_pack_and_quote_the_space() {
        let commands = build_commands();
        assert_eq!(commands.len(), 3);
        let joined = commands.join("\n");
        for output in ["Default.kvpack", "Holy Panda.kvpack", "Linear.kvpack"] {
            assert!(
                joined.contains(output),
                "no build command produces {output}:\n{joined}"
            );
        }
        assert!(
            joined.contains("-o \"assets/soundpacks/Holy Panda.kvpack\""),
            "the path with a space must be quoted whole:\n{joined}"
        );
        // The line continuation must not swallow the separating space.
        assert_eq!(
            commands[0],
            "cargo run --quiet --bin keyvibes -- pack build \
             assets/soundpacks/default-src/pack.toml -o assets/soundpacks/Default.kvpack"
        );
    }
}
