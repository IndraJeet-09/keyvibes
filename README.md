# KeyVibes

Mechanical keyboard sounds on Linux. KeyVibes reads key presses from the
kernel's evdev interface, mixes one sample voice per press, and plays them
through your user's PipeWire session. No window, no tray, no root, no
keylogging, no input injection — it observes keystrokes only to pick a sound.

## Install

### AppImage (recommended)

Download `KeyVibes-<version>-x86_64.AppImage` from the
[releases page](https://github.com/IndraJeet-09/keyvibes/releases):

```bash
chmod +x KeyVibes-*-x86_64.AppImage
./KeyVibes-*-x86_64.AppImage doctor   # is this machine ready?
./KeyVibes-*-x86_64.AppImage run
```

Ships the engine and all 21 sound packs. PipeWire must already be running.
The GTK desktop app is not bundled — use a distro package or `make install`
for the UI. Details: [docs/install.md](docs/install.md).

### Distro package or portable tarball

Releases also provide `.deb`, RPM, `PKGBUILD`, and a portable `tar.gz` with
`install.sh`. Upgrade and uninstall instructions:
[docs/install.md](docs/install.md).

### From source

```bash
git clone https://github.com/IndraJeet-09/keyvibes
cd keyvibes
cargo build --release
```

Requires Rust 1.75+, a C toolchain, and PipeWire headers
(`libpipewire-0.3-dev` on Debian/Ubuntu). Then grant input access (below),
build packs, and install:

```bash
make install PREFIX="$HOME/.local"    # per-user, no root
# or
sudo make install PREFIX=/usr/local
```

## Permissions

`/dev/input/event*` is `root:input 0660`. Join the group once:

```bash
sudo usermod -aG input "$USER"
```

Log out and back in, then verify with `keyvibes doctor`. Alternatively, the
optional udev rule in `dist/keyvibes-udev.rules` grants keyboards-only access
via logind ACL — distro packages install it automatically. Once permission
exists, everything runs as a normal user.

## Usage

```bash
keyvibes                              # play keyboard sounds
keyvibes doctor                       # diagnose this machine
keyvibes pack list                    # installed sound packs
keyvibes config show                  # every setting and its source
keyvibes --pack "Holy Panda (kbsim)" run
keyvibes run --duration 10            # stop after ten seconds
keyvibes run --simulate --duration 5  # no keyboard needed
systemctl --user enable --now keyvibes  # start with your session
```

## Sound packs

A pack is a `.kvpack`: manifest + per-key sample variants, validated once and
memory-mapped. Lookup at runtime is index arithmetic — no parsing, no
allocation on the audio thread.

21 real-switch packs (linear, tactile, clicky) are converted from the pinned
[rustyvibes-v2](https://github.com/KunalBagaria/rustyvibes-v2) catalog
(Mechvibes and kbsim audio, all MIT). Attribution:
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Format:
[docs/kvpack-format.md](docs/kvpack-format.md).

## Configuration

One TOML file (`~/.config/keyvibes/config.toml`, or `$KEYVIBES_CONFIG`).
Missing file is fine — defaults apply:

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | start the engine |
| `pack` | unset | pack name; unset = first installed |
| `output_device` | unset | PipeWire sink; unset = default |
| `volume` | `0.7` | master gain (0.0–1.0) |
| `pitch_variation` | `true` | randomise pitch per press |
| `gain_variation` | `true` | randomise gain per press |
| `release_sounds` | `false` | sound on key release |
| `spatial_audio` | `true` | pan keys by physical position |

```bash
keyvibes config path    # where it is
keyvibes config init    # write the defaults
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| `doctor` exits 1 | read what it printed — each entry has `why:` and `fix:` |
| `No keyboards found.` | not in the `input` group, or no keyboard attached |
| PipeWire not reachable | `systemctl --user start pipewire` |
| `pack list` is empty | run `cargo xtask packs`, or set `KEYVIBES_PACK_DIR` |
| sound too quiet/loud | `volume` in the config file |

More: [docs/install.md](docs/install.md), [docs/performance.md](docs/performance.md).

## Development

```text
crates/kv-core            shared types (platform-audited)
crates/kv-ring            lock-free SPSC queue
crates/kv-mixer           32-voice mixer, interpolation, limiter
crates/kv-pack            KVPack format: parse, validate, mmap, build
crates/kv-input-linux     evdev: discovery, hotplug, keycode translation
crates/kv-audio-pipewire  PipeWire stream, supervisor, reconnect
crates/kv-runtime         wiring: input → mixer → audio
crates/keyvibes           the binary: CLI, config, doctor, acceptance
crates/keyvibes-ui        GTK desktop app
site/                     static landing page (deployed on Vercel)
docs/                     format, environment, permissions, performance
```

The gate every change runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run --quiet --bin keyvibes -- acceptance-test
```

RT-safety rules (enforced by tests): the audio callback never allocates,
locks, logs, or touches the filesystem; the input path is lock-free end to
end; packs are never freed while the RT thread may still render from them.

### The website

`site/` is static HTML/CSS/JS — no build step. `site/packs.json` and
`site/assets/audio/*.wav` are generated and committed:

```bash
cargo xtask packs                     # build .kvpack files from sources
cargo xtask site                      # regenerate catalog + preview audio
cargo xtask dist                      # refresh site/releases.json
```

Preview locally:

```bash
python3 -m http.server 8077 --directory site
```

Deploy: import the GitHub repo on [Vercel](https://vercel.com) with **Root
Directory** set to `site` (no build command, output = root). The committed
`site/vercel.json` supplies cache headers.

## Security

- No keylogging — keycodes pick a sample and are never logged, stored, or sent.
- No network access from the binary.
- No keyboard grabbing or injection — devices are opened read-only.
- Packs are validated before use; malformed input is rejected.
- No root required.

`keyvibes security-test` audits the source for these properties.

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.

Sound-pack audio is third-party (MIT); see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for attribution.
