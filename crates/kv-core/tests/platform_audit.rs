//! `kv-core` must stay platform-neutral (Phase 21).
//!
//! The engine is written against a core of shared types, with every
//! platform-specific detail pushed into a leaf crate:
//!
//! ```text
//! kv-core                 PhysicalKey, PlayCommand, Settings, ...
//!    ├── kv-input-linux   evdev          (future: kv-input-windows)
//!    └── kv-audio-pipewire                (future: kv-audio-wasapi)
//! ```
//!
//! That split only holds while `kv-core` cannot see a platform. Two things
//! would break it, and both are checked here on every `cargo test -p
//! kv-core`:
//!
//! 1. a dependency on something that only exists on one platform - caught by
//!    reading `Cargo.toml`, because a dependency is how a type gets in, and
//! 2. a direct use of a platform API - caught by scanning the source with
//!    comments and string literals removed, so the doc comments that explain
//!    *why* the abstraction exists do not count against it.
//!
//! The first check is structural: `kv-core` depends on no other crate in the
//! workspace, so no `PhysicalKey`-shaped Linux type can arrive indirectly.

use std::fs;
use std::path::{Path, PathBuf};

/// The only dependencies `kv-core` may declare outside `dev-dependencies`.
const ALLOWED_DEPENDENCIES: &[&str] = &["thiserror"];
/// Optional dependencies, and the feature that enables each one.
const ALLOWED_OPTIONAL_DEPENDENCIES: &[(&str, &str)] = &[("serde", "serde")];

/// Tokens that mean a platform leaked into the core.
///
/// Checked against code only. Comments are allowed to say "evdev" or
/// "PipeWire" when they explain the boundary, which several do.
const FORBIDDEN_CODE_TOKENS: &[&str] = &[
    // Other platforms' APIs.
    "evdev",
    "pipewire",
    "wasapi",
    "winapi",
    "windows-sys",
    "uinput",
    "EVIOCGRAB",
    // Operating-system surfaces.
    "/dev/",
    "std::os::unix",
    "std::os::windows",
    "std::os::linux",
    "ioctl",
    "mmap",
    // Sibling crates. Reaching into one of these would drag a platform type
    // into the core even though the dependency itself is not declared.
    "kv_input_linux",
    "kv_audio_pipewire",
    "kv_input_windows",
    "kv_audio_wasapi",
];

/// Conditional-compilation gates that pick a platform.
const FORBIDDEN_CFG_TOKENS: &[&str] = &[
    "#[cfg(unix",
    "#[cfg(windows",
    "#[cfg(target_os",
    "#[cfg(target_family",
    "#[cfg(target_arch",
];

/// `Cargo.toml` sections that would smuggle a platform dependency in.
const FORBIDDEN_MANIFEST_SECTIONS: &[&str] = &[
    "target",
    "build-dependencies",
    "dependencies.evd",
    "dependencies.pipewire",
    "dependencies.libc",
];

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn main_rs() -> PathBuf {
    src_dir().join("lib.rs")
}

/// Every `.rs` file under `src/`, sorted so a failure names a stable list.
fn sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(&src_dir(), &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Build output, not crate source.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Removes line and nested block comments, preserving newlines so reported
/// line numbers still line up with the file.
fn strip_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1)) {
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                let mut depth = 1usize;
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
            _ => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Blanks out string literals, raw strings and byte strings.
///
/// A path like `"/dev/input"` is exactly the kind of thing this audit must
/// not flag when it appears in an error message, and exactly the kind of
/// thing it must flag if it ever appears as real logic.
fn blank_string_literals(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        // Optional `b` byte-string prefix, then an optional `r` with hashes.
        let mut cursor = i;
        if chars.get(cursor) == Some(&'b') {
            cursor += 1;
        }
        if chars.get(cursor) == Some(&'r') {
            cursor += 1;
            let hashes = (cursor..chars.len())
                .take_while(|&j| chars[j] == '#')
                .count();
            cursor += hashes;
            if chars.get(cursor) == Some(&'"') {
                cursor += 1;
                let closing = format!("\"{}", "#".repeat(hashes));
                let end = closing_index(&chars, cursor, &closing).unwrap_or(chars.len());
                blank(&mut out, end - i);
                i = end;
                continue;
            }
        }
        if chars.get(cursor) == Some(&'"') {
            cursor += 1;
            while cursor < chars.len() {
                match chars[cursor] {
                    '\\' => cursor += 2,
                    '"' => {
                        cursor += 1;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
            let end = cursor.min(chars.len());
            blank(&mut out, end - i);
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Replaces `count` characters with spaces, so column positions survive.
fn blank(out: &mut String, count: usize) {
    for _ in 0..count {
        out.push(' ');
    }
}

/// Index of `closing` at or after `from`, if present.
fn closing_index(chars: &[char], from: usize, closing: &str) -> Option<usize> {
    let closing: Vec<char> = closing.chars().collect();
    if closing.is_empty() || from > chars.len() {
        return None;
    }
    (from..=chars.len().saturating_sub(closing.len()))
        .find(|&start| chars[start..start + closing.len()] == closing)
}

/// Code only: comments and string literals removed.
fn code_only(source: &str) -> String {
    blank_string_literals(&strip_comments(source))
}

fn line_of(haystack: &str, needle: &str) -> usize {
    haystack
        .lines()
        .position(|line| line.contains(needle))
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// The manifest is the one place a dependency can enter, so it is read in
/// full rather than pattern-matched.
fn manifest_sections() -> toml::Table {
    let text = fs::read_to_string(manifest()).expect("kv-core/Cargo.toml is readable");
    text.parse::<toml::Table>()
        .expect("kv-core/Cargo.toml is valid TOML")
}

/// The workspace root manifest, used to resolve `workspace = true`.
fn workspace_dependencies() -> toml::Table {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("kv-core sits two levels below the workspace root")
        .join("Cargo.toml");
    let text = fs::read_to_string(root).expect("workspace Cargo.toml is readable");
    let table: toml::Table = text.parse().expect("workspace Cargo.toml is valid TOML");
    table
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn depends_on_no_other_crate_in_the_workspace() {
    let table = manifest_sections();
    let dependencies = match table.get("dependencies").and_then(toml::Value::as_table) {
        Some(dependencies) => dependencies,
        None => return,
    };
    let inherited = workspace_dependencies();

    for (name, value) in dependencies {
        let Some(entry) = value.as_table() else {
            continue;
        };
        // A direct path is the obvious way in.
        if entry.contains_key("path") {
            panic!(
                "kv-core depends on `{name}` by path, so every type it exports - \
                 and every platform type behind them - leaks into the core. Put the \
                 dependency in kv-input-linux / kv-audio-pipewire instead."
            );
        }
        // `workspace = true` inherits the *version*, not the crate's location;
        // resolve it to find out whether the thing being inherited is itself a
        // workspace crate.
        if entry.get("workspace") == Some(&toml::Value::Boolean(true)) {
            let resolved = inherited.get(name.as_str()).and_then(toml::Value::as_table);
            if resolved.is_some_and(|resolved| resolved.contains_key("path")) {
                panic!(
                    "kv-core inherits `{name}` from the workspace, and that entry is a \
                     workspace crate (it has a `path`), so its types - including any \
                     platform type - would leak into the core."
                );
            }
        }
    }
}

#[test]
fn declared_dependencies_are_platform_neutral() {
    let table = manifest_sections();
    let declared: Vec<String> = table
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .map(|entry| entry.keys().cloned().collect())
        .unwrap_or_default();

    for name in &declared {
        if ALLOWED_DEPENDENCIES.contains(&name.as_str()) {
            continue;
        }
        if ALLOWED_OPTIONAL_DEPENDENCIES
            .iter()
            .any(|(dependency, _)| dependency == name)
        {
            continue;
        }
        panic!(
            "kv-core declares `{name}`, which is not on the portable allow list \
             {ALLOWED_DEPENDENCIES:?} + optional {ALLOWED_OPTIONAL_DEPENDENCIES:?}. \
             A platform dependency belongs in kv-input-linux / kv-audio-pipewire \
             (or their future Windows counterparts), never in the core."
        );
    }
}

#[test]
fn optional_dependencies_are_gated_by_their_feature() {
    let table = manifest_sections();
    let features = table
        .get("features")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();

    for (dependency, feature) in ALLOWED_OPTIONAL_DEPENDENCIES {
        let Some(value) = table
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .and_then(|entry| entry.get(*dependency))
        else {
            continue;
        };
        assert!(
            value
                .as_table()
                .is_some_and(|entry| entry.get("optional") == Some(&toml::Value::Boolean(true))),
            "kv-core's `{dependency}` must stay optional, or every user of the core \
             pays for a dependency it does not need"
        );
        assert!(
            features.contains_key(*feature),
            "kv-core's `{dependency}` is optional but no `{feature}` feature enables it"
        );
    }
}

#[test]
fn manifest_has_no_platform_specific_dependency_section() {
    let text = fs::read_to_string(manifest()).expect("kv-core/Cargo.toml is readable");
    for section in FORBIDDEN_MANIFEST_SECTIONS {
        let needle = format!("[{section}");
        assert!(
            !text.contains(&needle),
            "kv-core/Cargo.toml contains `{needle}`, a platform-specific dependency \
             section. Platform crates belong in kv-input-linux / kv-audio-pipewire."
        );
    }
    // A target-specific table uses `[target.'cfg(unix)'.dependencies]`, which
    // the section list above does not cover word for word.
    assert!(
        !text.contains("[target."),
        "kv-core/Cargo.toml declares a `[target.*]` dependency table, which is \
         exactly how a platform-specific dependency hides from a plain list"
    );
}

#[test]
fn no_platform_api_appears_in_the_core_sources() {
    let mut violations = Vec::new();
    for path in sources() {
        let source = fs::read_to_string(&path).expect("source is readable");
        let code = code_only(&source);
        for token in FORBIDDEN_CODE_TOKENS {
            if let Some(line) = find_token(&code, token) {
                violations.push(format!(
                    "{}:{line}: `{token}`",
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&path)
                        .display()
                ));
            }
        }
        for token in FORBIDDEN_CFG_TOKENS {
            if let Some(line) = find_token(&code, token) {
                violations.push(format!(
                    "{}:{line}: `{token}` (conditional compilation must not pick a platform here)",
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&path)
                        .display()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "platform-specific code reached kv-core:\n  {}\n\nMove it into \
         kv-input-linux or kv-audio-pipewire: the core carries the shared \
         types (PhysicalKey, PlayCommand, Settings) and nothing that only \
         one operating system can provide.",
        violations.join("\n  ")
    );
}

/// Finds `token` in code, tolerating a leading `$crate` style path or a
/// trailing `::` so `use kv_input_linux::` is caught by the token
/// `kv_input_linux`.
fn find_token(code: &str, token: &str) -> Option<usize> {
    for (index, line) in code.lines().enumerate() {
        if line.contains(token) {
            return Some(index + 1);
        }
    }
    None
}

#[test]
fn core_declares_only_the_modules_it_is_supposed_to() {
    let source = fs::read_to_string(main_rs()).expect("lib.rs is readable");
    let code = code_only(&source);
    let modules: Vec<&str> = code
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("pub mod ")
                .and_then(|rest| rest.split(';').next())
                .map(|name| name.trim())
        })
        .collect();

    const EXPECTED: &[&str] = &[
        "geometry",
        "metrics",
        "physical_key",
        "play",
        "settings",
        "time",
        "variation",
        "wake",
    ];
    assert_eq!(
        modules, EXPECTED,
        "kv-core's public module list changed. Each entry is a platform-neutral \
         abstraction; a new one must be too, and must be covered by the rest \
         of this audit."
    );
}

#[test]
fn every_source_file_stays_under_src() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stray: Vec<String> = walk_from(root)
        .into_iter()
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "rs")
                && !path.starts_with(root.join("src"))
                && !path.starts_with(root.join("tests"))
        })
        .map(|path| path.display().to_string())
        .collect();
    assert!(
        stray.is_empty(),
        "kv-core has Rust files outside src/ and tests/: {stray:?}"
    );
}

fn walk_from(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(dir, &mut found);
    found
}

/// Line numbers are reported in failures; this keeps the helper honest.
#[test]
fn the_stripper_removes_what_it_claims_to() {
    let noisy = "// evdev\nlet a = \"/dev/input\";\n/* pipewire */\nlet b = 1;\n";
    let code = code_only(noisy);
    assert!(!code.contains("evdev"));
    assert!(!code.contains("/dev/input"));
    assert!(!code.contains("pipewire"));
    assert!(code.contains("let b = 1;"));

    let nested = "/* a /* b */ c */ let c = \"pipe\\\"wire\";\n";
    let code = code_only(nested);
    assert!(code.contains("let c ="));
    assert!(!code.contains("pipe"));

    // Line numbers survive the strip, so a violation names a usable line.
    assert_eq!(line_of(&code_only("// x\nlet c = 1;\n"), "let c"), 2);
}
