//! `keyvibes doctor` - does this machine actually run KeyVibes?
//!
//! Every check here runs on the control plane: no key is read, nothing is
//! grabbed, and the process exits without leaving a stream or a thread
//! behind. The report is a fixed set of fields so it can be diffed between
//! two machines or two moments on the same machine.
//!
//! Exit status: `0` when nothing is wrong, `1` when at least one **error**
//! was found. Warnings (a keyboard that is not plugged in yet, say) are
//! reported but do not fail the run, because the hot-plug path is exactly
//! what handles them.

use anyhow::Result;
use kv_audio_pipewire::PipeWireStream;
use kv_core::PlayCommand;
use kv_pack::KvPack;
use kv_ring::SpscRing;
use kv_runtime::Runtime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::pack_locate;

/// How hard a finding should push on the exit status.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// Something is missing that the engine cannot work without.
    Error,
    /// Reported so it is visible, but the engine copes.
    Warning,
}

impl Severity {
    fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One thing wrong with this machine, in the shape a person can act on.
struct Problem {
    /// What is wrong, in a few words.
    what: String,
    /// Why KeyVibes considers it wrong.
    why: String,
    /// The command or edit that fixes it.
    fix: String,
    severity: Severity,
}

/// A `label  value` row inside a section.
type Row = (String, String);

/// Widenest label used in the header block.
const HEADER_WIDTH: usize = 16;
/// Widest label used inside a section.
const SECTION_WIDTH: usize = 16;

/// Diagnoses this machine and exits `1` when a real problem was found.
///
/// `selector` is the global `--pack` argument, if the operator passed one.
pub fn run(selector: Option<&str>) -> Result<()> {
    // ---------------------------------------------------------------- gather
    let mut problems: Vec<Problem> = Vec::new();

    let config_path = Config::default_path();
    let (config, config_rows) = gather_config(&config_path, &mut problems);
    if !config.enabled {
        problems.push(Problem {
            what: "KeyVibes is switched off in the configuration".to_string(),
            why: "the file sets `enabled = false`, so `keyvibes run` refuses to start".to_string(),
            fix: "set `enabled = true` in the configuration file".to_string(),
            severity: Severity::Warning,
        });
    }

    let (packs, installed_row, selected_row) = gather_packs(selector, &config, &mut problems);
    let input_rows = gather_input(&mut problems);
    let audio = gather_audio(&config, packs.selected.as_deref(), &mut problems);

    // ----------------------------------------------------------------- print
    println!("KeyVibes doctor");
    println!("===============");
    println!();

    print_header(&[
        ("Version", env!("CARGO_PKG_VERSION")),
        ("OS", std::env::consts::OS),
        ("Kernel", &kernel_release()),
        ("Architecture", std::env::consts::ARCH),
    ]);

    println!("Input");
    print_rows(&input_rows);
    println!();

    println!("Audio");
    print_rows(&[
        (
            "PipeWire".to_string(),
            match &audio.availability {
                Availability::Up => "available".to_string(),
                Availability::Down(reason) => format!("unavailable - {reason}"),
            },
        ),
        ("default output".to_string(), audio.sink.clone()),
        ("sample rate".to_string(), audio.rate_line.clone()),
        ("quantum".to_string(), audio.quantum_line.clone()),
    ]);
    if let Some(detail) = &audio.note {
        println!("  {detail}");
    }
    println!();

    println!("Packs");
    print_rows(&[installed_row, selected_row]);
    println!();

    println!("Runtime");
    print_rows(&config_rows);
    println!();

    // -------------------------------------------------------------- problems
    if problems.is_empty() {
        println!("No problems found.");
        return Ok(());
    }

    println!("Problems");
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for (index, problem) in problems.iter().enumerate() {
        match problem.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
        }
        println!(
            "  {}. [{}] {}",
            index + 1,
            problem.severity.name(),
            problem.what
        );
        print_indented("       why:  ", "            ", &problem.why);
        print_indented("       fix:  ", "            ", &problem.fix);
    }
    println!();
    println!(
        "{errors} error(s), {warnings} warning(s) - {}",
        if errors == 0 {
            "healthy enough to run"
        } else {
            "not ready to run"
        }
    );

    if errors > 0 {
        // Nothing is holding a file or a thread at this point: everything
        // gathered above was dropped before we printed.
        std::process::exit(1);
    }
    Ok(())
}

/// PipeWire connectivity, as reported by a short-lived stream.
enum Availability {
    /// A stream connected and the server accepted it.
    Up,
    /// It did not, with the reason the backend gave.
    Down(String),
}

/// Everything the `Audio` section prints.
struct AudioReport {
    availability: Availability,
    sink: String,
    rate_line: String,
    quantum_line: String,
    note: Option<String>,
}

// --------------------------------------------------------------------- config

/// Reads (or falls back from) the configuration file.
///
/// Returns the config to use and the four `Runtime` rows, whatever happened.
fn gather_config(
    path: &std::result::Result<PathBuf, crate::config::ConfigError>,
    problems: &mut Vec<Problem>,
) -> (Config, Vec<Row>) {
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            problems.push(Problem {
                what: "the configuration file location is unknown".to_string(),
                why: error.to_string(),
                fix: "set `$KEYVIBES_CONFIG` to a file path, or set `$XDG_CONFIG_HOME`".to_string(),
                severity: Severity::Error,
            });
            return (Config::default(), runtime_rows(None, false));
        }
    };

    match Config::load(path) {
        Ok(config) => {
            let exists = path.is_file();
            (config, runtime_rows(Some(path), exists))
        }
        Err(error) => {
            problems.push(Problem {
                what: "the configuration file cannot be used".to_string(),
                why: error.to_string(),
                fix: format!(
                    "edit {} to correct it, or delete it to fall back to every default",
                    path.display()
                ),
                severity: Severity::Error,
            });
            // A broken file still tells us what it *tried* to say is on the
            // label next to it; the values shown are the safe defaults.
            (Config::default(), runtime_rows(Some(path), path.is_file()))
        }
    }
}

/// The four rows of the `Runtime` section.
fn runtime_rows(path: Option<&Path>, exists: bool) -> Vec<Row> {
    let config = path
        .and_then(|path| Config::load(path).ok())
        .unwrap_or_default();
    let location = match path {
        Some(path) => format!(
            "{} ({})",
            path.display(),
            if exists {
                "file"
            } else {
                "no file - built-in defaults"
            }
        ),
        None => "(unset)".to_string(),
    };
    vec![
        ("config".to_string(), location),
        (
            "enabled".to_string(),
            if config.enabled {
                "yes".to_string()
            } else {
                "no - KeyVibes will not start (set `enabled = true`)".to_string()
            },
        ),
        ("volume".to_string(), format!("{:.2}", config.volume)),
        (
            "output device".to_string(),
            config
                .output_device
                .clone()
                .unwrap_or_else(|| "<default sink>".to_string()),
        ),
    ]
}

// ---------------------------------------------------------------------- packs

/// Installed packs and the one that would be loaded.
struct PackReport {
    selected: Option<PathBuf>,
}

fn gather_packs(
    selector: Option<&str>,
    config: &Config,
    problems: &mut Vec<Problem>,
) -> (PackReport, Row, Row) {
    let installed = pack_locate::discover();
    let names: Vec<String> = installed
        .iter()
        .map(|path| {
            KvPack::open(path)
                .map(|p| p.stats().name)
                .unwrap_or_else(|_| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("?")
                        .to_string()
                })
        })
        .collect();

    if installed.is_empty() {
        let searched = pack_locate::search_dirs()
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        problems.push(Problem {
            what: "no sound pack is installed".to_string(),
            why: format!("searched {searched}"),
            fix: "build one: `keyvibes pack build assets/soundpacks/default-src/pack.toml -o \
                  assets/soundpacks/Default.kvpack`"
                .to_string(),
            severity: Severity::Error,
        });
    }

    // `select_pack` already reports "nothing is installed" and "that name is
    // not installed" with the directories it searched, so it - not a second
    // message written here - is the single source of that wording. Pushing
    // both would bury the selector the user actually typed.
    let installed_row = (
        "installed".to_string(),
        if installed.is_empty() {
            "0".to_string()
        } else {
            format!("{}  {}", installed.len(), names.join(", "))
        },
    );

    let selected = match crate::select_pack(selector, config) {
        Ok(path) => path,
        Err(error) => {
            problems.push(Problem {
                what: "the selected sound pack cannot be located".to_string(),
                why: format!("{error:#}"),
                fix: "`keyvibes pack list` shows what is installed; set `pack = \"Name\"` in the \
                      config, or pass `--pack Name`"
                    .to_string(),
                severity: Severity::Error,
            });
            return (
                PackReport { selected: None },
                installed_row,
                ("selected".to_string(), "<none>".to_string()),
            );
        }
    };

    match KvPack::open(&selected) {
        Ok(loaded) => loaded.stats().name,
        Err(error) => {
            problems.push(Problem {
                what: "the selected sound pack is not readable".to_string(),
                why: format!("{}: {error}", selected.display()),
                fix: format!("check it: `keyvibes pack validate {}`", selected.display()),
                severity: Severity::Error,
            });
            String::new()
        }
    };

    (
        PackReport {
            selected: Some(selected.clone()),
        },
        installed_row,
        (
            "selected".to_string(),
            format!("{}  ({})", selected_name_for(&selected), selected.display()),
        ),
    )
}

/// Display name of a pack we have already opened once.
fn selected_name_for(path: &Path) -> String {
    KvPack::open(path)
        .map(|loaded| loaded.stats().name)
        .unwrap_or_else(|_| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("?")
                .to_string()
        })
}

// ---------------------------------------------------------------------- input

/// Resolves everything the `Input` section prints.
fn gather_input(problems: &mut Vec<Problem>) -> Vec<Row> {
    let dir = Path::new("/dev/input");
    if !dir.exists() {
        problems.push(Problem {
            what: "/dev/input does not exist".to_string(),
            why: "KeyVibes reads Linux evdev device nodes directly, and none are present - a \
                  container started without them, or a non-Linux host"
                .to_string(),
            fix: "run on a Linux host, or start the container with `--device=/dev/input`"
                .to_string(),
            severity: Severity::Error,
        });
        return vec![
            ("keyboards".to_string(), "0".to_string()),
            ("/dev/input".to_string(), "missing".to_string()),
        ];
    }

    let nodes = event_nodes();
    let unreadable = nodes.iter().find(|node| {
        std::fs::File::open(node)
            .err()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    });

    if let Some(node) = unreadable {
        problems.push(Problem {
            what: format!("cannot read {}", node.display()),
            why: "the node is `root:input 0660` and your user is not in the `input` group, so \
                  KeyVibes would never see a key press"
                .to_string(),
            fix: "sudo usermod -aG input $USER, then log out and log back in".to_string(),
            severity: Severity::Error,
        });
        return vec![
            (
                "keyboards".to_string(),
                "0 (device nodes are not readable by this user)".to_string(),
            ),
            ("/dev/input".to_string(), "not readable".to_string()),
        ];
    }

    if nodes.is_empty() {
        problems.push(Problem {
            what: "there are no `/dev/input/event*` nodes".to_string(),
            why: "the directory exists but holds no evdev device, so nothing can be discovered"
                .to_string(),
            fix: "attach a keyboard (or pass the device through to this VM/container)".to_string(),
            severity: Severity::Warning,
        });
        return vec![
            ("keyboards".to_string(), "0".to_string()),
            ("/dev/input".to_string(), "empty".to_string()),
        ];
    }

    let keyboards = match kv_input_linux::discovery::discover_keyboards() {
        Ok(found) => found.len(),
        Err(error) => {
            problems.push(Problem {
                what: "input devices cannot be enumerated".to_string(),
                why: format!("{error}"),
                fix: "check `/dev/input` permissions and that evdev is available".to_string(),
                severity: Severity::Error,
            });
            0
        }
    };

    if keyboards == 0 {
        problems.push(Problem {
            what: "no keyboard is attached right now".to_string(),
            why: "every readable device failed the keyboard test - a mouse, a power button, or \
                  simply nothing plugged in"
                .to_string(),
            fix: "plug a keyboard in; KeyVibes attaches to it as soon as it appears (hot-plug)"
                .to_string(),
            severity: Severity::Warning,
        });
    }

    vec![
        (
            "keyboards".to_string(),
            if keyboards == 0 {
                "0 (waiting for one to appear)".to_string()
            } else {
                format!("{keyboards}")
            },
        ),
        (
            "/dev/input".to_string(),
            format!("readable ({} event node(s))", nodes.len()),
        ),
    ]
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

// ---------------------------------------------------------------------- audio

/// Connects to PipeWire, briefly runs the engine if there is a pack to run
/// it with, then shuts everything down again.
fn gather_audio(config: &Config, pack: Option<&Path>, problems: &mut Vec<Problem>) -> AudioReport {
    let sink = config
        .output_device
        .clone()
        .unwrap_or_else(|| "session default (PipeWire)".to_string());

    // With a pack we can ask the engine itself for the negotiated rate and
    // quantum; without one we only need to know the server answers.
    if let Some(pack) = pack {
        let mut runtime = Runtime::new(48_000);
        runtime.set_settings(config.settings());
        runtime.set_output_device(config.output_device.clone());
        if let Err(error) = runtime.load_pack(pack) {
            problems.push(Problem {
                what: "the selected sound pack failed to load".to_string(),
                why: format!("{}: {error}", pack.display()),
                fix: format!("check it: `keyvibes pack validate {}`", pack.display()),
                severity: Severity::Error,
            });
            runtime.shutdown();
            return AudioReport {
                availability: Availability::Down("no loadable pack".to_string()),
                sink,
                rate_line: "not started".to_string(),
                quantum_line: "not started".to_string(),
                note: None,
            };
        }

        match runtime.start_audio() {
            Ok(()) => {
                let active = runtime.wait_for_audio(Duration::from_secs(2));
                let stats = runtime.audio_stats();
                let rate = stats.negotiated_rate;
                let quantum_ns = stats.quantum_ns;
                runtime.shutdown();

                if !active {
                    problems.push(Problem {
                        what: "the audio stream did not become active".to_string(),
                        why: "PipeWire accepted the stream but no buffer was delivered within \
                              two seconds"
                            .to_string(),
                        fix: "check `pw-top` for a running sink, and that PipeWire is not muted \
                              or suspended"
                            .to_string(),
                        severity: Severity::Warning,
                    });
                }

                let shown_rate = if rate > 0 { rate } else { 48_000 };
                AudioReport {
                    availability: Availability::Up,
                    sink,
                    rate_line: if rate > 0 {
                        format!("{rate} Hz (negotiated)")
                    } else {
                        format!("{shown_rate} Hz (requested; stream not active)")
                    },
                    quantum_line: quantum_line(quantum_ns, shown_rate),
                    note: None,
                }
            }
            Err(error) => {
                runtime.shutdown();
                problems.push(Problem {
                    what: "a PipeWire stream could not be created".to_string(),
                    why: format!("{error}"),
                    fix: "start your desktop audio session: `systemctl --user start pipewire \
                          wireplumber`, then re-run `keyvibes doctor`"
                        .to_string(),
                    severity: Severity::Error,
                });
                AudioReport {
                    availability: Availability::Down(error.to_string()),
                    sink,
                    rate_line: "unavailable".to_string(),
                    quantum_line: "unavailable".to_string(),
                    note: None,
                }
            }
        }
    } else {
        let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(16));
        match PipeWireStream::new(48_000, queue) {
            Ok(_probe) => AudioReport {
                availability: Availability::Up,
                sink,
                rate_line: "48000 Hz (requested; no pack to negotiate with)".to_string(),
                quantum_line: "unknown (no pack to start the engine with)".to_string(),
                note: None,
            },
            Err(error) => {
                problems.push(Problem {
                    what: "PipeWire is not reachable".to_string(),
                    why: format!("{error}"),
                    fix: "start your desktop audio session: `systemctl --user start pipewire \
                          wireplumber`, then re-run `keyvibes doctor`"
                        .to_string(),
                    severity: Severity::Error,
                });
                AudioReport {
                    availability: Availability::Down(error.to_string()),
                    sink,
                    rate_line: "unavailable".to_string(),
                    quantum_line: "unavailable".to_string(),
                    note: None,
                }
            }
        }
    }
}

/// `quantum_ns` rendered as frames and milliseconds at `rate`.
fn quantum_line(quantum_ns: u64, rate: u32) -> String {
    if quantum_ns == 0 || rate == 0 {
        return "unknown (the stream has not delivered a buffer yet)".to_string();
    }
    let frames = (quantum_ns.saturating_mul(u64::from(rate)) + 500_000_000) / 1_000_000_000;
    let ms = f64::from(quantum_ns as u32) / 1_000_000.0;
    format!("{frames} frames ({ms:.2} ms)")
}

// -------------------------------------------------------------------- printing

fn print_header(rows: &[(&str, &str)]) {
    let width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .max(HEADER_WIDTH);
    for (label, value) in rows {
        println!("  {label:<width$} {value}");
    }
    println!();
}

/// Prints `first {text}`, keeping continuation lines aligned under it.
///
/// Several underlying error messages are multi-line (a TOML parse error, the
/// list of packs that were searched); without this they would lose their
/// indent and read as if they had escaped the report.
fn print_indented(first: &str, continuation: &str, text: &str) {
    for (index, line) in text.lines().enumerate() {
        if index == 0 {
            println!("{first}{line}");
        } else if line.is_empty() {
            println!();
        } else {
            println!("{continuation}{line}");
        }
    }
}

fn print_rows(rows: &[Row]) {
    for (label, value) in rows {
        println!("  {label:<SECTION_WIDTH$} {value}");
    }
}

fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
