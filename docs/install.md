# Installing KeyVibes

KeyVibes runs as a normal user. It never asks for root at runtime, never
writes outside your configuration and your pack directory, and never needs a
display server. The only privileged thing you may have to do once is add
yourself to the `input` group so it can read a keyboard.

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
| Display server | none - KeyVibes is a background service and reads/writes no windows |
| Root | never, once the `input` permission below is sorted |

Check the machine before you install anything:

```bash
keyvibes doctor
```

It names what is missing and how to fix it, and exits 1 when something the
engine needs is absent.

## Building a release binary

```bash
cargo build --release
./target/release/keyvibes --version
```

The release profile is not optional in spirit: the real-time budgets in
`keyvibes stress`, `benchmark` and `soak-test` are release budgets, and a
debug build cannot meet them.

## Installing

### From this checkout, per user (recommended, no root)

```bash
make install PREFIX="$HOME/.local"
```

This puts the binary in `~/.local/bin`, the packs in
`~/.local/share/keyvibes/packs`, the docs in `~/.local/share/doc/keyvibes`
and a systemd user unit in `~/.local/share/systemd/user`.

Make sure `~/.local/bin` is on your `PATH`, then check it:

```bash
keyvibes doctor
keyvibes pack list
```

### From this checkout, system-wide

```bash
sudo make install PREFIX=/usr/local
```

Same layout under `/usr/local`. Packages built with `checkinstall` or for a
distribution can set `DESTDIR`:

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
for a local graphical/TTY session. The optional udev rule does it:

```bash
sudo install -Dm644 dist/keyvibes-udev.rules \
    /etc/udev/rules.d/70-keyvibes-uaccess.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Neither needs anything at runtime: once the permission exists, KeyVibes opens
the device as an ordinary user. More detail in
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

Packs are not shipped pre-built in the repository; a fresh checkout needs
them built once, as [the README](../README.md#sound-packs) describes.

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

```bash
sudo make uninstall PREFIX=/usr/local     # or the PREFIX you installed to
```

That removes the binary, the packs, the docs and the user service. It leaves
your configuration alone, because it is yours:

```bash
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/keyvibes"
```

If you added the optional udev rule, remove that too:

```bash
sudo rm /etc/udev/rules.d/70-keyvibes-uaccess.rules
sudo udevadm control --reload-rules
```

Group membership is not removed automatically - `sudo gpasswd -d "$USER" input`
if you want it gone, but check nothing else on the machine relied on it.

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
