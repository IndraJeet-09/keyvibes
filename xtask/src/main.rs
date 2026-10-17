//! KeyVibes build and packaging tool.
//!
//! `cargo xtask dist <format>` stages a release through `make install` and
//! turns it into the artifacts the website links to. Every format is built
//! from the same staged tree, so a package can never disagree with what
//! `make install` puts on disk.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
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

/// Every `pack.toml` under `assets/soundpacks`, paired with the `.kvpack`
/// it should produce (named after the directory, which is what `pack list`
/// and the UI show as the file identity).
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
        let manifest = dir.join("pack.toml");
        if manifest.is_file() {
            let name = pack_name(&manifest, &dir);
            found.push((manifest, search.join(format!("{name}.kvpack"))));
            continue;
        }
        // A directory of packs, e.g. assets/soundpacks/upstream-src.
        let Ok(inner) = fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = inner.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for child in children {
            let manifest = child.join("pack.toml");
            if manifest.is_file() {
                found.push((
                    manifest.clone(),
                    search.join(format!("{}.kvpack", pack_name(&manifest, &child))),
                ));
            }
        }
    }
    found
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
            let rpm = make_rpm(&root, &spec, &out, &version)?;
            println!("ok     {}", rpm.display());
            produced.push(rpm);
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
         Homepage: https://github.com/yourusername/keyvibes\n\
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
    fs::write(work.join("debian-binary"), "2.0\n")?;

    run(
        "tar",
        &[
            "-czf",
            work.join("control.tar.gz").to_str().context("path")?,
            "-C",
            control_dir.to_str().context("path")?,
            "./control",
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
         URL:            https://github.com/yourusername/keyvibes\n\
         BuildArch:      x86_64\n\
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
         cp -a dist/stage/usr/. %{{buildroot}}/\n\
         \n\
         %files\n\
         %license LICENSE-MIT LICENSE-APACHE\n\
         %doc README.md\n\
         /usr/bin/keyvibes\n\
         /usr/bin/keyvibes-ui\n\
         /usr/share/keyvibes/*\n\
         /usr/share/applications/keyvibes-ui.desktop\n\
         /usr/share/icons/hicolor/scalable/apps/keyvibes.svg\n\
         /usr/share/doc/keyvibes/*\n\
         /usr/lib/systemd/user/keyvibes.service\n"
    );
    let path = out.join(format!("keyvibes-{version}.spec"));
    fs::write(&path, spec)?;
    Ok(path)
}

fn make_rpm(root: &Path, spec: &Path, out: &Path, version: &str) -> Result<PathBuf> {
    let top = out.join("rpm");
    for dir in ["SPECS", "BUILD", "RPMS", "SRPMS", "SOURCES"] {
        fs::create_dir_all(top.join(dir))?;
    }
    fs::copy(spec, top.join("SPECS").join(spec.file_name().unwrap()))?;
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
         url='https://github.com/yourusername/keyvibes'\n\
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

// ---------------------------------------------------------------- installer

fn make_installer(out: &Path, version: &str, portable: &str) -> Result<PathBuf> {
    let sha = sha256_of(&out.join(portable))?;
    let script = INSTALLER
        .replace("@VERSION@", version)
        .replace("@ARCHIVE@", portable)
        .replace("@SHA256@", &sha)
        .replace(
            "@URL@",
            &format!(
                "https://github.com/yourusername/keyvibes/releases/download/v{version}/{portable}"
            ),
        );
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
# Downloads the portable archive if it is not sitting next to this script,
# checks it against the checksum baked in at release time, then unpacks it.
set -eu

VERSION="@VERSION@"
ARCHIVE="@ARCHIVE@"
SHA256="@SHA256@"
URL="@URL@"
PREFIX="${PREFIX:-$HOME/.local}"

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
    if [ -f "$here/$ARCHIVE" ]; then
        cp "$here/$ARCHIVE" "$work/$ARCHIVE"
        return
    fi
    echo "Fetching $ARCHIVE"
    if command -v curl >/dev/null 2>&1; then
        curl -fL -o "$work/$ARCHIVE" "$URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "$work/$ARCHIVE" "$URL"
    else
        echo "Need curl or wget, or place $ARCHIVE next to this script." >&2
        exit 1
    fi
}

fetch

echo "$SHA256  $work/$ARCHIVE" | sha256sum --check --status 2>/dev/null || {
    echo "Checksum mismatch for $ARCHIVE" >&2
    exit 1
}

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

fn write_releases_json(out: &Path, version: &str, produced: &[PathBuf]) -> Result<PathBuf> {
    let mut entries = Vec::new();
    for path in produced {
        let name = file_name(path)?;
        if name == "SHA256SUMS" {
            continue;
        }
        let kind = if name == "PKGBUILD" {
            "pkgbuild"
        } else {
            match name.split('.').next_back().unwrap_or("") {
                "deb" => "deb",
                "spec" => "rpm-spec",
                "gz" => "portable",
                "sh" => "installer",
                _ => "other",
            }
        };
        entries.push(format!(
            "    {{\"name\": {name}, \"kind\": {kind}, \"size\": {size}, \"sha256\": {sha}}}",
            name = json(&name),
            kind = json(kind),
            size = fs::metadata(path)?.len(),
            sha = json(&sha256_of(path)?),
        ));
    }

    let files = [
        ("deb", "keyvibes_{}_amd64.deb"),
        ("rpm", "keyvibes-{}-1.x86_64.rpm"),
        ("portable", "keyvibes-{}-x86_64-linux.tar.gz"),
        ("installer", "install.sh"),
        ("pkgbuild", "PKGBUILD"),
    ];
    let mut file_entries = Vec::new();
    for (key, template) in files {
        file_entries.push(format!(
            "    {}: {}",
            json(key),
            json(&template.replace("{}", version))
        ));
    }

    let body = format!(
        "{{\n  \"name\": \"KeyVibes\",\n  \"version\": {version},\n  \"repository\": {repo},\n  \"files\": {{\n{files}\n  }},\n  \"artifacts\": [\n{artifacts}\n  ]\n}}\n",
        version = json(version),
        repo = json("https://github.com/yourusername/keyvibes"),
        files = file_entries.join(",\n"),
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
