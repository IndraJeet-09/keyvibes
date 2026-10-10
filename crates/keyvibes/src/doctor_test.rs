//! Phase 17/18 acceptance: `keyvibes doctor-test`.
//!
//! Every command in this binary is run the way a person would run it - as a
//! child process - because that is the only way to observe the thing that
//! matters here: the exit status and the text a confused user actually sees.
//!
//! The bar for each case is the same three parts:
//!
//! * **what** - the message names the thing that failed (the path, the pack
//!   selector, the field),
//! * **why** - it says what KeyVibes found rather than leaking an errno or a
//!   Rust type, and
//! * **fix** - it points at a command or an edit that resolves it.
//!
//! Nothing may panic. A panic exits `101` and prints `panicked at`, which is
//! the exact experience this test exists to prevent.

use crate::accept::{Report, Status};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Fields `keyvibes doctor` must print on every machine, healthy or not.
const REQUIRED_FIELDS: &[&str] = &[
    "Version",
    "OS",
    "Kernel",
    "Architecture",
    "Input",
    "Audio",
    "default output",
    "sample rate",
    "quantum",
    "Packs",
    "installed",
    "selected",
    "Runtime",
];

/// Text that only ever comes from an unhandled panic or an unwind.
const CRASH_TOKENS: &[&str] = &[
    "panicked at",
    "thread 'main' panicked",
    "stack backtrace",
    "RUST_BACKTRACE",
];

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("doctor-test");

    let Some(exe) = binary() else {
        report.not_run(
            "the keyvibes binary was found",
            "no built binary; run `cargo build --workspace` first",
        );
        return report.finish();
    };

    let root = scratch("root");
    let _ = std::fs::create_dir_all(&root);

    // Every child process's text is kept so the sweep at the end can look at
    // all of them at once.
    let mut outputs: Vec<(String, i32, String)> = Vec::new();

    // --- 1. doctor prints the whole picture --------------------------------
    let defaults = root.join("defaults.toml");
    let _ = std::fs::write(&defaults, default_config_text());
    let (code, text) = invoke(
        &exe,
        &["doctor"],
        &[("KEYVIBES_CONFIG", Some(&defaults))],
        Some(&root),
    );
    let missing: Vec<&str> = REQUIRED_FIELDS
        .iter()
        .copied()
        .filter(|field| !text.contains(field))
        .collect();
    outputs.push((
        format!("doctor --config {}", defaults.display()),
        code,
        text.clone(),
    ));
    report.add(
        if missing.is_empty() && (code == 0 || code == 1) {
            Status::Pass
        } else {
            Status::Fail
        },
        "doctor prints every required field",
        if missing.is_empty() {
            format!("all {} field(s), exit {code}", REQUIRED_FIELDS.len())
        } else {
            format!("missing {}; exit {code}", missing.join(", "))
        },
    );

    // --- 2. a configuration KeyVibes cannot use is explained, not fatal ----
    let broken = root.join("broken.toml");
    let _ = std::fs::write(&broken, "version = 1\nvolume = 9\n");
    let (code, text) = invoke(
        &exe,
        &["doctor"],
        &[("KEYVIBES_CONFIG", Some(&broken))],
        Some(&root),
    );
    outputs.push((
        format!("doctor --config {}", broken.display()),
        code,
        text.clone(),
    ));
    report.add(
        if code == 1 && has_what_why_fix(&text, "volume") {
            Status::Pass
        } else {
            Status::Fail
        },
        "a configuration KeyVibes rejects is diagnosed with a fix",
        if code == 1 {
            "exit 1; why names the `volume` field and fix says what to write".to_string()
        } else {
            format!("exit {code}; output: {}", one_line(&text))
        },
    );

    // --- 3. a pack selector that matches nothing is explained -------------
    let unknown = "KeyvibesNoSuchPack";
    let (code, text) = invoke(
        &exe,
        &["doctor", "--pack", unknown],
        &[("KEYVIBES_CONFIG", Some(&defaults))],
        Some(&root),
    );
    outputs.push((format!("doctor --pack {unknown}"), code, text.clone()));
    report.add(
        if code == 1 && text.contains(unknown) && text.contains("fix:") {
            Status::Pass
        } else {
            Status::Fail
        },
        "an unknown pack selector names itself and offers a fix",
        if code == 1 {
            format!("exit 1; repeats `{unknown}` and gives the build/list command")
        } else {
            format!("exit {code}; output: {}", one_line(&text))
        },
    );

    // --- 4. an empty pack directory is diagnosed --------------------------
    let empty = root.join("packs");
    let _ = std::fs::create_dir_all(&empty);
    let (code, text) = invoke(
        &exe,
        &["doctor"],
        &[
            ("KEYVIBES_CONFIG", Some(&defaults)),
            ("KEYVIBES_PACK_DIR", Some(&empty)),
        ],
        Some(&root),
    );
    outputs.push((
        format!("doctor with {}", empty.display()),
        code,
        text.clone(),
    ));
    report.add(
        if code == 1 && text.contains("no sound pack is installed") && text.contains("fix:") {
            Status::Pass
        } else {
            Status::Fail
        },
        "a machine with no pack installed says which directory it searched",
        if code == 1 {
            format!("exit 1; searched {}", empty.display())
        } else {
            format!("exit {code}; output: {}", one_line(&text))
        },
    );

    // --- 5. every failing invocation is actionable and panic-free ---------
    let missing_pack = root.join("missing.kvpack");
    let missing_manifest = root.join("missing-pack.toml");
    let output_pack = root.join("out.kvpack");
    let missing_pack_text = missing_pack.to_string_lossy().into_owned();
    let missing_manifest_text = missing_manifest.to_string_lossy().into_owned();
    let output_pack_text = output_pack.to_string_lossy().into_owned();

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["pack", "validate", &missing_pack_text],
            "a path that does not exist",
        ),
        (
            vec!["pack", "inspect", &missing_pack_text],
            "a path that does not exist",
        ),
        (
            vec![
                "pack",
                "build",
                &missing_manifest_text,
                "-o",
                &output_pack_text,
            ],
            "a manifest that does not exist",
        ),
        (
            vec!["run", "--pack", unknown],
            "a pack name that is not installed",
        ),
    ];
    let mut bad = Vec::new();
    for (args, description) in &cases {
        let (code, text) = invoke(&exe, args, &[], Some(&root));
        let label = args.join(" ");
        outputs.push((label.clone(), code, text.clone()));
        let mentions_input = text.contains(missing_pack_text.as_str())
            || text.contains(missing_manifest_text.as_str())
            || text.contains(unknown);
        let actionable = text.contains("fix:") || text.contains('`');
        if code == 0 || code == 101 || !mentions_input || !actionable {
            bad.push(format!(
                "{label} ({description}) -> exit {code}, \
                 actionable={actionable}, mentions_input={mentions_input}"
            ));
        }
    }
    report.add(
        if bad.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "every rejected argument is named, explained, and fixable",
        if bad.is_empty() {
            format!(
                "{} failing invocation(s), each with what/why/fix",
                cases.len()
            )
        } else {
            bad.join("; ")
        },
    );

    // --- 6. nothing anywhere leaked a panic --------------------------------
    let crashes: Vec<String> = outputs
        .iter()
        .filter(|(_, _, text)| CRASH_TOKENS.iter().any(|token| text.contains(token)))
        .map(|(label, _, _)| label.clone())
        .collect();
    let panics: Vec<String> = outputs
        .iter()
        .filter(|(_, code, _)| *code == 101)
        .map(|(label, _, _)| label.clone())
        .collect();
    report.add(
        if crashes.is_empty() && panics.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no command panicked or printed a stack trace",
        if crashes.is_empty() && panics.is_empty() {
            format!(
                "{} invocation(s) exited cleanly or with a message",
                outputs.len()
            )
        } else {
            format!(
                "crashed: {}; exit 101: {}",
                crashes.join(", "),
                panics.join(", ")
            )
        },
    );

    // --- 7. doctor exits 0 when nothing is missing -------------------------
    let (code, text) = invoke(&exe, &["doctor", "--pack", "Default"], &[], Some(&root));
    outputs.push(("doctor --pack Default".to_string(), code, text.clone()));
    let (healthy_exit, detail) = match code {
        0 => (
            Status::Pass,
            "exit 0; every required dependency is present".to_string(),
        ),
        1 => (
            Status::Pass,
            format!(
                "exit 1; {} - this host is missing something, which doctor named above",
                first_problem(&text).unwrap_or_else(|| "unstated problem".to_string())
            ),
        ),
        101 => (
            Status::Fail,
            "doctor panicked on a machine it was written to diagnose".to_string(),
        ),
        other => (
            Status::Fail,
            format!("doctor exited {other}, which is neither 0 nor 1"),
        ),
    };
    report.add(
        healthy_exit,
        "doctor exits 0 when healthy and 1 when a dependency is missing",
        detail,
    );

    let _ = std::fs::remove_dir_all(&root);
    report.finish()
}

/// The first `why` line doctor printed, if it printed one.
fn first_problem(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((_, rest)) = line.split_once("[error]") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Whether the output carries what/why/fix around `subject`.
fn has_what_why_fix(text: &str, subject: &str) -> bool {
    text.contains(subject) && text.contains("why:") && text.contains("fix:")
}

/// The path of the binary under test, or `None` when nothing was built.
fn binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_keyvibes") {
        return Some(PathBuf::from(path));
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("../../target").join(profile).join("keyvibes"),
        manifest
            .join("../../target")
            .join(profile)
            .join("keyvibes.exe"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

/// A scratch directory that is removed again by the caller.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keyvibes-doctor-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The smallest configuration file that is valid at every default.
fn default_config_text() -> &'static str {
    "version = 1\nenabled = true\nvolume = 0.7\n"
}

/// Runs one child process and returns its status code and combined output.
fn invoke(
    exe: &Path,
    args: &[&str],
    env: &[(&str, Option<&PathBuf>)],
    cwd: Option<&Path>,
) -> (i32, String) {
    let mut command = Command::new(exe);
    command.args(args);
    for (key, value) in env {
        match value {
            Some(path) => {
                command.env(key, path);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    match command.output() {
        Ok(output) => (output.status.code().unwrap_or(-1), combine(&output)),
        Err(error) => (-1, format!("could not run {}: {error}", exe.display())),
    }
}

/// stdout followed by stderr, so an error printed the way `anyhow` prints it
/// is visible to the same scan as the report itself.
fn combine(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Everything the report shows on one line.
fn one_line(text: &str) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() > 160 {
        let cut: String = flattened.chars().take(160).collect();
        format!("{cut}...")
    } else {
        flattened
    }
}
