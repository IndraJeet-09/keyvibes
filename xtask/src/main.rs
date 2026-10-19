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
/// A manifest is any `*.toml` directly inside a pack directory. `pack.toml`
/// is the common case, but the synthesized demo sources keep several packs
/// in one directory (`default-src/{pack,holy-panda,linear}.toml`), and those
/// must all be built - skipping them silently shipped packages with only
/// the Default demo pack.
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
         # rule does not keep applying. On upgrade the file is still\n\
         # present and %post already reloaded - skip to keep upgrades quiet.\n\
         if [ $1 -eq 0 ]; then\n\
         \x20   command -v udevadm >/dev/null 2>&1 && udevadm control --reload-rules || :\n\
         fi\n\
         \n\
         %files\n\
         %license LICENSE-MIT LICENSE-APACHE\n\
         %doc README.md THIRD_PARTY_NOTICES.md\n\
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

fn write_releases_json(out: &Path, version: &str, produced: &[PathBuf]) -> Result<PathBuf> {
    let mut entries = Vec::new();
    for path in produced {
        let name = file_name(path)?;
        if name == "SHA256SUMS" {
            continue;
        }
        let kind = if name == "PKGBUILD" {
            "pkgbuild"
        } else if name.ends_with(".AppImage") {
            "appimage"
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
        ("appimage", "KeyVibes-{}-x86_64.AppImage"),
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
        repo = json("https://github.com/IndraJeet-09/keyvibes"),
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
