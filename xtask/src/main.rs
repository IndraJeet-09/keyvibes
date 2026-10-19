//! KeyVibes build and packaging tool.
//!
//! `cargo xtask dist <format>` stages a release through `make install` and
//! turns it into the artifacts the website links to. Every format is built
//! from the same staged tree, so a package can never disagree with what
//! `make install` puts on disk.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Parser)]
#[command(name = "xtask", about = "KeyVibes build tool")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Stage a release and produce distributable artifacts
    Dist(DistArgs),
    /// Build every sound pack found under assets/soundpacks into .kvpack
    Packs,
    /// Write site/packs.json and the pack preview audio it points at
    Site(SiteArgs),
}

#[derive(Args)]
struct DistArgs {
    /// Which artifacts to produce
    #[arg(value_enum, default_value_t = Format::All)]
    format: Format,

    /// Where artifacts are written
    #[arg(long, default_value = "dist/out")]
    out: PathBuf,

    /// Skip `cargo build --release` and `make install`
    #[arg(long)]
    no_build: bool,
}

#[derive(Args)]
struct SiteArgs {
    /// Where the site lives (a relative path resolves against the workspace)
    #[arg(long, default_value = "site")]
    out: PathBuf,

    /// Rewrite packs.json only, reusing the preview audio already on disk
    #[arg(long)]
    json_only: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Compressed tree that runs from anywhere
    Portable,
    /// Debian package
    Deb,
    /// RPM package (writes the spec; builds when rpmbuild is present)
    Rpm,
    /// Arch PKGBUILD
    Pkgbuild,
    /// One-line installer script
    Installer,
    /// Self-contained AppImage (engine + bundled libpipewire + packs)
    Appimage,
    /// Everything above, plus checksums and the site's releases.json
    All,
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::Dist(args) => dist(&args),
        Task::Packs => {
            let built = build_packs()?;
            for path in &built {
                println!("pack  {}", path.display());
            }
            Ok(())
        }
        Task::Site(args) => site(&args),
    }
}

// ------------------------------------------------------------------ helpers

fn workspace_root() -> Result<PathBuf> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    Ok(Path::new(manifest)
        .parent()
        .context("xtask is not inside the workspace")?
        .to_path_buf())
}

fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<Output> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("could not run `{program}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("`{program} {}` failed:\n{stdout}{stderr}", args.join(" "));
    }
    Ok(output)
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Every pack manifest under `assets/soundpacks`, paired with the `.kvpack`
/// it should produce (named after the pack, which is what `pack list` and
/// the UI show as the file identity).
///
/// A manifest is any `*.toml` directly inside a pack directory, which is
/// how `upstream-src/<id>/pack.toml` is found through its parent, and any
/// `*.toml` in a directory of pack directories.
fn pack_manifests(root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut found = Vec::new();
    let search = root.join("assets/soundpacks");
    let Ok(entries) = fs::read_dir(&search) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let direct = toml_manifests_in(&dir);
        if !direct.is_empty() {
            for manifest in direct {
                let name = pack_name(&manifest, &dir);
                found.push((manifest, search.join(format!("{name}.kvpack"))));
            }
            continue;
        }
        // A directory of pack directories, e.g. assets/soundpacks/upstream-src.
        let Ok(inner) = fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = inner.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for child in children {
            if !child.is_dir() {
                continue;
            }
            for manifest in toml_manifests_in(&child) {
                found.push((
                    manifest.clone(),
                    search.join(format!("{}.kvpack", pack_name(&manifest, &child))),
                ));
            }
        }
    }
    found
}

/// `*.toml` files directly inside `dir`, sorted, so a directory holding
/// several pack manifests contributes all of them in a stable order.
fn toml_manifests_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut manifests: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    manifests.sort();
    manifests
}

/// The pack's own `name` when the manifest states one, else the directory.
/// Pack files are named after the pack, not after how the source tree is
/// laid out, so `keyvibes pack list` shows the same string everywhere.
fn pack_name(manifest: &Path, fallback: &Path) -> String {
    let Ok(text) = fs::read_to_string(manifest) else {
        return fallback
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    };
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("name") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim().trim_matches(['\'', '"']);
        if !value.is_empty() {
            return value.to_string();
        }
    }
    fallback
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn build_packs() -> Result<Vec<PathBuf>> {
    let root = workspace_root()?;
    let engine = root.join("target/release/keyvibes");
    let mut built = Vec::new();
    for (manifest, output) in pack_manifests(&root) {
        run(
            engine.to_str().context("engine path is not utf-8")?,
            &[
                "pack",
                "build",
                manifest.to_str().context("manifest path is not utf-8")?,
                "-o",
                output.to_str().context("output path is not utf-8")?,
            ],
            &root,
        )?;
        built.push(output);
    }
    Ok(built)
}

// -------------------------------------------------------------------- site

/// Keys whose press clip the site exports for every pack.
///
/// The whole physical board, in layout order. The site's hero keyboard shows
/// a full TKL, and a board with a silent function row is not a board you can
/// demo: a pack that carries every key gets a key that sounds on every key.
///
/// A pack may lack any of them - coverage is reported per pack - and a
/// missing key simply gets no slot. The site stays silent for it rather than
/// substituting another key's clip.
const PREVIEW_PRESS_KEYS: [kv_core::PhysicalKey; 104] = [
    // Function row
    kv_core::PhysicalKey::Escape,
    kv_core::PhysicalKey::F1,
    kv_core::PhysicalKey::F2,
    kv_core::PhysicalKey::F3,
    kv_core::PhysicalKey::F4,
    kv_core::PhysicalKey::F5,
    kv_core::PhysicalKey::F6,
    kv_core::PhysicalKey::F7,
    kv_core::PhysicalKey::F8,
    kv_core::PhysicalKey::F9,
    kv_core::PhysicalKey::F10,
    kv_core::PhysicalKey::F11,
    kv_core::PhysicalKey::F12,
    // Number row
    kv_core::PhysicalKey::Grave,
    kv_core::PhysicalKey::Digit1,
    kv_core::PhysicalKey::Digit2,
    kv_core::PhysicalKey::Digit3,
    kv_core::PhysicalKey::Digit4,
    kv_core::PhysicalKey::Digit5,
    kv_core::PhysicalKey::Digit6,
    kv_core::PhysicalKey::Digit7,
    kv_core::PhysicalKey::Digit8,
    kv_core::PhysicalKey::Digit9,
    kv_core::PhysicalKey::Digit0,
    kv_core::PhysicalKey::Minus,
    kv_core::PhysicalKey::Equal,
    kv_core::PhysicalKey::Backspace,
    // Top row
    kv_core::PhysicalKey::Tab,
    kv_core::PhysicalKey::Q,
    kv_core::PhysicalKey::W,
    kv_core::PhysicalKey::E,
    kv_core::PhysicalKey::R,
    kv_core::PhysicalKey::T,
    kv_core::PhysicalKey::Y,
    kv_core::PhysicalKey::U,
    kv_core::PhysicalKey::I,
    kv_core::PhysicalKey::O,
    kv_core::PhysicalKey::P,
    kv_core::PhysicalKey::LeftBracket,
    kv_core::PhysicalKey::RightBracket,
    kv_core::PhysicalKey::Backslash,
    // Home row
    kv_core::PhysicalKey::CapsLock,
    kv_core::PhysicalKey::A,
    kv_core::PhysicalKey::S,
    kv_core::PhysicalKey::D,
    kv_core::PhysicalKey::F,
    kv_core::PhysicalKey::G,
    kv_core::PhysicalKey::H,
    kv_core::PhysicalKey::J,
    kv_core::PhysicalKey::K,
    kv_core::PhysicalKey::L,
    kv_core::PhysicalKey::Semicolon,
    kv_core::PhysicalKey::Apostrophe,
    kv_core::PhysicalKey::Enter,
    // Bottom row
    kv_core::PhysicalKey::LeftShift,
    kv_core::PhysicalKey::Z,
    kv_core::PhysicalKey::X,
    kv_core::PhysicalKey::C,
    kv_core::PhysicalKey::V,
    kv_core::PhysicalKey::B,
    kv_core::PhysicalKey::N,
    kv_core::PhysicalKey::M,
    kv_core::PhysicalKey::Comma,
    kv_core::PhysicalKey::Period,
    kv_core::PhysicalKey::Slash,
    kv_core::PhysicalKey::RightShift,
    // Modifiers
    kv_core::PhysicalKey::LeftCtrl,
    kv_core::PhysicalKey::LeftSuper,
    kv_core::PhysicalKey::LeftAlt,
    kv_core::PhysicalKey::Space,
    kv_core::PhysicalKey::RightAlt,
    kv_core::PhysicalKey::RightSuper,
    kv_core::PhysicalKey::Menu,
    kv_core::PhysicalKey::RightCtrl,
    // Navigation cluster
    kv_core::PhysicalKey::Insert,
    kv_core::PhysicalKey::Delete,
    kv_core::PhysicalKey::Home,
    kv_core::PhysicalKey::End,
    kv_core::PhysicalKey::PageUp,
    kv_core::PhysicalKey::PageDown,
    // Arrow keys
    kv_core::PhysicalKey::ArrowUp,
    kv_core::PhysicalKey::ArrowDown,
    kv_core::PhysicalKey::ArrowLeft,
    kv_core::PhysicalKey::ArrowRight,
    // Numpad
    kv_core::PhysicalKey::NumLock,
    kv_core::PhysicalKey::NumpadDivide,
    kv_core::PhysicalKey::NumpadMultiply,
    kv_core::PhysicalKey::NumpadSubtract,
    kv_core::PhysicalKey::NumpadAdd,
    kv_core::PhysicalKey::NumpadEnter,
    kv_core::PhysicalKey::NumpadDecimal,
    kv_core::PhysicalKey::Numpad0,
    kv_core::PhysicalKey::Numpad1,
    kv_core::PhysicalKey::Numpad2,
    kv_core::PhysicalKey::Numpad3,
    kv_core::PhysicalKey::Numpad4,
    kv_core::PhysicalKey::Numpad5,
    kv_core::PhysicalKey::Numpad6,
    kv_core::PhysicalKey::Numpad7,
    kv_core::PhysicalKey::Numpad8,
    kv_core::PhysicalKey::Numpad9,
    // Additional keys
    kv_core::PhysicalKey::PrintScreen,
    kv_core::PhysicalKey::ScrollLock,
    kv_core::PhysicalKey::Pause,
];

/// Keys whose key-up clip is exported beside the press clips.
///
/// A key-up is what separates one switch from another on the way back, so
/// the site's burst layers them in. The set is smaller than the press set
/// because a release is far rarer than a press in a burst of typing.
const PREVIEW_RELEASE_KEYS: [kv_core::PhysicalKey; 28] = [
    kv_core::PhysicalKey::A,
    kv_core::PhysicalKey::B,
    kv_core::PhysicalKey::C,
    kv_core::PhysicalKey::D,
    kv_core::PhysicalKey::E,
    kv_core::PhysicalKey::F,
    kv_core::PhysicalKey::G,
    kv_core::PhysicalKey::H,
    kv_core::PhysicalKey::I,
    kv_core::PhysicalKey::J,
    kv_core::PhysicalKey::K,
    kv_core::PhysicalKey::L,
    kv_core::PhysicalKey::M,
    kv_core::PhysicalKey::N,
    kv_core::PhysicalKey::O,
    kv_core::PhysicalKey::P,
    kv_core::PhysicalKey::Q,
    kv_core::PhysicalKey::R,
    kv_core::PhysicalKey::S,
    kv_core::PhysicalKey::T,
    kv_core::PhysicalKey::U,
    kv_core::PhysicalKey::V,
    kv_core::PhysicalKey::W,
    kv_core::PhysicalKey::X,
    kv_core::PhysicalKey::Y,
    kv_core::PhysicalKey::Z,
    kv_core::PhysicalKey::Space,
    kv_core::PhysicalKey::Enter,
];

/// A slot longer than this is cut at the boundary. Press clips are far
/// shorter; the cap only keeps a pathological pack from shipping a strip a
/// browser would stall on.
const MAX_SLOT_SECONDS: f64 = 1.0;

/// One playable slice of a pack's preview strip.
#[derive(Debug, PartialEq)]
struct Slot {
    /// `PhysicalKey` debug name (`"Q"`, `"Escape"`), used as the keycap
    /// label and as the identifier the site looks slots up by.
    key: String,
    /// `"press"` or `"release"`.
    kind: &'static str,
    /// Frames (per channel) from the start of the strip.
    start: u64,
    /// Frames (per channel) this slot lasts.
    frames: u64,
}

impl Slot {
    fn start_seconds(&self, sample_rate: u32) -> f64 {
        seconds(self.start, sample_rate)
    }

    fn duration_seconds(&self, sample_rate: u32) -> f64 {
        seconds(self.frames, sample_rate)
    }
}

fn seconds(frames: u64, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    frames as f64 / f64::from(sample_rate)
}

/// Generates `packs.json` and, unless `--json-only`, one preview strip per
/// pack under `assets/audio/`.
///
/// Everything in the catalog is read from the pack files themselves: name,
/// category, coverage, release support, provenance. Nothing is typed in by
/// hand, so the site cannot claim a pack, a licence or a key count the
/// shipped `.kvpack` files do not have.
fn site(args: &SiteArgs) -> Result<()> {
    let root = workspace_root()?;
    let out = if args.out.is_absolute() {
        args.out.clone()
    } else {
        root.join(&args.out)
    };
    let audio_dir = out.join("assets").join("audio");
    if args.json_only {
        if !audio_dir.is_dir() {
            bail!(
                "{} does not exist; run `cargo xtask site` once before --json-only",
                audio_dir.display()
            );
        }
    } else {
        fs::create_dir_all(&audio_dir)?;
    }

    let packs = discover_site_packs(&root)?;
    if packs.is_empty() {
        bail!("no .kvpack found under assets/soundpacks or any installed pack directory");
    }

    let mut used_slugs: Vec<String> = Vec::new();
    let mut array = Vec::new();
    for (path, pack) in &packs {
        let slug = unique_slug(&pack.metadata().name, &mut used_slugs);
        let preview = write_preview(pack, path, &slug, &audio_dir, args.json_only)?;
        array.push(pack_entry(pack, &slug, preview));
    }

    let catalog = serde_json::json!({
        "generated_by": "cargo xtask site",
        "source": "assets/soundpacks/*.kvpack",
        "pack_count": array.len(),
        "packs": array,
    });
    let text = serde_json::to_string_pretty(&catalog)?;
    let target = out.join("packs.json");
    fs::write(&target, format!("{text}\n"))
        .with_context(|| format!("could not write {}", target.display()))?;

    let releases = array
        .iter()
        .filter(|pack| pack["has_release"] == serde_json::Value::Bool(true))
        .count();
    println!(
        "packs  {} written ({} packs, {} with release sounds)",
        target.display(),
        packs.len(),
        releases
    );
    Ok(())
}

/// Pack search order for the site: the checkout's own `assets/soundpacks`
/// first, so a committed catalog always describes the packs KeyVibes ships,
/// then the same directories `keyvibes pack list` searches as a fallback for
/// a tree that only has installed packs.
fn site_pack_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join("assets").join("soundpacks")];

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
    dirs
}

/// Opens every `.kvpack` reachable from [`site_pack_dirs`], keeping the first
/// pack under each metadata name (the second copy of an installed pack is
/// the same audio under another file name).
fn discover_site_packs(root: &Path) -> Result<Vec<(PathBuf, kv_pack::KvPack)>> {
    let mut found: Vec<(PathBuf, kv_pack::KvPack)> = Vec::new();
    for dir in site_pack_dirs(root) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "kvpack"))
            .collect();
        paths.sort();
        for path in paths {
            let pack = kv_pack::KvPack::open(&path)
                .with_context(|| format!("could not open {}", path.display()))?;
            let name = pack.metadata().name.clone();
            if found.iter().any(|(_, other)| other.metadata().name == name) {
                continue;
            }
            found.push((path, pack));
        }
    }
    found.sort_by(|a, b| a.1.metadata().name.cmp(&b.1.metadata().name));
    Ok(found)
}

/// Writes the pack's preview strip and describes it for `packs.json`.
///
/// Returns `None` when the pack exposes no previewable slot at all; the JSON
/// then carries `"preview": null` and the site falls back to metadata only.
fn write_preview(
    pack: &kv_pack::KvPack,
    path: &Path,
    slug: &str,
    audio_dir: &Path,
    json_only: bool,
) -> Result<Option<serde_json::Value>> {
    let sample_rate = pack.header().sample_rate;
    let channels = pack.header().channels.max(1);
    let (pcm, slots) = build_strip(pack);
    if slots.is_empty() {
        println!(
            "skip  {} ({}) has no previewable key",
            pack.metadata().name,
            path.display()
        );
        return Ok(None);
    }

    let file = format!("assets/audio/{slug}.wav");
    if !json_only {
        write_preview_wav(
            &audio_dir.join(format!("{slug}.wav")),
            sample_rate,
            channels,
            &pcm,
        )?;
    }
    // Relative to the site directory, which is what a browser resolves.
    let frames: u64 = slots.iter().map(|slot| slot.frames).sum();
    Ok(Some(serde_json::json!({
        "file": file,
        "sample_rate": sample_rate,
        "channels": channels,
        "duration": seconds(frames, sample_rate),
        "slots": slots.iter().map(|slot| serde_json::json!({
            "key": slot.key,
            "label": slot.key,
            "kind": slot.kind,
            "start": slot.start_seconds(sample_rate),
            "duration": slot.duration_seconds(sample_rate),
        })).collect::<Vec<_>>(),
    })))
}

/// Concatenates the pack's own PCM into one strip and records where each
/// key's clip begins.
///
/// Clips are copied exactly as stored, guards trimmed off and nothing else
/// touched: what the site plays is what the engine plays.
fn build_strip(pack: &kv_pack::KvPack) -> (Vec<i16>, Vec<Slot>) {
    let sample_rate = pack.header().sample_rate;
    let channels = pack.header().channels.max(1) as usize;
    let max_samples = (f64::from(sample_rate) * MAX_SLOT_SECONDS) as usize * channels;

    let mut pcm = Vec::new();
    let mut slots = Vec::new();
    let mut cursor = 0u64;

    for key in PREVIEW_PRESS_KEYS {
        push_slot(
            pack,
            key,
            "press",
            &mut pcm,
            &mut slots,
            &mut cursor,
            channels,
            max_samples,
        );
    }
    for key in PREVIEW_RELEASE_KEYS {
        push_slot(
            pack,
            key,
            "release",
            &mut pcm,
            &mut slots,
            &mut cursor,
            channels,
            max_samples,
        );
    }
    (pcm, slots)
}

/// Copies one key's first variant into the strip. A key the pack does not
/// carry is skipped: coverage below 104 is normal, and the site must stay
/// silent there instead of borrowing another key's clip.
#[allow(clippy::too_many_arguments)]
fn push_slot(
    pack: &kv_pack::KvPack,
    key: kv_core::PhysicalKey,
    kind: &'static str,
    pcm: &mut Vec<i16>,
    slots: &mut Vec<Slot>,
    cursor: &mut u64,
    channels: usize,
    max_samples: usize,
) {
    let clip_index = match kind {
        "release" => match pack.release_lookup(key) {
            Some((first, _)) => first,
            None => return,
        },
        _ => match pack.lookup(key) {
            Some(sounds) => sounds.first_clip,
            None => return,
        },
    };
    let Some(entry) = pack.clips().get(clip_index as usize) else {
        return;
    };
    let Some(clip) = pack.get_clip(clip_index) else {
        return;
    };

    let start = entry.guard_before as usize;
    let end = start.saturating_add(entry.sample_frames as usize);
    if end > clip.len() || end <= start {
        return;
    }
    let mut logical = &clip[start..end];
    if logical.len() % channels != 0 {
        return;
    }
    if logical.len() > max_samples {
        logical = &logical[..max_samples];
    }
    if logical.is_empty() {
        return;
    }

    let frames = (logical.len() / channels) as u64;
    slots.push(Slot {
        key: format!("{key:?}"),
        kind,
        start: *cursor,
        frames,
    });
    *cursor += frames;
    pcm.extend_from_slice(logical);
}

fn write_preview_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("could not create {}", path.display()))?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

/// The JSON object for one pack, built entirely from what the file says.
fn pack_entry(
    pack: &kv_pack::KvPack,
    slug: &str,
    preview: Option<serde_json::Value>,
) -> serde_json::Value {
    let metadata = pack.metadata();
    let stats = pack.stats();
    let source = metadata.source.trim().to_string();
    serde_json::json!({
        "name": metadata.name,
        "slug": slug,
        "category": category_of(metadata.category.as_str()),
        "synthesized": source.starts_with("keyvibes-synth"),
        "keys": stats.keys,
        "coverage": pack.key_coverage(),
        "release_keys": pack.release_key_count(),
        "has_release": pack.has_release_sounds(),
        "clips": stats.clips,
        "sample_rate": stats.sample_rate,
        "channels": pack.header().channels.max(1),
        "author": nonempty(metadata.author.clone()),
        "license": nonempty(metadata.license.clone()),
        "source": nonempty(metadata.source.clone()),
        "description": nonempty(metadata.description.clone()),
        "preview": preview,
    })
}

/// Lower-cased switch genre, or `"uncategorized"` for a pack whose
/// manifest left it blank.
fn category_of(raw: &str) -> String {
    let category = raw.trim().to_ascii_lowercase();
    if category.is_empty() {
        "uncategorized".to_string()
    } else {
        category
    }
}

fn nonempty(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// URL-safe slug; a name that collapses onto one already used gets a numeric
/// suffix so two packs can never fight over one preview file.
fn unique_slug(name: &str, used: &mut Vec<String>) -> String {
    let base = slugify(name);
    let mut candidate = base.clone();
    let mut copy = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{copy}");
        copy += 1;
    }
    used.push(candidate.clone());
    candidate
}

fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !slug.is_empty() {
            slug.push('-');
            dash = true;
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "pack".to_string()
    } else {
        trimmed
    }
}

// -------------------------------------------------------------------- dist

fn dist(args: &DistArgs) -> Result<()> {
    let root = workspace_root()?;
    let version = version();
    let out = if args.out.is_absolute() {
        args.out.clone()
    } else {
        root.join(&args.out)
    };
    fs::create_dir_all(&out)?;

    let want = |format: Format| args.format == Format::All || args.format == format;

    if !args.no_build {
        println!("build  cargo build --release");
        run("cargo", &["build", "--release"], &root)?;
        build_packs()?;
    }

    let stage = root.join("dist/stage");
    if !args.no_build {
        if stage.try_exists()? {
            fs::remove_dir_all(&stage)?;
        }
        println!(
            "stage  make install DESTDIR={} PREFIX=/usr",
            stage.display()
        );
        let destdir = format!("DESTDIR={}", stage.display());
        run("make", &["-s", "install", &destdir, "PREFIX=/usr"], &root)?;
    }
    if !stage.join("usr").is_dir() {
        bail!(
            "nothing staged at {}; run without --no-build",
            stage.display()
        );
    }

    let mut produced: Vec<PathBuf> = Vec::new();

    let portable_name = format!("keyvibes-{version}-x86_64-linux.tar.gz");
    if want(Format::Portable) || want(Format::Installer) {
        let portable = make_portable(&root, &stage, &out, &version, &portable_name)?;
        if want(Format::Portable) {
            println!("ok     {}", portable.display());
            produced.push(portable);
        }
    }

    if want(Format::Deb) {
        let deb = make_deb(&root, &stage, &out, &version)?;
        println!("ok     {}", deb.display());
        produced.push(deb);
    }

    if want(Format::Pkgbuild) {
        let pkgbuild = make_pkgbuild(&out, &version, &portable_name)?;
        println!("ok     {}", pkgbuild.display());
        produced.push(pkgbuild);
    }

    if want(Format::Rpm) {
        let spec = make_rpm_spec(&out, &version)?;
        println!("ok     {}", spec.display());
        if have("rpmbuild") {
            match make_rpm(&root, &stage, &spec, &out, &version) {
                Ok(rpm) => {
                    println!("ok     {}", rpm.display());
                    produced.push(rpm);
                }
                Err(error) => {
                    // The spec is the artifact people actually build from; a
                    // host whose rpm tooling is broken or absent must not
                    // fail the whole `dist all` run the way a broken deb
                    // would. The deb, portable tarball and installer still
                    // install KeyVibes. Report clearly and keep going.
                    println!("skip   RPM not built: {error:#}");
                }
            }
        } else {
            println!("skip   rpmbuild is not installed; the spec was written instead");
        }
        produced.push(spec);
    }

    if want(Format::Installer) {
        let installer = make_installer(&out, &version, &portable_name)?;
        println!("ok     {}", installer.display());
        produced.push(installer);
    }

    if want(Format::Appimage) {
        match make_appimage(&root, &stage, &out, &version) {
            Ok(appimage) => {
                println!("ok     {}", appimage.display());
                produced.push(appimage);
            }
            Err(error) => {
                // An AppImage is the preferred *download*, but it is not the
                // only way to install KeyVibes. Missing tooling here must not
                // fail the whole `dist all` run the way a broken deb would:
                // the deb, portable tarball and installer still work. Report
                // clearly and keep going; CI installs appimagetool so the
                // release path does produce one.
                println!("skip   AppImage not built: {error:#}");
            }
        }
    }

    if args.format == Format::All {
        let sums = write_checksums(&out)?;
        println!("ok     {}", sums.display());
        produced.push(sums);

        let releases = write_releases_json(&out, &version, &produced)?;
        println!("ok     {}", releases.display());

        let site = root.join("site/releases.json");
        fs::copy(&releases, &site)
            .with_context(|| format!("could not write {}", site.display()))?;
        println!("ok     {}", site.display());
    }

    println!("\n{} artifact(s) in {}", produced.len(), out.display());
    Ok(())
}

// ----------------------------------------------------------------- portable

/// Stages `/usr` into a relocatable tree plus the installer that unpacks it.
fn make_portable(
    root: &Path,
    stage: &Path,
    out: &Path,
    version: &str,
    file_name: &str,
) -> Result<PathBuf> {
    let name = format!("keyvibes-{version}-x86_64-linux");
    let tree = out.join(&name);
    if tree.try_exists()? {
        fs::remove_dir_all(&tree)?;
    }
    copy_tree(&stage.join("usr"), &tree)?;

    // The service unit was rendered for PREFIX=/usr; point it at the tree's
    // own bin/ so the archive is genuinely self-contained.
    let unit_src = tree.join("lib/systemd/user/keyvibes.service");
    if let Ok(unit) = fs::read_to_string(&unit_src) {
        let patched = unit.replace("/usr/bin", "./bin");
        let _ = fs::write(&unit_src, patched);
    }

    fs::copy(root.join("dist/install.sh"), tree.join("install.sh"))?;
    make_executable(&tree.join("install.sh"))?;

    let tarball = out.join(file_name);
    let archive_name = tree.file_name().unwrap_or_default().to_string_lossy();
    run(
        "tar",
        &[
            "-czf",
            tarball.to_str().context("tarball path is not utf-8")?,
            "-C",
            out.to_str().context("out path is not utf-8")?,
            &archive_name,
        ],
        root,
    )?;
    fs::remove_dir_all(&tree)?;
    Ok(tarball)
}

// ---------------------------------------------------------------------- deb

fn make_deb(root: &Path, stage: &Path, out: &Path, version: &str) -> Result<PathBuf> {
    let file_name = format!("keyvibes_{version}_amd64.deb");
    let work = out.join("deb");
    if work.try_exists()? {
        fs::remove_dir_all(&work)?;
    }
    let control_dir = work.join("control");
    fs::create_dir_all(&control_dir)?;

    let size = dir_size(&stage.join("usr")) / 1024;
    let control = format!(
        "Package: keyvibes\n\
         Version: {version}\n\
         Section: sound\n\
         Priority: optional\n\
         Architecture: amd64\n\
         Installed-Size: {size}\n\
         Maintainer: KeyVibes Contributors <keyvibes@localhost>\n\
         Depends: libc6, libpipewire-0.3-0, libgtk-4-1, libadwaita-1-0\n\
         Homepage: https://github.com/IndraJeet-09/keyvibes\n\
         Description: Mechanical keyboard sounds for Linux\n\
          KeyVibes plays a mechanical switch sound on every key press. It\n\
          reads key presses from evdev and renders the mix to PipeWire.\n\
          It is not a remapper and not a keylogger: key codes choose a\n\
          sound and are then forgotten.\n\
          .\n\
          This package ships the headless engine (keyvibes) and the\n\
          GTK 4/libadwaita desktop app (keyvibes-ui).\n"
    );
    fs::write(control_dir.join("control"), &control)?;
    // postinst/prerm: reload udev so the shipped keyboard rule takes effect
    // immediately instead of at next boot. No group is added and nothing is
    // chmod'ed - udev only applies TAG+="uaccess" to event devices udev itself
    // already classifies as keyboards, and logind then sets per-seat ACLs.
    // Exit 0 unconditionally: a package must never fail to configure because
    // udevadm is absent (containers, chroots) or reload returns nonzero.
    let postinst = "#!/bin/sh\n\
         set -e\n\
         if [ -f /etc/udev/rules.d/70-keyvibes-uaccess.rules ] || \\\n\
            [ -f /usr/lib/udev/rules.d/70-keyvibes-uaccess.rules ] || \\\n\
            [ -f /lib/udev/rules.d/70-keyvibes-uaccess.rules ]; then\n\
         \x20   command -v udevadm >/dev/null 2>&1 || exit 0\n\
         \x20   udevadm control --reload-rules || true\n\
         \x20   udevadm trigger --subsystem-match=input || true\n\
         fi\n\
         exit 0\n";
    let prerm = "#!/bin/sh\n\
         set -e\n\
         # Reload after removal so a stale rule does not keep applying.\n\
         # Package files are gone by the time prerm runs for `remove` on\n\
         # dpkg's default order; the reload here is a harmless no-op for\n\
         # upgrades (postrm is not called), so nothing else is needed.\n\
         command -v udevadm >/dev/null 2>&1 || exit 0\n\
         udevadm control --reload-rules || true\n\
         exit 0\n";
    // Note on conffiles: deliberately none. The rule ships in the
    // package-owned /usr/lib/udev/rules.d (proper for a .deb); a local
    // admin override belongs in /etc/udev/rules.d/, which udev reads first
    // and dpkg never touches. No conffile means upgrades cannot prompt.
    fs::write(control_dir.join("postinst"), postinst)?;
    fs::write(control_dir.join("prerm"), prerm)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["postinst", "prerm"] {
            fs::set_permissions(control_dir.join(name), fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::write(work.join("debian-binary"), "2.0\n")?;

    run(
        "tar",
        &[
            "-czf",
            work.join("control.tar.gz").to_str().context("path")?,
            "-C",
            control_dir.to_str().context("path")?,
            "./control",
            "./postinst",
            "./prerm",
        ],
        root,
    )?;
    run(
        "tar",
        &[
            "-czf",
            work.join("data.tar.gz").to_str().context("path")?,
            "-C",
            stage.to_str().context("path")?,
            ".",
        ],
        root,
    )?;

    let deb = out.join(&file_name);
    if deb.try_exists()? {
        fs::remove_file(&deb)?;
    }
    run(
        "ar",
        &[
            "rcs",
            deb.to_str().context("path")?,
            "debian-binary",
            "control.tar.gz",
            "data.tar.gz",
        ],
        &work,
    )?;
    // The unpacked control and data trees are build scratch, not artifacts.
    fs::remove_dir_all(&work)?;
    Ok(deb)
}

// --------------------------------------------------------------------- rpm

fn make_rpm_spec(out: &Path, version: &str) -> Result<PathBuf> {
    let spec = format!(
        "Name:           keyvibes\n\
         Version:        {version}\n\
         Release:        1%{{?dist}}\n\
         Summary:        Mechanical keyboard sounds for Linux\n\
         License:        MIT OR Apache-2.0\n\
         URL:            https://github.com/IndraJeet-09/keyvibes\n\
         BuildArch:      x86_64\n\
         \n\
         # The payload is staged into the RPM source directory before the\n\
         # build, because rpmbuild runs %install inside its own build\n\
         # directory, where a path relative to the repository root does not\n\
         # resolve. Override it to build from a checkout without staging:\n\
         #   rpmbuild -bb --define 'stage_dir dist/stage' keyvibes-{version}.spec\n\
         %global stage_dir %{{_sourcedir}}/stage\n\
         \n\
         %description\n\
         KeyVibes plays a mechanical switch sound on every key press. It reads\n\
         key presses from evdev and renders the mix to PipeWire. It is not a\n\
         remapper and not a keylogger: key codes choose a sound and are then\n\
         forgotten.\n\
         \n\
         %install\n\
         rm -rf %{{buildroot}}\n\
         mkdir -p %{{buildroot}}\n\
         cp -a %{{stage_dir}}/usr/. %{{buildroot}}/\n\
         \n\
         %post\n\
         # Reload udev so the keyboard rule takes effect now, not at next\n\
         # boot. Never fail the transaction: containers and chroots may not\n\
         # have udevadm, and a reload problem must not roll back an install.\n\
         if [ -f /usr/lib/udev/rules.d/70-keyvibes-uaccess.rules ] || \\\n\
            [ -f /lib/udev/rules.d/70-keyvibes-uaccess.rules ]; then\n\
         \x20   command -v udevadm >/dev/null 2>&1 && {{\n\
         \x20   \x20   udevadm control --reload-rules || :\n\
         \x20   \x20   udevadm trigger --subsystem-match=input || :\n\
         \x20   }}\n\
         fi\n\
         \n\
         %postun\n\
         # On final removal ($1 = 0) the rule is gone; reload so a stale\n\
         # rule does not keep applying. On upgrade the rule is still\n\
         # present and %post already reloaded - skip to keep upgrades quiet.\n\
         if [ $1 -eq 0 ]; then\n\
         \x20   command -v udevadm >/dev/null 2>&1 && udevadm control --reload-rules || :\n\
         fi\n\
         \n\
         %files\n\
         %license %{{_sourcedir}}/LICENSE-MIT %{{_sourcedir}}/LICENSE-APACHE\n\
         %doc %{{_sourcedir}}/README.md %{{_sourcedir}}/THIRD_PARTY_NOTICES.md\n\
         /usr/bin/keyvibes\n\
         /usr/bin/keyvibes-ui\n\
         /usr/share/keyvibes/*\n\
         /usr/share/applications/keyvibes-ui.desktop\n\
         /usr/share/icons/hicolor/scalable/apps/keyvibes.svg\n\
         /usr/share/doc/keyvibes/*\n\
         /usr/lib/systemd/user/keyvibes.service\n\
         /usr/lib/udev/rules.d/70-keyvibes-uaccess.rules\n"
    );
    let path = out.join(format!("keyvibes-{version}.spec"));
    fs::write(&path, spec)?;
    Ok(path)
}

fn make_rpm(root: &Path, stage: &Path, spec: &Path, out: &Path, version: &str) -> Result<PathBuf> {
    let top = out.join("rpm");
    for dir in ["SPECS", "BUILD", "RPMS", "SRPMS", "SOURCES"] {
        fs::create_dir_all(top.join(dir))?;
    }
    fs::copy(spec, top.join("SPECS").join(spec.file_name().unwrap()))?;

    // The spec reaches its payload through %{_sourcedir}, so the staged tree
    // and the licence files have to sit in the source directory. rpmbuild
    // runs %install in its own build directory, where the repository-relative
    // path the spec used to name does not resolve.
    let sources = top.join("SOURCES");
    let payload = sources.join("stage");
    if payload.try_exists()? {
        fs::remove_dir_all(&payload)?;
    }
    copy_tree(stage, &payload)?;
    for name in [
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
    ] {
        let from = root.join(name);
        if from.is_file() {
            fs::copy(&from, sources.join(name))?;
        }
    }

    run(
        "rpmbuild",
        &[
            "-bb",
            "--define",
            &format!("_topdir {}", top.display()),
            spec.to_str().context("spec path is not utf-8")?,
        ],
        root,
    )?;

    // rpmbuild honours %{?dist}; collect whatever it produced.
    let rpms = top.join("RPMS/x86_64");
    let produced = fs::read_dir(&rpms)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .find(|path| path.extension().is_some_and(|ext| ext == "rpm"))
        })
        .unwrap_or(None);
    let Some(built) = produced else {
        bail!(
            "rpmbuild reported success but wrote no rpm under {}",
            rpms.display()
        );
    };
    let target = out.join(format!("keyvibes-{version}-1.x86_64.rpm"));
    fs::rename(&built, &target)?;
    Ok(target)
}

// ----------------------------------------------------------------- pkgbuild

fn make_pkgbuild(out: &Path, version: &str, portable: &str) -> Result<PathBuf> {
    let pkgbuild = format!(
        "# Maintainer: KeyVibes Contributors <keyvibes@localhost>\n\
         pkgname=keyvibes\n\
         pkgver={version}\n\
         pkgrel=1\n\
         pkgdesc='Mechanical keyboard sounds for Linux'\n\
         arch=('x86_64')\n\
         url='https://github.com/IndraJeet-09/keyvibes'\n\
         license=('MIT' 'Apache-2.0')\n\
         depends=('glibc' 'pipewire' 'gtk4' 'libadwaita')\n\
         makedepends=('cargo')\n\
         source=(\"$url/archive/refs/tags/v$pkgver.tar.gz\")\n\
         sha256sums=('SKIP')\n\
         \n\
         build() {{\n\
         \tcd \"$pkgname-$pkgver\"\n\
         \tcargo build --frozen --release\n\
         \tcargo xtask packs\n\
         }}\n\
         \n\
         package() {{\n\
         \tcd \"$pkgname-$pkgver\"\n\
         \tmake DESTDIR=\"$pkgdir\" PREFIX=/usr install\n\
         }}\n\
         \n\
         # A prebuilt archive is also published for this release as\n\
         # {portable}, for machines that do not want to compile.\n"
    );
    let path = out.join("PKGBUILD");
    fs::write(&path, pkgbuild)?;
    Ok(path)
}

// ----------------------------------------------------------------- appimage

/// Builds a self-contained AppImage of the engine.
///
/// The AppImage carries the `keyvibes` engine, every installed pack, and the
/// `libpipewire` client library so it runs on any glibc-based distribution
/// without a package manager. It deliberately does **not** carry the GTK
/// desktop app: bundling GTK4 and its whole dependency closure is fragile and
/// distribution packages already ship it with the correct system libraries.
/// PipeWire itself is a host service the client library connects to; it is
/// documented as a prerequisite, never bundled.
fn make_appimage(root: &Path, stage: &Path, out: &Path, version: &str) -> Result<PathBuf> {
    let arch = std::env::consts::ARCH;
    let file_name = format!("KeyVibes-{version}-{arch}.AppImage");
    let appdir = out.join("KeyVibes.AppDir");
    if appdir.try_exists()? {
        fs::remove_dir_all(&appdir)?;
    }
    fs::create_dir_all(appdir.join("usr/bin"))?;
    fs::create_dir_all(appdir.join("usr/lib/keyvibes"))?;
    fs::create_dir_all(appdir.join("usr/share/keyvibes/packs"))?;
    fs::create_dir_all(appdir.join("usr/share/icons/hicolor/scalable/apps"))?;

    // Engine binary.
    fs::copy(
        stage.join("usr/bin/keyvibes"),
        appdir.join("usr/bin/keyvibes"),
    )?;
    make_executable(&appdir.join("usr/bin/keyvibes"))?;

    // Every pack the stage carries, so first launch needs no import step.
    let packs_src = stage.join("usr/share/keyvibes/packs");
    let mut pack_count = 0usize;
    for entry in fs::read_dir(&packs_src)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "kvpack") {
            fs::copy(
                entry.path(),
                appdir
                    .join("usr/share/keyvibes/packs")
                    .join(entry.file_name()),
            )?;
            pack_count += 1;
        }
    }
    if pack_count == 0 {
        bail!(
            "the staged tree at {} carries no packs; run without --no-build",
            stage.display()
        );
    }

    // The libpipewire *client* library. The PipeWire server and its SPA
    // plugins stay on the host: the client only opens the session socket.
    let pipewire_lib = find_pipewire_lib().with_context(|| {
        "libpipewire-0.3.so.0 was not found; install the PipeWire client \
         library (libpipewire-0.3-0 on Debian/Ubuntu, pipewire-libs on Fedora) \
         to build an AppImage"
    })?;
    fs::copy(
        &pipewire_lib,
        appdir.join("usr/lib/keyvibes/libpipewire-0.3.so.0"),
    )?;

    // Icon and desktop entry, at the AppDir root where appimagetool expects them.
    fs::copy(root.join("dist/keyvibes.svg"), appdir.join("keyvibes.svg"))?;
    fs::copy(
        root.join("dist/keyvibes.svg"),
        appdir.join("usr/share/icons/hicolor/scalable/apps/keyvibes.svg"),
    )?;
    fs::write(appdir.join("KeyVibes.desktop"), APPIMAGE_DESKTOP)?;
    fs::write(appdir.join("AppRun"), APPIMAGE_APPRUN)?;
    make_executable(&appdir.join("AppRun"))?;
    // A .DirIcon makes file managers show the icon for the AppDir itself.
    fs::copy(root.join("dist/keyvibes.svg"), appdir.join(".DirIcon"))?;

    let appimage = out.join(&file_name);
    if appimage.try_exists()? {
        fs::remove_file(&appimage)?;
    }
    match Command::new("appimagetool")
        .args([
            appdir.to_str().context("appdir path is not utf-8")?,
            appimage.to_str().context("appimage path is not utf-8")?,
        ])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => Ok(appimage),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "appimagetool failed:\n{stderr}\n\
                 install it from https://github.com/AppImage/appimagetool/releases \
                 (or set PATH) and re-run"
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Leave the AppDir behind so the caller can still inspect or
            // finish the build on a machine that has the tool.
            bail!(
                "appimagetool is not on PATH; the AppDir was staged at {} \
                 install appimagetool and re-run, or use the portable tarball",
                appdir.display()
            )
        }
        Err(error) => Err(error).context("could not run appimagetool"),
    }
}

/// Locates `libpipewire-0.3.so.0` via the dynamic loader's search path.
fn find_pipewire_lib() -> Result<PathBuf> {
    let name = "libpipewire-0.3.so.0";
    let output = Command::new("ldconfig")
        .arg("-p")
        .output()
        .ok()
        .filter(|out| out.status.success());
    if let Some(output) = output {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(name) {
                // "libpipewire-0.3.so.0 (libc6,x86-64) => /usr/lib/libpipewire-0.3.so.0"
                if let Some((_, path)) = rest.split_once("=> ") {
                    let path = PathBuf::from(path.trim());
                    if path.is_file() {
                        return Ok(path);
                    }
                }
            }
        }
    }
    // Fall back to the usual library directories.
    for dir in ["/usr/lib", "/usr/lib64", "/lib", "/lib64"] {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("not found via ldconfig or standard library directories")
}

/// Desktop entry shown by file managers and app launchers for the AppImage.
const APPIMAGE_DESKTOP: &str = r#"[Desktop Entry]
Type=Application
Version=1.0
Name=KeyVibes
GenericName=Keyboard sound
Comment=Play a mechanical keyboard sound on every key press
Exec=keyvibes
Icon=keyvibes
Terminal=false
Categories=AudioVideo;Audio;
Keywords=keyboard;switch;sound;typing;mechanical;
StartupNotify=true
"#;

/// AppRun: locate the bundled engine and packs, then exec it.
///
/// Sets `LD_LIBRARY_PATH` so the bundled libpipewire client is found, points
/// `KEYVIBES_PACK_DIR` at the bundled packs (unless the user already set it),
/// and gives a plain-language hint when the PipeWire session socket is absent
/// instead of a bare dynamic-loader error.
const APPIMAGE_APPRUN: &str = r#"#!/bin/sh
# KeyVibes AppImage launcher. Generated by `cargo xtask dist appimage`.
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export LD_LIBRARY_PATH="$HERE/usr/lib/keyvibes${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Bundled packs, unless the user points at their own.
if [ -z "${KEYVIBES_PACK_DIR:-}" ]; then
    KEYVIBES_PACK_DIR="$HERE/usr/share/keyvibes/packs"
    export KEYVIBES_PACK_DIR
fi
# A missing PipeWire session is the common first-run failure; name it once,
# plainly, instead of letting the engine fail on a socket error.
if [ -z "${PIPEWIRE_RUNTIME_DIR:-}${XDG_RUNTIME_DIR:-}" ]; then
    echo "KeyVibes: no XDG_RUNTIME_DIR; PipeWire (your desktop audio session) is probably not running." >&2
    echo "  fix: log into a desktop session, then re-run. `keyvibes doctor` explains the rest." >&2
fi
exec "$HERE/usr/bin/keyvibes" "$@"
"#;

// ---------------------------------------------------------------- installer

fn make_installer(out: &Path, version: &str, portable: &str) -> Result<PathBuf> {
    let sha = sha256_of(&out.join(portable))?;
    let script = INSTALLER
        .replace("@VERSION@", version)
        .replace("@ARCHIVE@", portable)
        .replace("@SHA256@", &sha);
    let path = out.join("install.sh");
    fs::write(&path, script)?;
    make_executable(&path)?;
    Ok(path)
}

const INSTALLER: &str = r#"#!/bin/sh
# KeyVibes @VERSION@ installer.
#
#   ./install.sh                install into $HOME/.local
#   PREFIX=/usr ./install.sh    install system-wide (needs sudo)
#
# Requires the portable archive to sit next to this script (both are on the
# release page). Verifies it against the checksum baked in at release time,
# then unpacks and installs. The script itself never touches the network -
# download both files with your browser or a fetch tool of your choice.
set -eu

VERSION="@VERSION@"
ARCHIVE="@ARCHIVE@"
SHA256="@SHA256@"
PREFIX="${PREFIX:-$HOME/.local}"

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ ! -f "$here/$ARCHIVE" ]; then
    echo "$ARCHIVE not found next to this script." >&2
    echo "Download it from the same release page as this installer:" >&2
    echo "  https://github.com/IndraJeet-09/keyvibes/releases" >&2
    echo "and place it in the same directory, then re-run." >&2
    exit 1
fi
cp "$here/$ARCHIVE" "$work/$ARCHIVE"

if ! command -v sha256sum >/dev/null 2>&1; then
    echo "sha256sum is required to verify $ARCHIVE but was not found." >&2
    exit 1
fi
if ! echo "$SHA256  $work/$ARCHIVE" | sha256sum --check --status 2>/dev/null; then
    echo "Checksum mismatch for $ARCHIVE" >&2
    exit 1
fi

tar -xzf "$work/$ARCHIVE" -C "$work"
tree=$(find "$work" -maxdepth 1 -type d -name 'keyvibes-*' | head -n 1)

mkdir -p "$PREFIX/bin" "$PREFIX/share" "$PREFIX/lib/systemd/user"
cp -a "$tree/bin/." "$PREFIX/bin/"
cp -a "$tree/share/." "$PREFIX/share/"
cp -a "$tree/lib/systemd/user/." "$PREFIX/lib/systemd/user/"

echo "Installed KeyVibes $VERSION into $PREFIX"
echo
echo "Next:"
echo "  keyvibes doctor                      # is this machine ready?"
echo "  keyvibes-ui                          # open the desktop app"
echo "  systemctl --user daemon-reload"
echo "  systemctl --user enable --now keyvibes"
echo
if ! id -nG | tr ' ' '\n' | grep -qx input; then
    echo "If doctor cannot read /dev/input:"
    echo "  sudo usermod -aG input \$USER   (then log out and back in)"
fi
"#;

// --------------------------------------------------------------- checksums

fn write_checksums(out: &Path) -> Result<PathBuf> {
    let mut lines = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(out)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name != "SHA256SUMS" && name != "releases.json"
        })
        .collect();
    entries.sort();
    for path in entries {
        lines.push(format!("{}  {}", sha256_of(&path)?, file_name(&path)?));
    }
    let target = out.join("SHA256SUMS");
    fs::write(&target, lines.join("\n") + "\n")?;
    Ok(target)
}

/// The download kind a produced file belongs to, or "other" for a file the
/// site has no download row for.
fn artifact_kind(name: &str) -> &'static str {
    if name == "PKGBUILD" {
        return "pkgbuild";
    }
    if name.ends_with(".AppImage") {
        return "appimage";
    }
    match name.split('.').next_back().unwrap_or("") {
        "deb" => "deb",
        "spec" => "rpm-spec",
        "rpm" => "rpm",
        "gz" => "portable",
        "sh" => "installer",
        _ => "other",
    }
}

/// Writes the machine-readable release description the website reads.
///
/// The `files` map lists only the kinds this release actually carries. A
/// packaging format the build host could not produce must not be advertised
/// as downloadable: the site would link to a file that was never published.
fn write_releases_json(out: &Path, version: &str, produced: &[PathBuf]) -> Result<PathBuf> {
    let mut entries = Vec::new();
    let mut by_kind: BTreeMap<&str, String> = BTreeMap::new();
    for path in produced {
        let name = file_name(path)?;
        if name == "SHA256SUMS" {
            continue;
        }
        let kind = artifact_kind(&name);
        entries.push(format!(
            "    {{\"name\": {name}, \"kind\": {kind}, \"size\": {size}, \"sha256\": {sha}}}",
            name = json(&name),
            kind = json(kind),
            size = fs::metadata(path)?.len(),
            sha = json(&sha256_of(path)?),
        ));
        by_kind.entry(kind).or_insert_with(|| name.clone());
    }

    let files = by_kind
        .iter()
        .filter(|(kind, _)| **kind != "other")
        .map(|(kind, name)| format!("    {}: {}", json(kind), json(name)))
        .collect::<Vec<_>>()
        .join(",\n");

    let body = format!(
        "{{\n  \"name\": \"KeyVibes\",\n  \"version\": {version},\n  \"repository\": {repo},\n  \"files\": {{\n{files}\n  }},\n  \"artifacts\": [\n{artifacts}\n  ]\n}}\n",
        version = json(version),
        repo = json("https://github.com/IndraJeet-09/keyvibes"),
        files = files,
        artifacts = entries.join(",\n"),
    );
    let target = out.join("releases.json");
    fs::write(&target, body)?;
    Ok(target)
}

fn json(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --------------------------------------------------------------- utilities

fn sha256_of(path: &Path) -> Result<String> {
    let name = file_name(path)?;
    let parent = path.parent().context("path has no parent")?;
    let output = run("sha256sum", &[name.as_str()], parent)?;
    let line = String::from_utf8_lossy(&output.stdout);
    Ok(line
        .split_whitespace()
        .next()
        .context("sha256sum printed nothing")?
        .to_string())
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .context("path has no file name")?
        .to_string_lossy()
        .into_owned())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += dir_size(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("Cherry MX Black ABS"), "cherry-mx-black-abs");
        assert_eq!(slugify("Holy Panda (kbsim)"), "holy-panda-kbsim");
        assert_eq!(slugify("SKCM Blue Alps"), "skcm-blue-alps");
        assert_eq!(slugify("  ...  "), "pack");
    }

    #[test]
    fn unique_slug_never_collides() {
        let mut used = Vec::new();
        assert_eq!(unique_slug("Topre", &mut used), "topre");
        assert_eq!(unique_slug("Topre", &mut used), "topre-2");
        assert_eq!(unique_slug("Topre", &mut used), "topre-3");
    }

    #[test]
    fn category_is_honest_about_missing_genre() {
        assert_eq!(category_of("Linear "), "linear");
        assert_eq!(category_of(""), "uncategorized");
        assert_eq!(category_of("   "), "uncategorized");
    }

    #[test]
    fn blank_metadata_becomes_absent_not_empty() {
        assert_eq!(nonempty("  ".to_string()), None);
        assert_eq!(nonempty("kbsim".to_string()), Some("kbsim".to_string()));
    }

    #[test]
    fn slot_times_come_from_the_pack_sample_rate() {
        let slot = Slot {
            key: "Q".to_string(),
            kind: "press",
            start: 44_100,
            frames: 22_050,
        };
        assert!((slot.start_seconds(44_100) - 1.0).abs() < 1e-9);
        assert!((slot.duration_seconds(44_100) - 0.5).abs() < 1e-9);
        assert_eq!(seconds(10, 0), 0.0);
    }

    /// Every pack shipped in the checkout must open and expose a preview
    /// strip; a pack that silently produced no slots would take the hero
    /// keyboard out of the site.
    #[test]
    fn every_shipped_pack_has_a_preview_strip() {
        let root = workspace_root().expect("workspace root");
        let packs = discover_site_packs(&root).expect("pack discovery");
        assert!(
            packs.len() >= 21,
            "expected the shipped packs, got {}",
            packs.len()
        );
        for (path, pack) in &packs {
            let (pcm, slots) = build_strip(pack);
            assert!(
                !slots.is_empty(),
                "{} produced no preview slots",
                path.display()
            );
            assert!(!pcm.is_empty(), "{} produced empty audio", path.display());

            let mut cursor = 0u64;
            for slot in &slots {
                assert_eq!(
                    slot.start,
                    cursor,
                    "{} slot offsets must be contiguous",
                    path.display()
                );
                cursor += slot.frames;
            }
            let total: u64 = slots.iter().map(|slot| slot.frames).sum();
            let rate = pack.header().sample_rate;
            assert_eq!(
                pcm.len() as u64,
                total * u64::from(pack.header().channels.max(1))
            );
            assert!(
                seconds(total, rate)
                    <= f64::from((PREVIEW_PRESS_KEYS.len() + PREVIEW_RELEASE_KEYS.len()) as u32)
                        * MAX_SLOT_SECONDS
                        + 1.0
            );
        }
    }

    /// The strip has to carry enough distinct keys for the site to build a
    /// varied burst out of it. A pack that exports only a couple of keys
    /// would make every preview sound like the same repeated click.
    #[test]
    fn every_shipped_pack_exports_a_varied_strip() {
        let root = workspace_root().expect("workspace root");
        let packs = discover_site_packs(&root).expect("pack discovery");
        for (path, pack) in &packs {
            let (_, slots) = build_strip(pack);
            let press: std::collections::BTreeSet<&str> = slots
                .iter()
                .filter(|slot| slot.kind == "press")
                .map(|slot| slot.key.as_str())
                .collect();
            assert!(
                press.len() >= 40,
                "{} exported only {} press keys",
                path.display(),
                press.len()
            );
            assert!(
                press.contains("A") && press.contains("Q") && press.contains("Space"),
                "{} is missing a common typing key",
                path.display()
            );
        }
    }

    /// A pack with release sounds must export them, so the site can layer a
    /// key-up into the burst instead of only ever playing key-downs.
    #[test]
    fn release_slots_follow_release_support() {
        let root = workspace_root().expect("workspace root");
        let packs = discover_site_packs(&root).expect("pack discovery");
        for (path, pack) in &packs {
            let (_, slots) = build_strip(pack);
            let releases = slots.iter().filter(|slot| slot.kind == "release").count();
            if pack.has_release_sounds() {
                assert!(
                    releases > 0,
                    "{} has release sounds but exported none",
                    path.display()
                );
            } else {
                assert_eq!(
                    releases,
                    0,
                    "{} exported release clips it does not have",
                    path.display()
                );
            }
        }
    }
}
