//! `keyvibes cli-test` - the command-line interface must stay complete.
//!
//! Help text drifts silently: a command gets renamed, its subcommand stops
//! matching the variant behind it, and the only sign is a user typing
//! something the program used to accept. This walks the interface the way a
//! reader of `--help` does - top level, then each advertised command, then
//! whatever those advertise in turn - and fails on the first command that is
//! missing from the help, refuses its own `--help`, or prints nothing.
//!
//! The walk is driven by the rendered help rather than by the enum, so it
//! cannot agree with a broken interface by construction.

use crate::accept::{Report, Status};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command as ProcessCommand;

/// Commands a user is expected to reach for.
///
/// These must be advertised at the top level and must answer their own
/// help. Everything else in the interface is allowed to change shape.
const REQUIRED_COMMANDS: &[&str] = &[
    "run",
    "doctor",
    "config",
    "input-test",
    "analyze",
    "process",
    "stress",
    "benchmark",
    "pack",
];

/// Commands that exist only to be invoked as `<name> --help`; asking them
/// for their own subcommands would loop forever.
const SKIP_SUBCOMMANDS: &[&str] = &["help"];

/// How deep the walk goes before it stops looking. The interface is two
/// levels (`pack build`, `config show`); a third would mean a command grew
/// a sub-subcommand, which this should notice rather than assume away.
const MAX_DEPTH: usize = 3;

pub fn run() -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate this binary")?;
    let mut report = Report::new("cli-test");

    let root = help_for(&exe, &[]);

    report.add(
        if root.exit == Some(0) {
            Status::Pass
        } else {
            Status::Fail
        },
        "`keyvibes --help` exits 0",
        match root.exit {
            Some(code) => format!("exit {code}, {} line(s)", root.text.lines().count()),
            None => format!(
                "could not be run: {}",
                root.error.as_deref().unwrap_or("unknown")
            ),
        },
    );

    let has_usage = root.text.contains("Usage:");
    let has_commands = root.text.contains("Commands:");
    report.add(
        if has_usage && has_commands {
            Status::Pass
        } else {
            Status::Fail
        },
        "the help shows usage and a command list",
        match (has_usage, has_commands) {
            (true, true) => "usage line and command list both present".to_string(),
            (false, true) => "the usage line is missing".to_string(),
            (true, false) => "the command list is missing".to_string(),
            (false, false) => "both the usage line and the command list are missing".to_string(),
        },
    );

    let advertised = advertised_commands(&root.text);
    report.add(
        if advertised.is_empty() {
            Status::Fail
        } else {
            Status::Pass
        },
        "commands are advertised",
        format!("{} command(s): {}", advertised.len(), advertised.join(" ")),
    );

    let missing: Vec<&str> = REQUIRED_COMMANDS
        .iter()
        .copied()
        .filter(|name| !advertised.iter().any(|found| found == name))
        .collect();
    report.add(
        if missing.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "every command a user needs is advertised",
        if missing.is_empty() {
            format!("{} required command(s) all listed", REQUIRED_COMMANDS.len())
        } else {
            format!("missing from --help: {}", missing.join(", "))
        },
    );

    // --- the walk -----------------------------------------------------------
    let mut visited: Vec<Vec<String>> = Vec::new();
    let mut checked = 0usize;
    let mut broken: Vec<String> = Vec::new();

    walk(&exe, &[], &mut visited, &mut checked, &mut broken, 0);

    report.add(
        if broken.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "every advertised command shows its own help",
        if broken.is_empty() {
            format!("{checked} command(s) answered --help with exit 0")
        } else {
            broken.join("; ")
        },
    );

    // --- the product commands in particular ---------------------------------
    let mut product_broken: Vec<String> = Vec::new();
    for name in REQUIRED_COMMANDS {
        let child = help_for(&exe, &[*name]);
        if child.exit != Some(0) {
            product_broken.push(format!("{name} (exit {:?})", child.exit));
            continue;
        }
        // `pack` and `config` are only useful because of what is under them.
        let needs_subcommands = *name == "pack" || *name == "config";
        if needs_subcommands && advertised_commands(&child.text).is_empty() {
            product_broken.push(format!("{name} (advertises no subcommand)"));
        }
    }
    report.add(
        if product_broken.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "each required command is usable, not merely listed",
        if product_broken.is_empty() {
            "pack and config both advertise their subcommands".to_string()
        } else {
            product_broken.join(", ")
        },
    );

    // --- subcommands of the product commands --------------------------------
    let expected_pack = ["build", "list", "inspect", "validate"];
    let pack_help = help_for(&exe, &["pack"]);
    let pack_subcommands = advertised_commands(&pack_help.text);
    let pack_missing: Vec<&str> = expected_pack
        .iter()
        .copied()
        .filter(|name| !pack_subcommands.iter().any(|found| found == name))
        .collect();
    report.add(
        if pack_help.exit == Some(0) && pack_missing.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "`pack` offers build, list, inspect and validate",
        if pack_missing.is_empty() {
            format!("all present: {}", pack_subcommands.join(" "))
        } else {
            format!("missing: {}", pack_missing.join(", "))
        },
    );

    let expected_config = ["show", "path", "init"];
    let config_help = help_for(&exe, &["config"]);
    let config_subcommands = advertised_commands(&config_help.text);
    let config_missing: Vec<&str> = expected_config
        .iter()
        .copied()
        .filter(|name| !config_subcommands.iter().any(|found| found == name))
        .collect();
    report.add(
        if config_help.exit == Some(0) && config_missing.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "`config` offers show, path and init",
        if config_missing.is_empty() {
            format!("all present: {}", config_subcommands.join(" "))
        } else {
            format!("missing: {}", config_missing.join(", "))
        },
    );

    report.finish()
}

/// Recursively asks every advertised command for its own help.
fn walk(
    exe: &Path,
    path: &[&str],
    visited: &mut Vec<Vec<String>>,
    checked: &mut usize,
    broken: &mut Vec<String>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let key: Vec<String> = path.iter().map(|part| (*part).to_string()).collect();
    if visited.contains(&key) {
        return;
    }
    visited.push(key);

    let help = help_for(exe, path);
    let advertised = advertised_commands(&help.text);

    if !path.is_empty() {
        *checked += 1;
        let label = path.join(" ");
        if help.exit != Some(0) {
            broken.push(format!("`{label} --help` exited {:?}", help.exit));
        } else if help.text.trim().is_empty() {
            broken.push(format!("`{label} --help` printed nothing"));
        } else if !help.text.contains("Usage:") {
            broken.push(format!("`{label} --help` has no usage line"));
        }
    }

    if depth == MAX_DEPTH {
        return;
    }
    for name in advertised {
        if SKIP_SUBCOMMANDS.contains(&name.as_str()) {
            continue;
        }
        let mut child: Vec<String> = path.to_vec().iter().map(|p| (*p).to_string()).collect();
        child.push(name);
        let child_refs: Vec<&str> = child.iter().map(String::as_str).collect();
        walk(exe, &child_refs, visited, checked, broken, depth + 1);
    }
}

/// Command names listed in a rendered help body.
///
/// Clap prints `Commands:` for the default group and a named heading for a
/// grouped one (verification commands live under `Verification:`), so every
/// section is treated as a command list except the ones that are obviously
/// not.
fn advertised_commands(help: &str) -> Vec<String> {
    const NON_COMMAND_HEADINGS: &[&str] = &["Options", "Arguments", "Examples"];

    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        // A section heading: unindented and ending in a colon.
        if !line.starts_with(' ') && line.ends_with(':') {
            let heading = line.trim_end_matches(':').trim();
            in_commands = !NON_COMMAND_HEADINGS.contains(&heading);
            continue;
        }
        if !in_commands {
            continue;
        }
        if !line.starts_with(' ') {
            in_commands = false;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }
        let name = trimmed.split_whitespace().next().unwrap_or_default();
        if !name.is_empty() && !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    names
}

struct HelpOutput {
    exit: Option<i32>,
    text: String,
    error: Option<String>,
}

/// Runs `keyvibes <path...> --help` and keeps what it printed.
fn help_for(exe: &Path, path: &[&str]) -> HelpOutput {
    let mut command = ProcessCommand::new(exe);
    command.args(path).arg("--help");
    match command.output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if text.trim().is_empty() {
                text = stderr;
            }
            HelpOutput {
                exit: output.status.code(),
                text,
                error: None,
            }
        }
        Err(error) => HelpOutput {
            exit: None,
            text: String::new(),
            error: Some(error.to_string()),
        },
    }
}
