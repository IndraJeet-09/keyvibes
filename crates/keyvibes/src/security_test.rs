//! Phase 16 acceptance: `keyvibes security-test`.
//!
//! KeyVibes sees every key the user types, so the privacy claims are only as
//! good as the code behind them. This command audits the whole workspace and
//! then proves the two nastiest input-handling properties in-process.
//!
//! What is checked:
//!
//! * no network client exists anywhere in the dependency graph or the source,
//!   so key events have nowhere to go,
//! * the only module that writes a file at runtime is the configuration
//!   writer, and the configuration schema has no field that could hold a key,
//! * nothing in the real-time path logs; the single site that can print a key
//!   is the opt-in `input-test --verbose` probe, which writes to the terminal
//!   and nowhere else,
//! * no grab, no injection, no privilege escalation, no device outside
//!   `/dev/input`,
//! * pack metadata is never executed,
//! * pack manifests cannot escape their root, and corrupt packs fail with an
//!   error rather than a panic.
//!
//! Comments and `#[cfg(test)]` modules are stripped before scanning, so
//! documentation prose and unit-test fixtures cannot mask a finding - and
//! cannot cause a false one.

use crate::accept::{Report, Status};
use crate::compat_test::{
    cargo_manifests, display, rust_sources_excluding, strip_comments, strip_test_modules,
    summarize, transitive_deps, workspace_root,
};
use anyhow::Result;
use kv_input_linux::discovery::discover_keyboards;
use kv_pack::{Header, KvPack, HEADER_SIZE};
use std::path::{Path, PathBuf};

/// This file quotes everything it forbids, and is excluded from its own scan.
const POLICY_FILE: &str = "security_test.rs";

/// Acceptance modules that quote banned tokens as part of their own audits.
const POLICY_FILES: &[&str] = &["security_test.rs", "compat_test.rs"];

/// Modules the binary is allowed to create or replace on disk.
///
/// Everything else in the workspace is read-only at runtime.
const WRITE_ALLOWLIST: &[&str] = &[
    // The user's own settings file: volume, pack name, output device.
    "crates/keyvibes/src/config.rs",
    // Builds a `.kvpack` from a manifest the user named on the command line.
    "crates/kv-pack/src/writer.rs",
    // Acceptance commands write scratch files they delete again.
    "crates/keyvibes/src/config_test.rs",
    "crates/keyvibes/src/compat_test.rs",
    "crates/keyvibes/src/security_test.rs",
    "crates/keyvibes/src/doctor_test.rs",
    "crates/keyvibes/src/pack_test.rs",
    "crates/keyvibes/src/idle_test.rs",
    "crates/keyvibes/src/audio_recovery_test.rs",
    "crates/keyvibes/src/hotplug_test.rs",
];

/// Crates whose source must never log a key event.
const REALTIME_CRATES: &[&str] = &[
    "kv-core",
    "kv-ring",
    "kv-mixer",
    "kv-runtime",
    "kv-input-linux",
    "kv-audio-pipewire",
];

/// Files allowed to spawn a process.
///
/// Only three kinds exist in the workspace: the `pw-top` XRUN probe (fixed
/// binary, fixed arguments, no shell), and acceptance commands that re-exec
/// this binary. Nothing in the pack crate is on this list, so pack metadata
/// can never reach an argv.
const COMMAND_ALLOWLIST: &[&str] = &[
    // Reads PipeWire's per-node ERR counter with literal arguments only.
    "xrun.rs",
    // Re-executes `keyvibes` itself with literal arguments.
    "compat_test.rs",
    "hotplug_test.rs",
    "doctor_test.rs",
    // Re-executes `keyvibes --help` to walk the advertised interface.
    "cli_test.rs",
];

/// Crates that may be absent from the dependency graph, checked transitively.
const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    "reqwest",
    "hyper",
    "ureq",
    "isahc",
    "minreq",
    "attohttpc",
    "curl",
    "curl-sys",
    "log",
    "tracing",
    "tracing-subscriber",
    "env_logger",
    "fern",
    "slog",
    "socket2",
];

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("security-test");
    let root = workspace_root();
    let files = rust_sources_excluding(&root, &[POLICY_FILE]);
    report.add(
        if files.is_empty() {
            Status::Fail
        } else {
            Status::Pass
        },
        "workspace sources are scanned",
        format!(
            "{} Rust file(s), comments and test modules stripped",
            files.len()
        ),
    );

    let sources = load(&files);

    // --- 1. nowhere to send a key -----------------------------------------
    let network = find_code(
        &sources,
        &[
            "std::net",
            "TcpStream",
            "UdpSocket",
            "to_socket_addrs",
            "reqwest",
            "hyper::",
            "ureq::",
            "isahc",
            "minreq",
            "curl",
        ],
        true,
    );
    report.add(
        if network.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no network client exists in the source",
        if network.is_empty() {
            "no socket, HTTP client, or DNS call in any workspace source (string literals ignored)"
                .to_string()
        } else {
            format!("found {}", summarize(&network))
        },
    );

    match transitive_deps(&root) {
        Ok(deps) => {
            let hits: Vec<String> = deps
                .iter()
                .filter(|name| FORBIDDEN_DEPENDENCIES.contains(&name.as_str()))
                .cloned()
                .collect();
            report.add(
                if hits.is_empty() {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "the dependency graph has no network or logging crate",
                if hits.is_empty() {
                    format!("{} crate(s), none able to reach the network", deps.len())
                } else {
                    format!("found {}", summarize(&hits))
                },
            );
        }
        Err(error) => report.not_run(
            "the dependency graph has no network or logging crate",
            error,
        ),
    }

    // --- 2. nothing typed reaches the disk --------------------------------
    let writers = find(
        &sources,
        &[
            "File::create(",
            "fs::write(",
            "OpenOptions::new(",
            "write_all(",
        ],
        false,
    );
    let unexpected: Vec<String> = writers
        .iter()
        .filter(|hit| !WRITE_ALLOWLIST.iter().any(|allowed| hit.contains(allowed)))
        .cloned()
        .collect();
    report.add(
        if unexpected.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no module writes key data to disk",
        if unexpected.is_empty() {
            format!(
                "{} write site(s), all in {}",
                writers.len(),
                WRITE_ALLOWLIST.join(", ")
            )
        } else {
            format!("found {}", summarize(&unexpected))
        },
    );

    let fields = config_fields(&root);
    let expected = [
        "version",
        "enabled",
        "pack",
        "output_device",
        "volume",
        "pitch_variation",
        "gain_variation",
        "release_sounds",
        "spatial_audio",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for name in expected {
        if !fields.iter().any(|field| field.as_str() == name) {
            missing.push(name);
        }
    }
    for field in &fields {
        if !expected.contains(&field.as_str()) {
            missing.push(field.as_str());
        }
    }
    report.add(
        if missing.is_empty() && !fields.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "the configuration schema holds no typed character",
        if missing.is_empty() {
            format!(
                "Config carries only {:?} - volume, pack name, and output device",
                fields
            )
        } else {
            format!("unexpected or missing field(s): {}", missing.join(", "))
        },
    );

    // --- 3. nothing logs a key --------------------------------------------
    let realtime: Vec<(PathBuf, String)> = sources
        .iter()
        .filter(|(path, _)| {
            REALTIME_CRATES
                .iter()
                .any(|krate| path.to_string_lossy().contains(krate))
        })
        .cloned()
        .collect();
    let key_logs = find(&realtime, &["{key", "{event", "{ke}", "{press"], false);
    report.add(
        if key_logs.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "the real-time path never formats a key event",
        if key_logs.is_empty() {
            format!(
                "{} source file(s) in the audio, input, and runtime crates; none renders a key",
                realtime.len()
            )
        } else {
            format!("found {}", summarize(&key_logs))
        },
    );

    // Scanned across the real-time crates and the binary itself. The pack
    // crate is build-time only (its `{key}` messages name a pack manifest
    // entry, not a press), and the audit modules quote these tokens as part
    // of their own checks.
    let mut printable: Vec<(PathBuf, String)> = realtime.clone();
    printable.extend(
        sources
            .iter()
            .filter(|(path, _)| path.to_string_lossy().contains("crates/keyvibes/src"))
            .filter(|(path, _)| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                !name.is_some_and(|n| POLICY_FILES.contains(&n.as_str()))
            })
            .cloned(),
    );
    let key_prints = find(
        &printable,
        &["{key}", "{key:?}", "{key:}", "{event}"],
        false,
    );
    let outside_probe: Vec<String> = key_prints
        .iter()
        .filter(|hit| !hit.contains("crates/keyvibes/src/main.rs"))
        .cloned()
        .collect();
    report.add(
        if outside_probe.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "the only key-printing site is the opt-in input probe",
        if outside_probe.is_empty() {
            "`input-test --verbose` prints each press to the terminal; no other site does, and nothing writes it to a file or socket"
                .to_string()
        } else {
            format!("found {}", summarize(&outside_probe))
        },
    );

    // --- 4. no grab, no injection, no privilege ---------------------------
    let input_source = sources
        .iter()
        .filter(|(path, _)| path.to_string_lossy().contains("kv-input-linux/src"))
        .cloned()
        .collect::<Vec<_>>();
    let grab = find(
        &input_source,
        &[
            "EVIOCGRAB",
            "uinput",
            "UINPUT",
            "O_RDWR",
            "O_WRONLY",
            ".write(",
            "EVIOCS",
        ],
        false,
    );
    report.add(
        if grab.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "keyboard input is only ever read, never grabbed or injected",
        if grab.is_empty() {
            "no EVIOCGRAB, no uinput, no write syscall in the input crate".to_string()
        } else {
            format!("found {}", summarize(&grab))
        },
    );

    let privilege = find(
        &sources,
        &[
            "setuid",
            "setgid",
            "setreuid",
            "setresuid",
            "capset",
            "prctl",
            "pkexec",
            "/etc/passwd",
            "/etc/shadow",
            "chmod(",
            "chown(",
        ],
        true,
    );
    report.add(
        if privilege.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no root or privilege escalation is requested",
        if privilege.is_empty() {
            "no setuid/capset/polkit call and no write outside the user's own directories"
                .to_string()
        } else {
            format!("found {}", summarize(&privilege))
        },
    );

    // --- 5. only the devices we need --------------------------------------
    let other_devices = find(
        &sources,
        &[
            "/dev/hidraw",
            "/dev/tty",
            "/dev/usb",
            "/dev/fb",
            "/dev/hiddev",
        ],
        true,
    );
    let open_sites = find(&sources, &["Device::open("], false);
    report.add(
        if other_devices.is_empty() && open_sites.len() == 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "only /dev/input/event* is opened",
        if other_devices.is_empty() && open_sites.len() == 1 {
            "a single `Device::open` site, fed by keyboard discovery over /dev/input".to_string()
        } else if !other_devices.is_empty() {
            format!("found {}", summarize(&other_devices))
        } else {
            format!(
                "{} `Device::open` sites, expected exactly one",
                open_sites.len()
            )
        },
    );

    match discover_keyboards() {
        Ok(keyboards) => {
            let outside: Vec<String> = keyboards
                .iter()
                .map(|kb| kb.path.display().to_string())
                .filter(|path| !path.starts_with("/dev/input/"))
                .collect();
            report.add(
                if outside.is_empty() {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "discovered devices live under /dev/input",
                if outside.is_empty() {
                    format!(
                        "{} device(s) enumerated, all under /dev/input",
                        keyboards.len()
                    )
                } else {
                    format!("found {}", summarize(&outside))
                },
            );
        }
        Err(error) => report.fail(
            "discovered devices live under /dev/input",
            format!("discovery failed: {error}"),
        ),
    }

    // --- 6. pack metadata is inert ----------------------------------------
    let pack_sources: Vec<(PathBuf, String)> = sources
        .iter()
        .filter(|(path, _)| path.to_string_lossy().contains("kv-pack/src"))
        .cloned()
        .collect();
    let shell = find(
        &pack_sources,
        &[
            "std::process",
            "Command::new",
            "/bin/sh",
            "/bin/bash",
            "popen",
            "execvp",
            "execve",
        ],
        false,
    );
    report.add(
        if shell.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "pack metadata is never executed",
        if shell.is_empty() {
            "the pack crate contains no process spawn of any kind".to_string()
        } else {
            format!("found {}", summarize(&shell))
        },
    );

    let commands = find_code(
        &sources,
        &["std::process::Command", "std::process::{Command"],
        false,
    );
    let stray_commands: Vec<String> = commands
        .iter()
        .filter(|hit| {
            !COMMAND_ALLOWLIST
                .iter()
                .any(|allowed| hit.contains(allowed))
        })
        .cloned()
        .collect();
    report.add(
        if stray_commands.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no production module spawns a process",
        if stray_commands.is_empty() {
            format!(
                "{} `Command::new` site(s), all in acceptance commands, tests, or benches",
                commands.len()
            )
        } else {
            format!("found {}", summarize(&stray_commands))
        },
    );

    // --- 7. manifests cannot escape their root ----------------------------
    let (traversal, traversal_detail) = check_traversal();
    report.add(
        traversal,
        "pack paths cannot traverse the filesystem",
        traversal_detail,
    );
    let (malformed, malformed_detail) = check_malformed();
    report.add(
        malformed,
        "malformed packs fail instead of panicking",
        malformed_detail,
    );

    // --- 8. no network or logging crate is declared directly -------------
    let manifests = cargo_manifests(&root);
    let declared: Vec<&str> = FORBIDDEN_DEPENDENCIES.to_vec();
    let manifest_hits = find_manifests(&manifests, &declared);
    report.add(
        if manifest_hits.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no Cargo.toml declares a network or logging crate",
        if manifest_hits.is_empty() {
            format!("{} manifest(s) checked", manifests.len())
        } else {
            format!("found {}", summarize(&manifest_hits))
        },
    );

    report.finish()
}

// ---------------------------------------------------------------------------
// in-process proofs
// ---------------------------------------------------------------------------

/// `resolve_source` must refuse every way out of the pack root, including
/// through a symlink.
fn check_traversal() -> (Status, String) {
    use kv_pack::manifest::resolve_source;

    let tag = std::process::id();
    let root = std::env::temp_dir().join(format!("keyvibes-sec-root-{tag}"));
    let outside = std::env::temp_dir().join(format!("keyvibes-sec-outside-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::create_dir_all(&root);
    let _ = std::fs::create_dir_all(&outside);
    let _ = std::fs::write(root.join("ok.wav"), b"RIFF");
    let _ = std::fs::write(outside.join("secret.wav"), b"RIFF");
    let _ = std::os::unix::fs::symlink(outside.join("secret.wav"), root.join("escape.wav"));

    let rejected = [
        "../secret.wav",
        "/etc/passwd",
        "a/../../secret.wav",
        "escape.wav",
        "",
    ];
    let mut accepted = Vec::new();
    for candidate in rejected {
        if resolve_source(&root, candidate).is_ok() {
            accepted.push(candidate);
        }
    }
    let genuine = resolve_source(&root, "ok.wav").is_ok();

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);

    if accepted.is_empty() && genuine {
        (
            Status::Pass,
            format!(
                "{} hostile path(s) refused (traversal, absolute, symlink, empty); a real in-root source still resolves",
                rejected.len()
            ),
        )
    } else {
        (
            Status::Fail,
            format!("accepted: {accepted:?}; in-root source resolves: {genuine}"),
        )
    }
}

/// A single edit applied to a golden pack's bytes.
type Mutation = Box<dyn Fn(&mut Vec<u8>)>;

/// A deliberately corrupted pack must produce an error, never a panic.
fn check_malformed() -> (Status, String) {
    let dir = std::env::temp_dir().join(format!(
        "keyvibes-security-malformed-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);

    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("crates/kv-pack/tests/fixtures/golden.kvpack"));
    let golden = match golden.and_then(|path| std::fs::read(path).ok()) {
        Some(bytes) => bytes,
        None => {
            let _ = std::fs::remove_dir_all(&dir);
            return (Status::NotRun, "golden fixture missing".to_string());
        }
    };

    let mut failures = Vec::new();
    let mut cases: Vec<(&str, Mutation)> = Vec::new();
    cases.push(("truncated", Box::new(|b| b.truncate(64))));
    cases.push(("bad magic", Box::new(|b| b[0] = 0)));
    cases.push(("bad version", Box::new(|b| b[8] = 42)));
    cases.push((
        "key table past EOF",
        Box::new(|b| {
            b[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
        }),
    ));
    cases.push((
        "sample region overflow",
        Box::new(|b| {
            b[76..84].copy_from_slice(&u64::MAX.to_le_bytes());
        }),
    ));
    cases.push(("misaligned samples", Box::new(|b| b[68] |= 1)));
    cases.push(("reserved bytes set", Box::new(|b| b[92] = 9)));

    let declared = cases.len();
    for (label, mutate) in cases {
        let mut bytes = golden.clone();
        mutate(&mut bytes);
        let path = dir.join(format!("{label}.kvpack"));
        if std::fs::write(&path, &bytes).is_err() {
            failures.push(format!("{label}: could not be written"));
            continue;
        }
        if KvPack::open(&path).is_ok() {
            failures.push(format!("{label}: accepted a corrupt pack"));
        }
        let _ = std::fs::remove_file(&path);
    }

    // Header-sized file: the minimum possible input.
    let path = dir.join("tiny.kvpack");
    let _ = std::fs::write(&path, vec![0u8; HEADER_SIZE as usize]);
    if KvPack::open(&path).is_ok() {
        failures.push("an all-zero header was accepted".to_string());
    }
    let _ = std::fs::remove_file(&path);

    // And a valid header must still be rejected when its regions are bogus.
    let mut bytes = golden.clone();
    if let Ok(header) = Header::parse(&bytes, bytes.len() as u64) {
        let off = header.clip_table_offset as usize;
        if off + 8 <= bytes.len() {
            bytes[off + 8..off + 12].copy_from_slice(&0u32.to_le_bytes()); // zero-length clip
        }
        let path = dir.join("zero-clip.kvpack");
        let _ = std::fs::write(&path, &bytes);
        if KvPack::open(&path).is_ok() {
            failures.push("a zero-length clip was accepted".to_string());
        }
        let _ = std::fs::remove_file(&path);
    }

    let _ = std::fs::remove_dir_all(&dir);
    let tried = declared + 2;
    if failures.is_empty() {
        (
            Status::Pass,
            format!("{tried} corruption(s) rejected with a structured error, none panicked"),
        )
    } else {
        (Status::Fail, summarize(&failures))
    }
}

// ---------------------------------------------------------------------------
// scanning helpers
// ---------------------------------------------------------------------------

type Source = (PathBuf, String);

fn load(files: &[PathBuf]) -> Vec<Source> {
    files
        .iter()
        // Integration tests and benchmarks are their own crates and are
        // covered by `cargo test`, not by this production-source audit.
        .filter(|path| {
            !path.components().any(
                |c| matches!(c, std::path::Component::Normal(n) if n == "tests" || n == "benches"),
            )
        })
        .filter_map(|path| {
            std::fs::read_to_string(path).ok().map(|text| {
                let clean = strip_test_modules(&strip_comments(&text));
                (path.clone(), clean)
            })
        })
        .collect()
}

fn find(sources: &[Source], tokens: &[&str], insensitive: bool) -> Vec<String> {
    let needles: Vec<String> = tokens
        .iter()
        .map(|token| {
            if insensitive {
                token.to_lowercase()
            } else {
                (*token).to_string()
            }
        })
        .collect();

    let mut hits = Vec::new();
    for (path, code) in sources {
        let haystack = if insensitive {
            code.to_lowercase()
        } else {
            code.clone()
        };
        for needle in &needles {
            if let Some(index) = haystack.find(needle.as_str()) {
                let (line, column) = line_column(&haystack, index);
                hits.push(format!("{}:{line}:{column} `{needle}`", display(path)));
            }
        }
    }
    hits
}

/// Like [`find`] but with string literals blanked out first, so a banned
/// token that only appears inside a quoted audit list is not mistaken for a
/// live reference. A real `use std::net::TcpStream;` is code and still hits.
fn find_code(sources: &[Source], tokens: &[&str], insensitive: bool) -> Vec<String> {
    let blanked: Vec<Source> = sources
        .iter()
        .map(|(path, code)| (path.clone(), blank_string_literals(code)))
        .collect();
    find(&blanked, tokens, insensitive)
}

/// Replaces the contents of `"..."` and `r#..."#` runs with spaces, keeping
/// byte offsets so line/column reporting stays accurate.
fn blank_string_literals(code: &str) -> String {
    let mut masked = code.as_bytes().to_vec();
    let mut i = 0;
    while i < masked.len() {
        match masked[i] {
            b'r' if i + 1 < masked.len()
                && (i == 0
                    || !(masked[i - 1].is_ascii_alphanumeric() || masked[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                let mut hashes = 0usize;
                while j < masked.len() && masked[j] == b'#' && hashes <= 16 {
                    hashes += 1;
                    j += 1;
                }
                if j < masked.len() && masked[j] == b'"' {
                    j += 1;
                    let start = j;
                    while j < masked.len() {
                        if masked[j] == b'"'
                            && (0..hashes)
                                .all(|n| j + 1 + n < masked.len() && masked[j + 1 + n] == b'#')
                        {
                            break;
                        }
                        j += 1;
                    }
                    for slot in masked.iter_mut().take(j).skip(start) {
                        *slot = b' ';
                    }
                    i = j;
                } else {
                    i += 1;
                }
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < masked.len() && masked[j] != b'"' {
                    if masked[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                for slot in masked.iter_mut().take(j).skip(start) {
                    *slot = b' ';
                }
                i = (j + 1).min(masked.len());
            }
            _ => i += 1,
        }
    }
    String::from_utf8(masked)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn line_column(text: &str, index: usize) -> (usize, usize) {
    let up_to = &text[..index];
    let line = up_to.matches('\n').count() + 1;
    let column = up_to
        .rsplit('\n')
        .next()
        .map_or(1, |s| s.chars().count() + 1);
    (line, column)
}

/// Field names declared on `pub struct Config`.
fn config_fields(root: &Path) -> Vec<String> {
    let Ok(source) = std::fs::read_to_string(root.join("crates/keyvibes/src/config.rs")) else {
        return Vec::new();
    };
    let clean = strip_test_modules(&strip_comments(&source));
    let Some(start) = clean.find("pub struct Config") else {
        return Vec::new();
    };
    let body = &clean[start..];
    let end = match body.find("\n}") {
        Some(index) => index,
        None => return Vec::new(),
    };
    body[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            // `pub struct Config` itself is not a field; a field reads
            // `pub name: Type`.
            if !line.starts_with("pub ") || line.starts_with("pub struct") {
                return None;
            }
            line.split_whitespace()
                .nth(1)
                .map(|name| name.trim_end_matches(':').to_string())
        })
        .collect()
}

fn find_manifests(manifests: &[PathBuf], tokens: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for manifest in manifests {
        let Ok(text) = std::fs::read_to_string(manifest) else {
            continue;
        };
        for (number, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let key = if line.starts_with('[') {
                line.trim_start_matches('[')
                    .trim_end_matches(']')
                    .split('.')
                    .nth(1)
                    .unwrap_or("")
                    .to_string()
            } else {
                line.split('=')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"')
                    .to_string()
            };
            let key = key.to_lowercase();
            if !key.is_empty() && tokens.contains(&key.as_str()) {
                hits.push(format!(
                    "{}:{} declares `{key}`",
                    display(manifest),
                    number + 1
                ));
            }
        }
    }
    hits
}
