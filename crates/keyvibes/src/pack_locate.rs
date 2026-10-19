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
    discover_in(&search_dirs())
}

/// Whether `path` is a test fixture rather than a shippable pack.
///
/// Fixtures live under a `tests/fixtures` directory - the layout this
/// repository uses for `crates/kv-pack/tests/fixtures` - or are named
/// `golden*`. Production discovery skips them, so a checkout that happens to
/// have one lying around can never become the default pack (or show up in
/// `pack list` looking installed). An explicit `--pack <path>` still opens
/// whatever the user points at: the guard is about *selection*, not access.
pub fn is_test_fixture(path: &Path) -> bool {
    let fixtures = path
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case("fixtures"));
    let golden = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.to_lowercase().starts_with("golden"));
    fixtures || golden
}

/// [`discover`] over an explicit list of directories.
///
/// Split out so the fixture filter can be tested without touching
/// `$KEYVIBES_PACK_DIR` (which would race with every other test in the
/// binary).
pub fn discover_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut here: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "kvpack"))
            .filter(|path| !is_test_fixture(path))
            .collect();
        here.sort();
        found.extend(here);
    }
    found
}

/// [`discover`], with one entry per pack name.
///
/// The same pack can be reachable from more than one search directory - a
/// checkout's `assets/soundpacks` and a `make install` into `$HOME/.local`
/// both being on the list is the common case. Selection already takes the
/// first name match, so a second copy is not a second choice; listing it
/// would make `--pack` look like it had two answers.
///
/// Unreadable packs are kept under their file stem, so they still appear in
/// `pack list` with the error rather than vanishing.
pub fn discover_unique() -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for path in discover() {
        let key = KvPack::open(&path)
            .map(|loaded| loaded.stats().name.to_lowercase())
            .unwrap_or_else(|_| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            });
        if seen.insert(key) {
            unique.push(path);
        }
    }
    unique
}

/// Resolves the pack to load.
///
/// `explicit` wins; otherwise `KEYVIBES_PACK` is used if set; otherwise the
/// first discovered pack is taken. Errors carry the directories that were
/// searched so the user can act on them.
pub fn resolve(explicit: Option<&Path>) -> Result<PathBuf> {
    resolve_from(
        explicit,
        std::env::var_os("KEYVIBES_PACK").map(PathBuf::from),
        &discover(),
    )
}

/// [`resolve`] with its three inputs handed in, so the default-selection rule
/// can be tested without touching the environment.
///
/// `env_pack` is `$KEYVIBES_PACK`; `discovered` is whatever the search
/// directories produced (test fixtures already filtered out). Selection is
/// "explicit path, then `$KEYVIBES_PACK`, then the first discovered pack" -
/// and *first discovered* can only be a real installed pack, never a fixture.
pub fn resolve_from(
    explicit: Option<&Path>,
    env_pack: Option<PathBuf>,
    discovered: &[PathBuf],
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.exists() {
            bail!("pack not found: {}", path.display());
        }
        return Ok(path.to_path_buf());
    }

    if let Some(path) = env_pack {
        if !path.exists() {
            bail!(
                "$KEYVIBES_PACK points at a missing file: {}",
                path.display()
            );
        }
        return Ok(path);
    }

    let first = discovered.first().context(no_pack_message())?;
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
/// This is what makes `keyvibes --pack "Holy Panda (kbsim)"` work without the user
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
    discover_unique()
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

    #[test]
    fn unique_discovery_lists_each_name_once() {
        let unique = discover_unique();
        let mut names: Vec<String> = unique
            .iter()
            .filter_map(|path| KvPack::open(path).ok())
            .map(|loaded| loaded.stats().name.to_lowercase())
            .collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "the same pack was listed twice: {names:?}"
        );
    }

    /// Scratch directory holding whatever files the caller names in `files`.
    fn scratch(name: &str, files: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("keyvibes-locate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            let path = dir.join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, b"KVPACK\x00\x00 not a real pack").unwrap();
        }
        dir
    }

    #[test]
    fn fixture_paths_are_recognised() {
        assert!(is_test_fixture(Path::new(
            "/repo/crates/kv-pack/tests/fixtures/golden.kvpack"
        )));
        assert!(is_test_fixture(Path::new("/tmp/golden.kvpack")));
        assert!(is_test_fixture(Path::new(
            "/home/me/.local/share/keyvibes/packs/fixtures/old.kvpack"
        )));
        assert!(!is_test_fixture(Path::new(
            "/home/me/.local/share/keyvibes/packs/Alpaca.kvpack"
        )));
        assert!(!is_test_fixture(Path::new(
            "/repo/assets/soundpacks/Topre.kvpack"
        )));
    }

    #[test]
    fn discovery_lists_installed_packs_and_skips_fixtures() {
        let dir = scratch(
            "discover",
            &["Alpaca.kvpack", "fixtures/golden.kvpack", "Golden.kvpack"],
        );
        let found = discover_in(std::slice::from_ref(&dir));
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["Alpaca.kvpack".to_string()], "found: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_selection_refuses_a_directory_of_fixtures() {
        let dir = scratch(
            "fixtures-only",
            &["fixtures/golden.kvpack", "golden.kvpack"],
        );
        let discovered = discover_in(std::slice::from_ref(&dir));

        let error = resolve_from(None, None, &discovered)
            .expect_err("a directory of test fixtures must not become the default pack");
        let text = error.to_string();
        // The wording depends on what else is installed on this machine; the
        // property does not - an empty discovery is an error, never a pick.
        assert!(
            text.contains("no sound pack is installed") || text.contains("no .kvpack matched"),
            "expected the 'nothing to choose' diagnosis, got: {text}"
        );

        // An explicit path is the user's own decision and still works.
        let explicit = dir.join("golden.kvpack");
        assert!(resolve_from(Some(&explicit), None, &discovered).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_selection_takes_the_first_installed_pack() {
        let dir = scratch("installed", &["Alpha.kvpack", "Beta.kvpack"]);
        let discovered = discover_in(std::slice::from_ref(&dir));
        let selected = resolve_from(None, None, &discovered).expect("two packs are installed");
        assert_eq!(
            selected.file_name().unwrap().to_string_lossy(),
            "Alpha.kvpack",
            "search order, not alphabetical luck"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_discovered_in_this_checkout_is_a_fixture() {
        for path in discover() {
            assert!(
                !is_test_fixture(&path),
                "production discovery returned a test fixture: {}",
                path.display()
            );
        }
    }

    #[test]
    fn unique_discovery_keeps_every_discovered_file_in_order() {
        let unique = discover_unique();
        assert!(unique.len() <= discover().len());
        let mut subset = discover();
        subset.retain(|path| unique.contains(path));
        assert_eq!(subset, unique, "dedup must not reorder or invent paths");
    }
}
