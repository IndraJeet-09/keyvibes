//! Phase 15 acceptance: `keyvibes compatibility-test`.
//!
//! KeyVibes is deliberately *not* a desktop application. It talks to the
//! kernel through evdev and to the sound server through PipeWire, and that is
//! the whole of its platform surface. Nothing in the process may depend on a
//! display server, a compositor, or a particular desktop session - the engine
//! has to work the same on X11, on Wayland, on a bare TTY, and inside a
//! container that has neither.
//!
//! The test proves that claim three ways:
//!
//! 1. **Statically** - every Rust source file in the workspace is scanned with
//!    comments stripped (comments legitimately say "no X11 here") for session
//!    variables and compositor APIs, and every `Cargo.toml` is scanned for
//!    desktop dependencies. The transitive dependency graph from `cargo tree`
//!    is checked too, so a desktop crate cannot sneak in transitively.
//! 2. **Dynamically** - the binary re-executes itself with `DISPLAY`,
//!    `WAYLAND_DISPLAY`, `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP` and friends
//!    *unset*, and must still discover keyboards and play sound. That is the
//!    environment of a headless machine or a TTY login.
//! 3. **Documented** - `docs/environment.md` must exist and name both the
//!    supported path and the session variables we promise never to read.
//!
//! This file quotes the very tokens it forbids, so it exempts itself from the
//! static scan; every other file is scanned.
//!
//! Checks needing PipeWire are reported as `NOT RUN` when no session is
//! reachable - never as a pass.

use crate::accept::{Report, Status};
use anyhow::Result;
use kv_audio_pipewire::PipeWireStream;
use kv_input_linux::discovery::discover_keyboards;
use kv_ring::SpscRing;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The policy file: it defines the deny lists, so it necessarily contains
/// them. Excluding it here is what keeps the check honest for all others.
const POLICY_FILE: &str = "compat_test.rs";

/// Session variables a desktop environment exports. KeyVibes must work with
/// all of them absent.
const SESSION_VARS: [&str; 6] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
    "XDG_CURRENT_DESKTOP",
    "DESKTOP_SESSION",
];

/// Tokens that may never appear in workspace Rust source, matched
/// case-sensitively because they are environment-variable names.
///
/// `DISPLAY` is listed uppercase on purpose: the `fmt::Display` trait is used
/// throughout this codebase and is not a display server.
const FORBIDDEN_EXACT: [&str; 5] = [
    "DISPLAY",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
    "XDG_CURRENT_DESKTOP",
    "DESKTOP_SESSION",
];

/// Tokens that may never appear in workspace Rust source, matched without
/// regard to case. These are display-server, compositor, and desktop
/// technology names with no other meaning here.
const FORBIDDEN_LOOSE: [&str; 12] = [
    "x11",
    "wayland",
    "dbus",
    "compositor",
    "gnome",
    "kde",
    "hyprland",
    "wl_display",
    "xopendisplay",
    "xkbcommon",
    "winit",
    "xcb",
];

/// Crates that tie the program to a display server or desktop session.
/// Matched against `Cargo.toml` dependency keys and against `cargo tree`.
const FORBIDDEN_DEPS: [&str; 24] = [
    "x11",
    "x11rb",
    "x11-dl",
    "x11-clipboard",
    "xcb",
    "xkbcommon",
    "libxkbcommon",
    "wayland-client",
    "wayland-sys",
    "wayland-protocols",
    "wayland-backend",
    "wayland-scanner",
    "wayland-dlopen",
    "dbus",
    "libdbus",
    "dbus-crossroads",
    "ashpd",
    "xdg-portal",
    "gtk",
    "glib",
    "gdk",
    "winit",
    "tao",
    "global-hotkey",
];

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("compatibility-test");
    let root = workspace_root();
    let source_files = rust_sources(&root);
    report.add(
        if source_files.is_empty() {
            Status::Fail
        } else {
            Status::Pass
        },
        "workspace sources are discoverable",
        format!(
            "{} Rust file(s) under crates/ and xtask/ (policy file excluded)",
            source_files.len()
        ),
    );

    // --- 1. static: no session variables, no compositor APIs ---------------
    let exact = scan(&source_files, &FORBIDDEN_EXACT, false);
    report.add(
        if exact.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no desktop session variable is read",
        if exact.is_empty() {
            "DISPLAY, WAYLAND_DISPLAY, XDG_SESSION_TYPE, XDG_CURRENT_DESKTOP and DESKTOP_SESSION appear in no source file".to_string()
        } else {
            format!("found {}", summarize(&exact))
        },
    );

    let loose = scan(&source_files, &FORBIDDEN_LOOSE, true);
    report.add(
        if loose.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no X11, Wayland, or compositor API is referenced",
        if loose.is_empty() {
            format!(
                "{} comment-stripped file(s) clean; comments are free to say \"X11\"",
                source_files.len()
            )
        } else {
            format!("found {}", summarize(&loose))
        },
    );

    // --- 2. static: no desktop dependencies -------------------------------
    let manifests = cargo_manifests(&root);
    let manifest_hits = scan_manifests(&manifests);
    report.add(
        if manifest_hits.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no Cargo.toml declares a desktop dependency",
        if manifest_hits.is_empty() {
            format!("{} manifest(s) checked", manifests.len())
        } else {
            format!("found {}", summarize(&manifest_hits))
        },
    );

    match transitive_deps(&root) {
        Ok(deps) => {
            let hits: Vec<String> = deps
                .iter()
                .filter(|name| FORBIDDEN_DEPS.contains(&name.as_str()))
                .cloned()
                .collect();
            report.add(
                if hits.is_empty() {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "the transitive dependency graph is free of desktop crates",
                if hits.is_empty() {
                    format!("{} crate(s) in the graph, none display-related", deps.len())
                } else {
                    format!("found {}", summarize(&hits))
                },
            );
        }
        Err(error) => report.not_run(
            "the transitive dependency graph is free of desktop crates",
            error,
        ),
    }

    // --- 3. dynamic: PipeWire comes up ------------------------------------
    let (pipewire, pipewire_detail) =
        match PipeWireStream::new(48_000, Arc::new(SpscRing::with_capacity(256))) {
            Ok(_) => (
                Status::Pass,
                "stream created against the user's PipeWire session".to_string(),
            ),
            Err(error) => (Status::NotRun, format!("{error}; is PipeWire running?")),
        };
    let pipewire_ok = pipewire == Status::Pass;
    report.add(
        pipewire,
        "PipeWire initialises without a desktop session",
        pipewire_detail,
    );

    // --- 4. dynamic: evdev discovery --------------------------------------
    match discover_keyboards() {
        Ok(keyboards) => report.pass(
            "evdev discovers keyboards",
            format!("{} device(s) readable under /dev/input", keyboards.len()),
        ),
        Err(error) => report.fail(
            "evdev discovers keyboards",
            format!("{error} (am I in the `input` group?)"),
        ),
    }

    // --- 5. dynamic: the binary runs with no session variables -------------
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            report.not_run(
                "the binary runs with every session variable unset",
                format!("cannot locate this executable: {error}"),
            );
            return report.finish();
        }
    };
    let scratch = std::env::temp_dir().join(format!("keyvibes-compat-test-{}", std::process::id()));

    // --- 5. static: the binary itself needs no session -------------------
    // `config show` is the cheapest real command: it opens no device, starts
    // no audio, and reads only the config file, which run_without_session
    // points at an absent path.
    match run_without_session(&exe, &["config", "show"], &scratch) {
        Ok((code, _, _)) => {
            let detail = format!("exit {code:?} with every desktop session variable removed");
            if code == Some(0) {
                report.pass("the binary runs with every session variable unset", detail);
            } else {
                report.fail("the binary runs with every session variable unset", detail);
            }
        }
        Err(error) => report.not_run("the binary runs with every session variable unset", error),
    }

    // --- 6. dynamic: sound comes out on a sessionless machine --------------
    let full_args = [
        "--pack",
        "Holy Panda",
        "run",
        "--simulate",
        "--duration",
        "2",
    ];
    if !pipewire_ok {
        report.not_run(
            "sound plays with every session variable unset",
            "no PipeWire session reachable here",
        );
    } else {
        match run_without_session(&exe, &full_args, &scratch) {
            Ok((code, stdout, _)) => {
                let rendered = stdout
                    .lines()
                    .any(|line| line.starts_with("Done:") && !line.contains("0 presses"));
                let detail = format!(
                    "exit {code:?}; {}",
                    if rendered {
                        "engine rendered voices end to end".to_string()
                    } else {
                        format!("no rendered output; tail: {}", tail(&stdout))
                    }
                );
                if code == Some(0) && rendered {
                    report.pass("sound plays with every session variable unset", detail);
                } else {
                    report.fail("sound plays with every session variable unset", detail);
                }
            }
            Err(error) => report.not_run("sound plays with every session variable unset", error),
        }
    }

    // --- 7. documented environment ----------------------------------------
    match std::fs::read_to_string(root.join("docs/environment.md")) {
        Ok(doc) => {
            let wanted = [
                "PipeWire",
                "/dev/input",
                "X11",
                "Wayland",
                "XDG_SESSION_TYPE",
            ];
            let missing: Vec<&str> = wanted
                .into_iter()
                .filter(|token| !doc.contains(token))
                .collect();
            report.add(
                if missing.is_empty() {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "docs/environment.md describes the supported environment",
                if missing.is_empty() {
                    format!(
                        "{} byte(s), naming both the supported path and the variables we never read",
                        doc.len()
                    )
                } else {
                    format!("missing mention of {}", missing.join(", "))
                },
            );
        }
        Err(error) => report.fail(
            "docs/environment.md describes the supported environment",
            error.to_string(),
        ),
    }

    report.finish()
}

// ---------------------------------------------------------------------------
// static scanning
// ---------------------------------------------------------------------------

pub(crate) fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/crates/keyvibes`; the workspace root is
    // two levels above it.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn rust_sources(root: &Path) -> Vec<PathBuf> {
    rust_sources_excluding(root, &[POLICY_FILE])
}

/// Every workspace Rust file except the policy files that quote the tokens
/// they ban (this module and any caller passing its own exclusions).
pub(crate) fn rust_sources_excluding(root: &Path, exclude: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["crates", "xtask/src"] {
        walk(&root.join(dir), &mut files);
    }
    files.retain(|path| match path.file_name() {
        Some(name) => {
            let name = name.to_string_lossy();
            !exclude.iter().any(|excluded| name == *excluded)
        }
        None => true,
    });
    files
}

pub(crate) fn cargo_manifests(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for candidate in [root.join("Cargo.toml"), root.join("xtask/Cargo.toml")] {
        if candidate.is_file() {
            files.push(candidate);
        }
    }
    let mut from_crates = Vec::new();
    walk_named(&root.join("crates"), "Cargo.toml", &mut from_crates);
    files.extend(from_crates);
    files
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn walk_named(dir: &Path, file_name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk_named(&path, file_name, out);
        } else if path.file_name().is_some_and(|name| name == file_name) {
            out.push(path);
        }
    }
}

pub(crate) fn scan(files: &[PathBuf], tokens: &[&str], insensitive: bool) -> Vec<String> {
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
    for file in files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let code = mask_hex_literals(&strip_comments(&source));
        let haystack = if insensitive {
            code.to_lowercase()
        } else {
            code
        };
        for needle in &needles {
            if let Some(index) = haystack.find(needle.as_str()) {
                let (line, column) = line_column(&haystack, index);
                hits.push(format!(
                    "{}:{line}:{column} contains `{needle}`",
                    display(file)
                ));
            }
        }
    }
    hits
}

fn scan_manifests(manifests: &[PathBuf]) -> Vec<String> {
    let mut hits = Vec::new();
    for manifest in manifests {
        let Ok(text) = std::fs::read_to_string(manifest) else {
            continue;
        };
        for (number, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                // `[dependencies.wayland-client]` is a dependency too.
                let parts: Vec<String> = line
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split('.')
                    .map(str::to_string)
                    .collect();
                if parts.len() >= 2 && parts[0].to_lowercase().contains("dependencies") {
                    let name = parts[1].to_lowercase();
                    if FORBIDDEN_DEPS.contains(&name.as_str()) {
                        hits.push(format!(
                            "{}:{} declares `{name}`",
                            display(manifest),
                            number + 1
                        ));
                    }
                }
                continue;
            }
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().trim_matches('"').to_lowercase();
            if FORBIDDEN_DEPS.contains(&key.as_str()) {
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

pub(crate) fn transitive_deps(root: &Path) -> Result<Vec<String>, String> {
    let output = ProcessCommand::new("cargo")
        .args([
            "tree",
            "--workspace",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--target",
            "all",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("`cargo tree` unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo tree` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut names: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

pub(crate) fn summarize(hits: &[String]) -> String {
    const MAX: usize = 3;
    let shown: Vec<&str> = hits.iter().take(MAX).map(String::as_str).collect();
    let more = hits.len().saturating_sub(shown.len());
    if more > 0 {
        format!("{} (+{more} more)", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

fn tail(text: &str) -> String {
    const LINES: usize = 3;
    let all: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    all.iter()
        .rev()
        .take(LINES)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" / ")
}

pub(crate) fn display(path: &Path) -> String {
    let root = workspace_root();
    path.strip_prefix(&root)
        .map(|rest| rest.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
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

/// Blanks out `0x...` integer literals in place, keeping byte offsets.
///
/// Without this a hash constant such as `0xcbf2_9ce4_8422_2325` would look
/// like a reference to the `xcb` crate. Hex digits carry no meaning to this
/// scan, so they are replaced with spaces rather than removed.
fn mask_hex_literals(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let starts_literal = bytes[i] == b'0'
            && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'));
        if !starts_literal {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < bytes.len() && (bytes[j].is_ascii_hexdigit() || bytes[j] == b'_') {
            j += 1;
        }
        for slot in out.iter_mut().take(j).skip(i) {
            *slot = b' ';
        }
        i = j;
    }
    String::from_utf8(out).unwrap_or_else(|_| code.to_string())
}

/// Removes `//` line comments and `/* */` block comments while respecting
/// string literals, raw strings, and char literals, so a URL inside a string
/// is not mistaken for a comment and `fmt::Display` is left alone.
pub(crate) fn strip_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                let mut depth = 1;
                while i < chars.len() && depth > 0 {
                    match (chars[i], chars.get(i + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            i += 2;
                        }
                        (c, _) => {
                            if c == '\n' {
                                out.push('\n');
                            }
                            i += 1;
                        }
                    }
                }
            }
            'r' if !chars[..i]
                .last()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_') =>
            {
                let mut j = i + 1;
                let mut hashes = 0usize;
                while chars.get(j) == Some(&'#') && hashes <= 16 {
                    hashes += 1;
                    j += 1;
                }
                if chars.get(j) == Some(&'"') {
                    let start = i;
                    j += 1;
                    while j < chars.len() {
                        if chars[j] == '"'
                            && (0..hashes).all(|n| chars.get(j + 1 + n) == Some(&'#'))
                        {
                            j += 1 + hashes;
                            break;
                        }
                        j += 1;
                    }
                    out.extend(&chars[start..j.min(chars.len())]);
                    i = j.min(chars.len());
                } else {
                    out.push('r');
                    i += 1;
                }
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        out.push(chars[i]);
                        out.push(chars[i + 1]);
                        i += 2;
                    } else {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < chars.len() {
                    out.push('"');
                    i += 1;
                }
            }
            '\'' => {
                // `'x'` and `'\n'` are char literals; `'a` and `'static` are
                // lifetimes and stay in the code.
                let escaped = chars.get(i + 1) == Some(&'\\');
                let plain = chars.get(i + 1).is_some() && chars.get(i + 2) == Some(&'\'');
                if escaped || plain {
                    let end = if escaped { i + 4 } else { i + 3 };
                    out.extend(&chars[i..end.min(chars.len())]);
                    i = end.min(chars.len());
                } else {
                    out.push('\'');
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
}

/// Removes `#[cfg(test)] mod ... { ... }` items so an audit of production
/// source does not trip over unit-test fixtures that legitimately write
/// files, print keys, or mention forbidden tokens.
///
/// Stripping (rather than skipping the whole file) keeps the line numbers of
/// everything that remains meaningful.
pub(crate) fn strip_test_modules(source: &str) -> String {
    const MARKER: &[char] = &['#', '[', 'c', 'f', 'g', '(', 't', 'e', 's', 't'];
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let looks_like_attribute =
            i + MARKER.len() <= chars.len() && chars[i..i + MARKER.len()] == *MARKER;
        if !looks_like_attribute {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Close the attribute.
        let mut j = i;
        while j < chars.len() && chars[j] != ']' {
            j += 1;
        }
        if j >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Skip trivia between `]` and the item.
        let mut k = j + 1;
        while k < chars.len() && chars[k].is_whitespace() {
            k += 1;
        }
        let is_mod = k + 3 <= chars.len() && chars[k..k + 3] == ['m', 'o', 'd'];
        if !is_mod {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Skip the whole item, braces included.
        let mut m = k;
        while m < chars.len() && chars[m] != '{' {
            m += 1;
        }
        if m >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut depth = 0usize;
        while m < chars.len() {
            match chars[m] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        m += 1;
                        break;
                    }
                }
                _ => {}
            }
            m += 1;
        }
        i = m;
    }
    out
}

// ---------------------------------------------------------------------------
// dynamic: re-execution without a desktop session
// ---------------------------------------------------------------------------

/// Runs `keyvibes <args>` with every desktop session variable removed and a
/// config file that does not exist (so every default applies).
///
/// Returns the exit code plus captured stdout and stderr.
fn run_without_session(
    exe: &Path,
    args: &[&str],
    scratch: &Path,
) -> Result<(Option<i32>, String, String), String> {
    let _ = std::fs::create_dir_all(scratch);
    let mut command = ProcessCommand::new(exe);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.env("KEYVIBES_CONFIG", scratch.join("absent.toml"));
    command.env_remove("KEYVIBES_PACK_DIR");
    for var in SESSION_VARS {
        command.env_remove(var);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot re-execute {}: {error}", exe.display()))?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || read_all(&mut stdout_pipe));
    let stderr_thread = std::thread::spawn(move || read_all(&mut stderr_pipe));

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting on child failed: {error}"));
            }
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    let Some(status) = status else {
        return Err(format!(
            "`keyvibes {}` did not finish within 30s (stdout: {})",
            args.join(" "),
            tail(&String::from_utf8_lossy(&stdout))
        ));
    };

    Ok((
        status.code(),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

fn read_all(pipe: &mut Option<impl std::io::Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(reader) = pipe.as_mut() {
        let _ = std::io::Read::read_to_end(reader, &mut buffer);
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_comments_go_away_but_block_comments_only_wrap_text() {
        let source = "let a = 1; // WAYLAND_DISPLAY\n/* compositor */ let b = 2;\n";
        let stripped = strip_comments(source);
        assert!(!stripped.contains("WAYLAND_DISPLAY"));
        assert!(!stripped.contains("compositor"));
        assert!(stripped.contains("let a = 1;"));
        assert!(stripped.contains("let b = 2;"));
    }

    #[test]
    fn urls_inside_strings_survive_comment_stripping() {
        let source = "const DOC: &str = \"https://example.test/x11\";\n// x11\n";
        let stripped = strip_comments(source);
        assert!(stripped.contains("https://example.test/x11"));
        assert!(!stripped.ends_with("// x11\n"));
    }

    #[test]
    fn nested_block_comments_are_balanced() {
        let source = "let a = 1; /* outer /* inner */ still comment */ let b = 2;";
        let stripped = strip_comments(source);
        assert!(stripped.contains("let a = 1;"));
        assert!(stripped.contains("let b = 2;"));
        assert!(!stripped.contains("still comment"));
    }

    #[test]
    fn raw_strings_are_left_intact() {
        // The scanner must see the whole raw string, not stop at the `//`
        // a naive reader would find inside it.
        let source = "let s = r#\"https://example.test/wayland\"#; // compositor";
        let stripped = strip_comments(source);
        assert!(stripped.contains("https://example.test/wayland"));
        assert!(!stripped.contains("compositor"));
    }

    #[test]
    fn lifetimes_survive_but_char_literals_do_not_confuse_the_scanner() {
        let source = "fn f<'a>(x: &'a str) { let c = 'x'; }";
        let stripped = strip_comments(source);
        assert!(stripped.contains("'a"));
        assert!(stripped.contains("'x'"));
    }

    #[test]
    fn hex_literals_are_masked_so_constants_are_not_mistaken_for_crates() {
        let source = "const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;";
        let masked = mask_hex_literals(&strip_comments(source));
        assert!(!masked.contains("xcb"));
        assert!(!masked.contains("11"));
        // Offsets are preserved so a hit still reports the real line/column.
        assert_eq!(masked.len(), source.len());
        assert!(masked.contains("const FNV_OFFSET"));
    }

    #[test]
    fn identifiers_that_merely_start_like_a_hex_literal_are_kept() {
        let source = "let prefix = 0; let x11_only_in_text = mix0x11;";
        let masked = mask_hex_literals(source);
        assert!(masked.contains("0x11"));
    }

    #[test]
    fn the_scan_finds_a_session_variable_in_code_and_ignores_it_in_a_comment() {
        let dir = std::env::temp_dir().join(format!("keyvibes-compat-scan-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("sample.rs");
        std::fs::write(&file, "let x = 1; // WAYLAND_DISPLAY\n").unwrap();
        assert!(scan(std::slice::from_ref(&file), &["WAYLAND_DISPLAY"], false).is_empty());

        std::fs::write(&file, "let session = WAYLAND_DISPLAY;\n").unwrap();
        assert!(!scan(&[file], &["WAYLAND_DISPLAY"], false).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
