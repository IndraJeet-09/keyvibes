# Installing KeyVibes

KeyVibes runs as a normal user. It never asks for root at runtime, never
writes outside your configuration and your pack directory, and never needs a
display server. The only privileged thing you may have to do once is add
yourself to the `input` group so it can read a keyboard.

## Supported platforms

| Platform | Engine (`keyvibes`) | Desktop app (`keyvibes-ui`) |
|---|---|---|
| x86-64 Linux (glibc, PipeWire) | yes - AppImage, deb, RPM, PKGBUILD, portable tarball | deb, RPM, PKGBUILD, `make install` |
| aarch64 Linux | builds from source; no prebuilt artifact yet | builds from source |
| musl/Alpine | not tested; no prebuilt artifact | not tested |
| Windows, macOS | not supported - the platform surface is evdev + PipeWire only | - |

The AppImage is the recommended download for the engine. It is engine-only
(the GTK UI is deliberately not bundled); use a distro package or
`make install` when you want the UI.

## Requirements

### To build

| | |
|---|---|
| Rust | 1.75 or newer (`rustup` is the usual way to get it) |
| PipeWire headers | `libpipewire-0.3-dev` (Debian/Ubuntu), `pipewire-devel` (Fedora), `pipewire` (Arch) |
| A C toolchain | `build-essential` / `gcc` / `base-devel` - the PipeWire bindings build a small C shim |

### At runtime

| | |
|---|---|
| Kernel | evdev (`CONFIG_INPUT_EVDEV`), present in every stock kernel |
| PipeWire | 0.3.49 or newer, with a running session instance |
| Display server | none for the engine - `keyvibes-ui` needs GTK 4 + libadwaita |
| Root | never, once the `input` permission below is sorted |

Check the machine before you install anything:

```bash
keyvibes doctor
```

It names what is missing and how to fix it, and exits 1 when something the
engine needs is absent.

## Installing

### AppImage (recommended download)

Download `KeyVibes-<version>-x86_64.AppImage` from the releases page, then:

```bash
chmod +x KeyVibes-*-x86_64.AppImage
mv KeyVibes-*-x86_64.AppImage ~/.local/bin/keyvibes-appimage   # optional
keyvibes-appimage doctor
keyvibes-appimage run
```

No root, no install step, no files written outside your home directory. What
the AppImage contains and what it deliberately does not:

- **Contains:** the `keyvibes` engine, the client `libpipewire-0.3.so`, and
  all 24 sound packs.
- **Does not contain:** the GTK desktop app (`keyvibes-ui`), the systemd user
  unit, the udev rule, or the PipeWire *server*. PipeWire must already be
  running - it is part of every desktop session. Bundling GTK into an
  AppImage is not attempted.
- **Permissions:** the AppImage cannot install a udev rule for you. Use one
  of the options under [Keyboard permission](#keyboard-permission).
- **Service:** to run as a systemd user service from an AppImage, point
  `ExecStart` at the AppImage path yourself; the packaged unit is not
  included.

Upgrade by downloading the newer AppImage over the old one. Uninstall by
deleting the file.

### deb / RPM / PKGBUILD

Release artifacts include `keyvibes_<version>_amd64.deb` (engine + UI +
packs + udev rule + user unit), an RPM spec, and a `PKGBUILD` that builds
from the portable tarball. The deb and RPM maintainer scripts reload udev
after install so the keyboard rule takes effect without a reboot.

```bash
sudo apt install ./keyvibes_*_amd64.deb     # Debian/Ubuntu
sudo rpm -i keyvibes-<version>-1.x86_64.rpm # Fedora/RHEL
makepkg -si                                 # Arch, from the PKGBUILD
```

Upgrade with the package manager (`apt install ./new.deb`, `dnf upgrade`,
`pacman -Syu`). Uninstall with `apt remove keyvibes`, `dnf remove keyvibes`,
or `pacman -R keyvibes`. Configuration under `~/.config/keyvibes` is left
alone either way.

### Portable tarball with install.sh

The release tarball unpacks to a tree with an `install.sh`:

```bash
tar -xzf keyvibes-<version>-x86_64-linux.tar.gz
cd keyvibes-<version>-x86_64-linux
./install.sh                      # into $HOME/.local, no root
# or
sudo PREFIX=/usr ./install.sh     # system-wide
```

For a system prefix the script also installs the keyboard udev rule and
reloads udev. Upgrade is the same as install (run it again). Uninstall:

```bash
./install.sh --uninstall                    # or PREFIX=/usr for system
```

### From this checkout, per user (no root)

```bash
make install PREFIX="$HOME/.local"
```

This puts the binary in `~/.local/bin`, the packs in
`~/.local/share/keyvibes/packs`, the docs in `~/.local/share/doc/keyvibes`
and a systemd user unit in `~/.local/share/systemd/user`. Build the packs
first (`cargo xtask packs`) so the install has something to copy.

Make sure `~/.local/bin` is on your `PATH`, then check it:

```bash
keyvibes doctor
keyvibes pack list
```

### From this checkout, system-wide

```bash
sudo make install PREFIX=/usr/local
```

Same layout under `/usr/local`. For `/usr` or `/usr/local` the install also
places the udev rule in `$PREFIX/lib/udev/rules.d/`. Packages built with
`checkinstall` or for a distribution can set `DESTDIR`:

```bash
make install DESTDIR=/tmp/stage PREFIX=/usr
```

### With cargo

```bash
cargo install --path crates/keyvibes --release
```

This installs only the binary, to `~/.cargo/bin`. Sound packs and the systemd
unit are not included; install packs separately (see below).

### Manually

```bash
install -Dm755 target/release/keyvibes ~/.local/bin/keyvibes
install -d ~/.local/share/keyvibes/packs
install -m644 assets/soundpacks/*.kvpack ~/.local/share/keyvibes/packs/
```

## Keyboard permission

Reading a keyboard means opening `/dev/input/event*`. Those nodes are
`root:input 0660`, so one of these has to be true:

**You are in the `input` group** - the common setup:

```bash
sudo usermod -aG input "$USER"
```

Log out and back in for it to take effect, then confirm:

```bash
keyvibes doctor          # the input section should go quiet
```

**Or a logind ACL grants your seat access** - no group membership, but only
for a local graphical/TTY session. The optional udev rule does it, and it
matches keyboards only (`ID_INPUT_KEYBOARD`) - not mice, not other devices:

```bash
sudo install -Dm644 dist/keyvibes-udev.rules \
    /etc/udev/rules.d/70-keyvibes-uaccess.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Distro packages install this rule automatically under
`/usr/lib/udev/rules.d/`; the portable `install.sh` does the same for a
system prefix. AppImage users must install it by hand (the AppImage cannot
write system files).

Neither option needs anything at runtime: once the permission exists,
KeyVibes opens the device as an ordinary user. More detail in
[linux-permissions.md](linux-permissions.md).

## PipeWire

KeyVibes renders through the session's PipeWire instance - the one started
with your user session, not a system daemon.

```bash
systemctl --user status pipewire        # should be active
systemctl --user start pipewire         # if it is not
```

It talks to the socket in `$XDG_RUNTIME_DIR`, so it works over SSH with
`XDG_RUNTIME_DIR` set, in a container with the socket mounted, and under any
compositor - or none.

Output goes to the session's default sink. Point it elsewhere with
`output_device` in the configuration, or check what the default is with
`keyvibes doctor`.

## Sound packs

Installed packs are found, in this order:

1. `$KEYVIBES_PACK_DIR` - explicit override
2. `$XDG_DATA_HOME/keyvibes/packs` (defaults to `~/.local/share/...`)
3. `~/.local/share/keyvibes/packs`
4. `/usr/local/share/keyvibes/packs`
5. `/usr/share/keyvibes/packs`

`make install` puts packs in step 2 for a per-user prefix and step 4 for
`/usr/local`, so neither needs configuration. A prefix outside those (a
temporary one, say) needs the override:

```bash
KEYVIBES_PACK_DIR=/tmp/prefix/share/keyvibes/packs keyvibes pack list
```

List what is installed and choose one by name:

```bash
keyvibes pack list
keyvibes --pack "Holy Panda" run
```

Packs are not shipped pre-built in the repository; build them once with
`cargo xtask packs` (all 24: the three synthesized demo packs and the 21
upstream mechanical-switch packs), or install a package, which includes
them. See [the README](../README.md#sound-packs) for pack provenance.

## Configuration

One TOML file:

```text
$KEYVIBES_CONFIG                if set, this wins
$XDG_CONFIG_HOME/keyvibes/config.toml
~/.config/keyvibes/config.toml
```

```bash
keyvibes config path      # where it is
keyvibes config show      # every setting, and whether it came from the file
keyvibes config init      # write the defaults
```

A missing file is not an error: everything runs at its default. The keys are
`enabled`, `pack`, `output_device`, `volume`, `pitch_variation`,
`gain_variation`, `release_sounds` and `spatial_audio`.

## Running it

```bash
keyvibes                 # same as `keyvibes run`
keyvibes run --duration 5    # stop after five seconds
keyvibes --pack Linear run   # a different pack for this run
```

Or as a user service, so it starts with your session:

```bash
systemctl --user daemon-reload
systemctl --user enable --now keyvibes
systemctl --user status keyvibes
journalctl --user -u keyvibes -f
```

The unit installed by `make install` already points at the right binary for
your prefix; there is nothing to edit.

## Uninstalling

By install method:

```bash
./install.sh --uninstall                 # portable tarball (or PREFIX=/usr)
sudo make uninstall PREFIX=/usr/local    # make install, your PREFIX
sudo apt remove keyvibes                 # deb
sudo dnf remove keyvibes                 # rpm
sudo pacman -R keyvibes                  # PKGBUILD
rm KeyVibes-*-x86_64.AppImage            # AppImage
```

Each removes the binary, the packs it installed, the docs and the user
service - nothing else. The `install.sh` and package uninstallers also remove
the udev rule they put in place; a hand-installed rule under
`/etc/udev/rules.d/` is yours to delete:

```bash
sudo rm /etc/udev/rules.d/70-keyvibes-uaccess.rules
sudo udevadm control --reload-rules
```

Your configuration is left alone, because it is yours:

```bash
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/keyvibes"
```

Group membership is not removed automatically - `sudo gpasswd -d "$USER" input`
if you want it gone, but check nothing else on the machine relied on it.

## Versioning and upgrades

KeyVibes versions are `MAJOR.MINOR.PATCH` (semver), tagged `vX.Y.Z`. The
tagged commit is what CI builds; every artifact in a release carries the same
version and a matching `SHA256SUMS`.

| Change | Version bump |
|---|---|
| bug fix, pack content, docs | PATCH |
| new feature, new config key, new CLI flag (backward compatible) | MINOR |
| `.kvpack` format change, config format change, removed flag | MAJOR |

The `.kvpack` format and the config file format are versioned separately
from the binary: the format number is stored in the file, and a binary that
sees a newer format refuses it with a message rather than guessing. Within a
major version, older packs keep loading.

Upgrades never touch `~/.config/keyvibes` or your own packs under
`~/.local/share/keyvibes/packs`. After an upgrade, `keyvibes doctor` re-checks
the machine; a restart of the user service picks up the new binary:

```bash
systemctl --user restart keyvibes
```

## Troubleshooting

Run `keyvibes doctor` first: it names the problem, why it thinks so, and the
command that fixes it. If it exits 1 on a machine you expect to be fine, its
output is the answer.

| Symptom | Cause |
|---|---|
| `keyvibes` is not found after `make install` | `~/.local/bin` is not on your `PATH` |
| doctor: `cannot read /dev/input/event*` | not in the `input` group (see above) |
| doctor: PipeWire not reachable | session not started: `systemctl --user start pipewire` |
| `pack list` is empty | packs were not built, or a non-standard prefix needs `KEYVIBES_PACK_DIR` |
| no sound, no error | the sink is muted, or `output_device` names a sink that is gone |

More environment detail in [environment.md](environment.md).
