//! Read-only environment checks exposed to the desktop UI as
//! `keyvibes ctl`'s `diagnostics` request.
//!
//! `keyvibes doctor` is the deep, from-scratch version: it starts a second
//! audio stream to read the negotiated quantum and re-enumerates packs. That
//! is the right tool from a terminal, and the wrong one for a button a user
//! can press every few seconds while music is playing.
//!
//! This module therefore only ever *observes*:
//!
//! * it opens `/dev/input` and the PipeWire socket read-only,
//! * it re-runs keyboard discovery, which is what a "recheck" should redo,
//! * it reads the pack list and the config file that is already loaded,
//! * it never spawns a process, never writes a file, never touches the
//!   engine's audio stream or input pipeline.
//!
//! Every item carries an exact, copyable fix. KeyVibes never runs it: the
//! user decides, and the next recheck tells them whether it worked.

use kv_control::{DiagnosticItem, DiagnosticStatus, DiagnosticsReport, State};
use kv_core::PhysicalKey;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Runs every check and assembles the report.
///
/// `state` is the engine's own snapshot, so the report can distinguish
/// "the system has no readable keyboard" from "the engine has not attached
/// to the ones that exist".
pub fn run(state: &State, config_path: &Path, simulate: bool) -> DiagnosticsReport {
    let mut items = Vec::new();
    input_checks(state, simulate, &mut items);
    audio_checks(state, &mut items);
    pack_checks(state, &mut items);
    config_checks(state, config_path, &mut items);
    engine_checks(state, simulate, &mut items);

    let errors = items
        .iter()
        .filter(|item| item.status == DiagnosticStatus::Error)
        .count() as u32;
    let warnings = items
        .iter()
        .filter(|item| item.status == DiagnosticStatus::Warning)
        .count() as u32;
    DiagnosticsReport {
        healthy: errors == 0 && warnings == 0,
        errors,
        warnings,
        items,
    }
}

// --------------------------------------------------------------------- input

fn input_checks(state: &State, simulate: bool, items: &mut Vec<DiagnosticItem>) {
    let dir = Path::new("/dev/input");
    if !dir.exists() {
        items.push(DiagnosticItem {
            id: "input.device_nodes".to_string(),
            label: "Keyboard device nodes".to_string(),
            status: DiagnosticStatus::Error,
            detail: "/dev/input does not exist, so there is nothing to read".to_string(),
            fix: Some(
                "KeyVibes reads Linux evdev nodes directly. On a container, start it with \
                 `--device=/dev/input`; on a bare-metal Linux host this directory is created \
                 by the kernel."
                    .to_string(),
            ),
            command: None,
            recheckable: true,
        });
        return;
    }

    let nodes = event_nodes();
    if nodes.is_empty() {
        items.push(DiagnosticItem {
            id: "input.device_nodes".to_string(),
            label: "Keyboard device nodes".to_string(),
            status: DiagnosticStatus::Warning,
            detail: "/dev/input exists but contains no event* node".to_string(),
            fix: Some(
                "Plug a keyboard in, or pass the device through to this virtual machine. \
                 KeyVibes attaches automatically when one appears."
                    .to_string(),
            ),
            command: None,
            recheckable: true,
        });
    } else {
        items.push(DiagnosticItem {
            id: "input.device_nodes".to_string(),
            label: "Keyboard device nodes".to_string(),
            status: DiagnosticStatus::Ok,
            detail: format!("{} event node(s) present", nodes.len()),
            fix: None,
            command: None,
            recheckable: true,
        });
    }

    match probe_read_access(&nodes) {
        Ok(()) => items.push(DiagnosticItem {
            id: "input.permission".to_string(),
            label: "Keyboard input access".to_string(),
            status: DiagnosticStatus::Ok,
            detail: "this user can read the evdev device nodes".to_string(),
            fix: None,
            command: None,
            recheckable: true,
        }),
        Err(reason) => {
            // Two very different causes, two very different fixes. Saying
            // "join the input group" when the real problem is a container
            // without /dev/input would send the user down the wrong path.
            let (detail, fix, command) = if reason == "missing" {
                (
                    "the event nodes disappeared between listing and opening \
                     (a device was unplugged, or the sandbox dropped them)"
                        .to_string(),
                    "Reconnect the keyboard, then press Recheck.".to_string(),
                    None,
                )
            } else {
                (
                    "the evdev nodes are readable only by root and members of the \
                     `input` group, and this user is neither"
                        .to_string(),
                    "Joining `input` grants read access to *every* keyboard on the \
                     machine, including passwords typed elsewhere. The narrower option is \
                     the `uaccess` rule, which only grants access to keyboards on your \
                     own seat. Either way you must log out and back in."
                        .to_string(),
                    Some("sudo usermod -aG input $USER".to_string()),
                )
            };
            items.push(DiagnosticItem {
                id: "input.permission".to_string(),
                label: "Keyboard input access".to_string(),
                status: DiagnosticStatus::Error,
                detail,
                fix: Some(fix),
                command,
                recheckable: true,
            });
        }
    }

    match kv_input_linux::discovery::discover_keyboards() {
        Ok(found) => {
            let attached = state.input.keyboards;
            let detail = if simulate {
                format!(
                    "{} keyboard(s) visible; input is simulated right now, so none is attached",
                    found.len()
                )
            } else if found.is_empty() {
                "no device passed the keyboard test - plug one in and recheck".to_string()
            } else if attached >= found.len() {
                format!("{attached} keyboard(s) attached and being read")
            } else {
                format!(
                    "{} keyboard(s) visible, {attached} attached to the engine \
                     (the rest may be mice or power buttons)",
                    found.len()
                )
            };
            items.push(DiagnosticItem {
                id: "input.keyboards".to_string(),
                label: "Detected keyboards".to_string(),
                status: if found.is_empty() {
                    DiagnosticStatus::Warning
                } else {
                    DiagnosticStatus::Ok
                },
                detail,
                fix: if found.is_empty() {
                    Some("Plug in a keyboard; hot-plug picks it up without a restart.".to_string())
                } else {
                    None
                },
                command: None,
                recheckable: true,
            });
        }
        Err(error) => items.push(DiagnosticItem {
            id: "input.keyboards".to_string(),
            label: "Detected keyboards".to_string(),
            status: DiagnosticStatus::Error,
            detail: format!("enumeration failed: {error}"),
            fix: Some(
                "Check /dev/input permissions (see the check above) and that the evdev \
                 driver is loaded."
                    .to_string(),
            ),
            command: None,
            recheckable: true,
        }),
    }

    items.push(DiagnosticItem {
        id: "input.hotplug".to_string(),
        label: "Hot-plug monitoring".to_string(),
        status: if state.input.hotplug || simulate {
            DiagnosticStatus::Ok
        } else {
            DiagnosticStatus::Warning
        },
        detail: if simulate {
            "simulated input - no hot-plug monitor".to_string()
        } else if state.input.hotplug {
            "running; keyboards plugged in later attach within a second".to_string()
        } else {
            "not running; a keyboard plugged in now needs a restart".to_string()
        },
        fix: if state.input.hotplug || simulate {
            None
        } else {
            Some("Restart the KeyVibes engine to start the monitor.".to_string())
        },
        command: None,
        recheckable: false,
    });
}

/// `Ok` when every listed node can be opened for reading.
///
/// `Err("denied")` means at least one node exists and refused us - the
/// case where a permission fix is the answer. `Err("missing")` means the
/// nodes vanished underneath the check.
fn probe_read_access(nodes: &[PathBuf]) -> Result<(), &'static str> {
    if nodes.is_empty() {
        return Ok(());
    }
    let mut denied = false;
    for node in nodes {
        match std::fs::File::open(node) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::PermissionDenied => denied = true,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
    if denied {
        Err("denied")
    } else {
        Err("missing")
    }
}

/// Every `/dev/input/event*` node, in order.
fn event_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    let mut nodes: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .collect();
    nodes.sort();
    nodes
}

// --------------------------------------------------------------------- audio

/// One command, quoted identically wherever the fix is offered.
const PIPEWIRE_FIX: &str = "systemctl --user enable --now pipewire pipewire-pulse wireplumber";

fn audio_checks(state: &State, items: &mut Vec<DiagnosticItem>) {
    match pipewire_socket() {
        Ok(path) => match UnixStream::connect(&path) {
            Ok(_) => items.push(DiagnosticItem {
                id: "audio.pipewire".to_string(),
                label: "PipeWire audio server".to_string(),
                status: DiagnosticStatus::Ok,
                detail: format!("connected at {}", path.display()),
                fix: None,
                command: None,
                recheckable: true,
            }),
            Err(error) => items.push(DiagnosticItem {
                id: "audio.pipewire".to_string(),
                label: "PipeWire audio server".to_string(),
                status: DiagnosticStatus::Error,
                detail: format!(
                    "the socket at {} refused the connection: {error}",
                    path.display()
                ),
                fix: Some(
                    "The PipeWire service is not accepting clients. Start it for your \
                     session, then press Recheck."
                        .to_string(),
                ),
                command: Some(PIPEWIRE_FIX.to_string()),
                recheckable: true,
            }),
        },
        Err(detail) => items.push(DiagnosticItem {
            id: "audio.pipewire".to_string(),
            label: "PipeWire audio server".to_string(),
            status: DiagnosticStatus::Error,
            detail,
            fix: Some(
                "KeyVibes plays through PipeWire, which most Linux desktops start with the \
                 session. Install it if the distribution does not, then start the user services."
                    .to_string(),
            ),
            command: Some(PIPEWIRE_FIX.to_string()),
            recheckable: true,
        }),
    }

    let phase = state.audio.phase.as_str();
    let (status, detail) = match phase {
        "running" => (
            DiagnosticStatus::Ok,
            format!(
                "stream running at {} Hz, {} underrun(s), device {}",
                state.audio.output_rate,
                state.audio.xruns,
                state
                    .audio
                    .output_device
                    .as_deref()
                    .unwrap_or("session default")
            ),
        ),
        "reconnecting" | "starting" => (
            DiagnosticStatus::Warning,
            format!("stream is {phase} - PipeWire may have restarted"),
        ),
        "failed" => (
            DiagnosticStatus::Error,
            "the stream failed and its reconnect budget is exhausted".to_string(),
        ),
        "stopped" => (
            DiagnosticStatus::Warning,
            "the output stream is stopped".to_string(),
        ),
        other => (DiagnosticStatus::Warning, format!("stream phase: {other}")),
    };
    let fix = match status {
        DiagnosticStatus::Ok => None,
        DiagnosticStatus::Warning => Some(
            "Wait a moment - the stream reconnects on its own. If it stays this way, \
             restart the engine."
                .to_string(),
        ),
        DiagnosticStatus::Error => Some(
            "Check that PipeWire is running and that your session has an output sink, \
             then restart the engine."
                .to_string(),
        ),
    };
    items.push(DiagnosticItem {
        id: "audio.stream".to_string(),
        label: "Audio output stream".to_string(),
        status,
        detail,
        fix,
        command: None,
        recheckable: true,
    });
}

/// Locates the per-user PipeWire core socket.
fn pipewire_socket() -> Result<PathBuf, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "XDG_RUNTIME_DIR is not set, so the PipeWire socket cannot be found".to_string()
        })?;
    let explicit = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let dir = explicit.unwrap_or(runtime);
    let socket = dir.join("pipewire-0");
    if socket.exists() {
        Ok(socket)
    } else {
        Err(format!(
            "no PipeWire core socket at {} - the audio server has not started for this session",
            socket.display()
        ))
    }
}

// --------------------------------------------------------------------- packs

fn pack_checks(state: &State, items: &mut Vec<DiagnosticItem>) {
    if state.packs.is_empty() {
        items.push(DiagnosticItem {
            id: "packs.installed".to_string(),
            label: "Sound packs".to_string(),
            status: DiagnosticStatus::Error,
            detail: "no .kvpack file was found in any search directory".to_string(),
            fix: Some(
                "Reinstall KeyVibes: the packages ship all 21 packs under \
                 /usr/share/keyvibes/packs. User packs live in \
                 ~/.local/share/keyvibes/packs."
                    .to_string(),
            ),
            command: None,
            recheckable: true,
        });
        return;
    }

    let synthesized = state.packs.iter().filter(|pack| pack.synthesized).count();
    items.push(DiagnosticItem {
        id: "packs.installed".to_string(),
        label: "Sound packs".to_string(),
        status: DiagnosticStatus::Ok,
        detail: format!(
            "{} pack(s) installed ({} of them synthesized by KeyVibes)",
            state.packs.len(),
            synthesized
        ),
        fix: None,
        command: None,
        recheckable: true,
    });

    match state.pack.as_ref() {
        None => items.push(DiagnosticItem {
            id: "packs.active".to_string(),
            label: "Active sound pack".to_string(),
            status: DiagnosticStatus::Warning,
            detail: "no pack is loaded".to_string(),
            fix: Some("Choose a pack in the Sounds tab.".to_string()),
            command: None,
            recheckable: false,
        }),
        Some(pack) => {
            let total = PhysicalKey::COUNT as u32;
            let missing = total.saturating_sub(pack.coverage);
            let (status, detail, fix) = if missing == 0 {
                (
                    DiagnosticStatus::Ok,
                    format!(
                        "{} covers all {total} physical keys{}",
                        pack.name,
                        if pack.has_release {
                            " and has key-release sounds"
                        } else {
                            ""
                        }
                    ),
                    None,
                )
            } else {
                (
                    // Honest, not fatal: a short pack still plays every key
                    // it does cover, and KeyVibes never invents a substitute.
                    DiagnosticStatus::Warning,
                    format!(
                        "{} covers {}/{} keys; the rest stay silent rather than being \
                         substituted",
                        pack.name, pack.coverage, total
                    ),
                    Some(
                        "This is a property of the pack, not a fault. Pick a pack with \
                         full coverage if you need every key."
                            .to_string(),
                    ),
                )
            };
            items.push(DiagnosticItem {
                id: "packs.active".to_string(),
                label: "Active sound pack".to_string(),
                status,
                detail,
                fix,
                command: None,
                recheckable: false,
            });

            items.push(DiagnosticItem {
                id: "packs.release".to_string(),
                label: "Key-release sounds".to_string(),
                status: DiagnosticStatus::Ok,
                detail: if pack.has_release {
                    format!(
                        "available ({} release keys); currently {}",
                        pack.release_keys,
                        if state.settings.release_sounds {
                            "on"
                        } else {
                            "off"
                        }
                    )
                } else {
                    "not available in this pack - the toggle stays off".to_string()
                },
                fix: None,
                command: None,
                recheckable: false,
            });
        }
    }
}

// -------------------------------------------------------------------- config

fn config_checks(state: &State, config_path: &Path, items: &mut Vec<DiagnosticItem>) {
    let writable = config_path
        .parent()
        .is_some_and(|dir| dir.is_dir() && directory_writable(dir));

    if !config_path.exists() {
        items.push(DiagnosticItem {
            id: "config.file".to_string(),
            label: "Settings file".to_string(),
            status: DiagnosticStatus::Ok,
            detail: format!(
                "{} does not exist yet - defaults are in use and will be written on the first change",
                config_path.display()
            ),
            fix: None,
            command: None,
            recheckable: true,
        });
        return;
    }

    let readable = std::fs::read_to_string(config_path).is_ok();
    let (status, detail, fix) = match (readable, writable) {
        (true, true) => (
            DiagnosticStatus::Ok,
            format!("{} is readable and up to date", config_path.display()),
            None,
        ),
        (true, false) => (
            DiagnosticStatus::Warning,
            format!(
                "{} can be read but its directory is not writable",
                config_path.display()
            ),
            Some("Settings would not survive a change. Fix the directory permissions.".to_string()),
        ),
        (false, _) => (
            DiagnosticStatus::Error,
            format!("{} exists but cannot be read", config_path.display()),
            Some(
                "Check the file permissions, or move it aside to fall back to defaults."
                    .to_string(),
            ),
        ),
    };
    items.push(DiagnosticItem {
        id: "config.file".to_string(),
        label: "Settings file".to_string(),
        status,
        detail,
        fix,
        command: None,
        recheckable: true,
    });

    items.push(DiagnosticItem {
        id: "config.values".to_string(),
        label: "Current settings".to_string(),
        status: DiagnosticStatus::Ok,
        detail: format!(
            "master {}, volume {:.0}%, pitch variation {}, gain variation {}, spatial {}, \
             release sounds {}",
            if state.settings.enabled { "on" } else { "off" },
            state.settings.volume * 100.0,
            if state.settings.pitch_variation {
                "on"
            } else {
                "off"
            },
            if state.settings.gain_variation {
                "on"
            } else {
                "off"
            },
            if state.settings.spatial_audio {
                "on"
            } else {
                "off"
            },
            if state.settings.release_sounds {
                "on"
            } else {
                "off"
            },
        ),
        fix: None,
        command: None,
        recheckable: false,
    });
}

/// Whether `dir` allows this user to create files - i.e. whether a settings
/// write could actually land there.
fn directory_writable(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir)
        .map(|meta| meta.permissions().mode() & 0o200 != 0)
        .unwrap_or(false)
}

// -------------------------------------------------------------------- engine

fn engine_checks(state: &State, simulate: bool, items: &mut Vec<DiagnosticItem>) {
    items.push(DiagnosticItem {
        id: "engine.process".to_string(),
        label: "Engine process".to_string(),
        status: DiagnosticStatus::Ok,
        detail: format!(
            "keyvibes {} running as pid {} for {}s",
            state.engine.version, state.engine.pid, state.engine.uptime_secs
        ),
        fix: None,
        command: None,
        recheckable: false,
    });

    if simulate {
        items.push(DiagnosticItem {
            id: "engine.mode".to_string(),
            label: "Input mode".to_string(),
            status: DiagnosticStatus::Warning,
            detail: "simulated input - no physical keyboard is being read".to_string(),
            fix: Some(
                "This mode exists for automated tests. Restart the engine without \
                 `--simulate` to play from a real keyboard."
                    .to_string(),
            ),
            command: None,
            recheckable: false,
        });
    }

    for (index, issue) in state.issues.iter().enumerate() {
        items.push(DiagnosticItem {
            id: format!("engine.issue.{index}"),
            label: "Engine report".to_string(),
            status: DiagnosticStatus::Warning,
            detail: issue.clone(),
            fix: None,
            command: None,
            recheckable: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_control::{AudioInfo, EngineInfo, InputInfo, LiveSettings, PackInfo};

    fn pack() -> PackInfo {
        PackInfo {
            name: "Alpaca".to_string(),
            category: "linear".to_string(),
            path: "/tmp/alpaca.kvpack".to_string(),
            keys: 104,
            release_keys: 0,
            clips: 104,
            sample_rate: 48_000,
            coverage: 104,
            has_release: false,
            author: Some("Mechvibes".to_string()),
            license: Some("MIT".to_string()),
            source: Some("upstream".to_string()),
            description: None,
            synthesized: false,
        }
    }

    fn state() -> State {
        State {
            engine: EngineInfo {
                pid: 4242,
                version: "0.1.0".to_string(),
                simulate: false,
                uptime_secs: 3,
            },
            settings: LiveSettings {
                enabled: true,
                volume: 0.7,
                pitch_variation: true,
                gain_variation: true,
                spatial_audio: true,
                release_sounds: false,
            },
            pack: Some(pack()),
            packs: vec![pack()],
            audio: AudioInfo {
                phase: "running".to_string(),
                output_rate: 48_000,
                output_device: None,
                xruns: 0,
                frames_rendered: 1000,
                peak: 0.5,
                last_error: None,
            },
            input: InputInfo {
                keyboards: 1,
                presses: 10,
                releases: 9,
                missing_sounds: 0,
                commands_generated: 10,
                hotplug: true,
            },
            issues: vec![],
        }
    }

    fn ids(report: &DiagnosticsReport) -> Vec<&str> {
        report.items.iter().map(|item| item.id.as_str()).collect()
    }

    #[test]
    fn report_covers_every_area() {
        let report = run(
            &state(),
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        let found = ids(&report);
        for expected in [
            "input.permission",
            "input.keyboards",
            "audio.pipewire",
            "audio.stream",
            "packs.installed",
            "packs.active",
            "packs.release",
            "config.file",
            "engine.process",
        ] {
            assert!(
                found.contains(&expected),
                "missing check {expected}: {found:?}"
            );
        }
    }

    #[test]
    fn counters_match_the_items() {
        let report = run(
            &state(),
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        let errors = report
            .items
            .iter()
            .filter(|item| item.status == DiagnosticStatus::Error)
            .count() as u32;
        let warnings = report
            .items
            .iter()
            .filter(|item| item.status == DiagnosticStatus::Warning)
            .count() as u32;
        assert_eq!(report.errors, errors);
        assert_eq!(report.warnings, warnings);
        assert_eq!(report.healthy, errors == 0 && warnings == 0);
    }

    #[test]
    fn missing_config_is_not_an_error() {
        let report = run(
            &state(),
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        let item = report
            .items
            .iter()
            .find(|item| item.id == "config.file")
            .expect("config check present");
        assert_eq!(item.status, DiagnosticStatus::Ok);
        assert!(item.detail.contains("defaults are in use"));
    }

    #[test]
    fn incomplete_pack_is_reported_honestly() {
        let mut state = state();
        state.pack.as_mut().unwrap().coverage = 96;
        let report = run(
            &state,
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        let item = report
            .items
            .iter()
            .find(|item| item.id == "packs.active")
            .expect("pack check present");
        assert_eq!(item.status, DiagnosticStatus::Warning);
        assert!(item.detail.contains("96/104"));
        assert!(item.detail.contains("stay silent"));
    }

    #[test]
    fn pack_without_release_clips_is_explained() {
        let report = run(
            &state(),
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        let item = report
            .items
            .iter()
            .find(|item| item.id == "packs.release")
            .expect("release check present");
        assert!(item.detail.contains("not available"));
        assert_eq!(item.status, DiagnosticStatus::Ok);
    }

    #[test]
    fn simulate_mode_is_flagged() {
        let mut state = state();
        state.engine.simulate = true;
        state.input.hotplug = false;
        let report = run(&state, Path::new("/nonexistent/keyvibes/config.toml"), true);
        let mode = report
            .items
            .iter()
            .find(|item| item.id == "engine.mode")
            .expect("mode check present");
        assert_eq!(mode.status, DiagnosticStatus::Warning);
        let hotplug = report
            .items
            .iter()
            .find(|item| item.id == "input.hotplug")
            .expect("hotplug check present");
        assert_eq!(hotplug.status, DiagnosticStatus::Ok);
    }

    #[test]
    fn every_fix_that_ships_a_command_is_a_single_line() {
        let report = run(
            &state(),
            Path::new("/nonexistent/keyvibes/config.toml"),
            false,
        );
        for item in &report.items {
            if let Some(command) = &item.command {
                assert!(!command.contains('\n'), "{} must be copyable", item.id);
                assert!(!command.contains('|'), "{} must not pipe a shell", item.id);
                assert!(
                    !command.contains("sudo sh"),
                    "{} must not escalate",
                    item.id
                );
            }
        }
    }
}
