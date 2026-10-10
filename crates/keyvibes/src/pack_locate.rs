//! Locating an installed sound pack by path or by convention.
//!
//! Keeps pack discovery out of the real-time and input paths: everything here
//! runs on the control plane, before any thread starts.

use anyhow::{bail, Context, Result};
use kv_pack::KvPack;
use std::path::{Path, PathBuf};

/// Directories searched for packs, most specific first.
///
/// Environment override, then user data, then system data, then the source
/// checkout's `assets/soundpacks` (so a developer can run KeyVibes without
/// installing anything).
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(dir) = std::env::var_os("KEYVIBES_PACK_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Some(home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(home.join("keyvibes").join("packs"));
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(
            home.join(".local")
                .join("share")
                .join("keyvibes")
                .join("packs"),
        );
    }

    dirs.push(PathBuf::from("/usr/local/share/keyvibes/packs"));
    dirs.push(PathBuf::from("/usr/share/keyvibes/packs"));
    dirs.push(PathBuf::from("assets").join("soundpacks"));

    dirs
}

/// Every `.kvpack` reachable from [`search_dirs`], in search order.
pub fn discover() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "kvpack"))
            .collect();
        here.sort();
        found.extend(here);
    }
    found
}

/// Resolves the pack to load.
///
/// `explicit` wins; otherwise `KEYVIBES_PACK` is used if set; otherwise the
/// first discovered pack is taken. Errors carry the directories that were
/// searched so the user can act on them.
pub fn resolve(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.exists() {
            bail!("pack not found: {}", path.display());
        }
        return Ok(path.to_path_buf());
    }

    if let Some(path) = std::env::var_os("KEYVIBES_PACK") {
        let path = PathBuf::from(path);
        if !path.exists() {
            bail!(
                "$KEYVIBES_PACK points at a missing file: {}",
                path.display()
            );
        }
        return Ok(path);
    }

    let found = discover();
    let first = found.first().context(no_pack_message())?;
    Ok(first.clone())
}

/// Resolves a user-supplied pack selector.
///
/// Accepts either:
///
/// * a path to a `.kvpack` file (anything that exists on disk), or
/// * a pack **name** as it appears in `keyvibes pack list`, matched
///   case-insensitively against the pack's own metadata, then against the
///   file stem.
///
/// This is what makes `keyvibes --pack "Holy Panda"` work without the user
/// having to know where packs are installed.
pub fn resolve_selector(selector: &str) -> Result<PathBuf> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        bail!("--pack needs a pack name or a path, not an empty string");
    }

    let as_path = Path::new(trimmed);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }

    let wanted = trimmed.to_lowercase();
    let mut stems = Vec::new();
    for path in discover() {
        let loaded = match KvPack::open(&path) {
            Ok(loaded) => loaded,
            // Skip packs that fail to open here; `pack validate` reports them.
            Err(_) => continue,
        };
        if loaded.stats().name.to_lowercase() == wanted {
            return Ok(path);
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            stems.push((stem.to_lowercase(), path));
        }
    }
    if let Some((_, path)) = stems.into_iter().find(|(stem, _)| *stem == wanted) {
        return Ok(path);
    }

    let available = available_names();
    if available.is_empty() {
        bail!(
            "pack not found: {trimmed}\n  no sound pack is installed, so there \
             is nothing to match it against\n  fix: build one -\n    \
             `keyvibes pack build <pack.toml> -o <pack.kvpack>`\n  or pass \
             --pack <path to a .kvpack>"
        );
    }
    bail!(
        "pack not found: {trimmed}\n  searched for a file at that path and \
         for a pack name among:\n{}\n  fix: pass --pack <one of those names>, \
         or set `pack = \"...\"` in the config",
        available.join("\n")
    )
}

/// Installed pack names, in search order, for error messages and `pack list`.
pub fn available_names() -> Vec<String> {
    discover()
        .into_iter()
        .filter_map(|path| KvPack::open(&path).ok())
        .map(|loaded| loaded.stats().name)
        .collect()
}

fn no_pack_message() -> String {
    let dirs: Vec<String> = search_dirs()
        .iter()
        .map(|dir| format!("  {}", dir.display()))
        .collect();
    let searched = dirs.join("\n");
    if discover().is_empty() {
        format!(
            "no sound pack is installed; searched:\n{searched}\n\
             fix: build one -\n  `keyvibes pack build <pack.toml> -o <pack.kvpack>`\n\
             or point $KEYVIBES_PACK at a .kvpack you already have"
        )
    } else {
        format!(
            "no .kvpack matched; searched:\n{searched}\n\
             installed packs: {}\n\
             fix: pass --pack <name-or-path>, or set `pack = \"...\"` in the config",
            available_names().join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_dirs_are_deterministic() {
        assert_eq!(search_dirs(), search_dirs());
        assert!(search_dirs().len() >= 4);
    }

    #[test]
    fn explicit_missing_pack_is_an_error() {
        let error = resolve(Some(Path::new("/nonexistent/nope.kvpack"))).unwrap_err();
        assert!(error.to_string().contains("pack not found"));
    }

    #[test]
    fn discovery_finds_the_bundled_pack_in_a_checkout() {
        let packs = discover();
        for path in packs {
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some("kvpack"),
                "only .kvpack files are discovered: {}",
                path.display()
            );
        }
    }
}
