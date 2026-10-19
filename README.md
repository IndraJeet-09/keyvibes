# KeyVibes

KeyVibes is a Linux program that plays a mechanical keyboard sound when you
press a key on any keyboard. It reads key presses from the kernel's evdev
interface, mixes one sample voice per press, and renders the mix to your
session's PipeWire output. It is a background process: it has no window, no
tray icon, and never asks what your desktop is.

Phases 0-25 are complete. The one command that answers "is the product
finished" is [`keyvibes acceptance-test`](#development), which drives the
whole flow from launch to speakers in a single pass.

## Why this exists

A keyboard's sound is part of what typing feels like, and most people cannot
change the switches on the keyboard they already own. A software layer is the
cheapest way to get it: no hardware, no firmware, no driver.

The interesting constraint is latency. A key sound that arrives late is worse
than no key sound at all - you notice the lag before you notice the tone. So
the design is built around one rule: the path from key press to speaker must
never do anything that can block. That rule decides where the locks are, what
the real-time thread is allowed to touch, and why packs are memory-mapped
files rather than parsed structures.

Rustyvibes was the reference point for the idea. KeyVibes is written from
scratch for Linux, with the real-time and format work done explicitly rather
than inherited.

## What it is not

- Not an input remapper. It never modifies, blocks, or injects key events.
- Not a keylogger. Key codes are used to pick a sound and then forgotten; no
  key code is ever written to a log, a file, or a socket.
- Not a desktop application. No X11, no Wayland, no toolkit, no portal -
  enforced by an audit over the dependency graph.
- Not root. It reads `/dev/input` and talks to your user's PipeWire socket,
  which is what a normal user can do.

## Architecture

The runtime path, top to bottom:

```text
                    ┌──────────────┐
                    │  kv-core     │
                    │ PhysicalKey  │
                    │ PlayCommand  │
                    └──────┬───────┘
                           │
             ┌─────────────┴─────────────┐
             │                           │
      ┌──────▼──────┐             ┌──────▼──────┐
      │ Linux Input │             │ Future Win  │
      │   evdev     │             │   Input     │
      └──────┬──────┘             └─────────────┘
             │
             ▼
       SPSC event queue
             │
             ▼
       Runtime / Variant
          Selection
             │
             ▼
       ┌─────────────┐
       │  32 Voice   │
       │   Mixer     │
       └──────┬──────┘
              │
              ▼
       ┌─────────────┐
       │  PipeWire   │
       └──────┬──────┘
              │
              ▼
           Speakers
```

Threading, which is the part that matters:

| Thread | Job | May block? |
|---|---|---|
| input | read evdev, turn keycodes into `PhysicalKey`, select a variant, push a `PlayCommand` | yes |
| PipeWire RT callback | pop commands, mix 32 voices, write samples | **no** - no locks, no allocation, no I/O, no logging |
| engine control | supervise the PipeWire connection, reconnect with backoff | yes |
| main | config, CLI, pack switching, lifecycle | yes |

The two threads that touch each other do so through one lock-free SPSC queue
(`kv-ring`). That queue is the whole boundary between the untrusted world
(keyboards, files, a PipeWire socket that can go away) and the real-time
world.

The offline path, which runs ahead of all of the above:

```text
WAV
 ↓  Analysis
 ↓  DSP
 ↓  Dither
 ↓  KVPack          (kv-pack build)
 ↓  Immutable mmap
 ↓  Runtime         (lookup is index arithmetic, not parsing)
```

A `.kvpack` is built once, validated once, then memory-mapped. At runtime a
key press is an offset into a mapped file - no deserialization, no
allocation, no lock.

## Supported environments

The whole platform surface is two interfaces: evdev for input, PipeWire for
output. See [docs/environment.md](docs/environment.md) for the full statement
and how it is enforced.

- Linux with evdev (`CONFIG_INPUT_EVDEV`). Tested on x86-64.
- A running PipeWire session owned by the logged-in user.
- Any logind or TTY session: a graphical login, a TTY, a systemd user
  service, or a container with `/dev/input` passed through.
- 48 kHz is the reference rate; `--rate` takes anything PipeWire exposes.

Explicitly not required and never read: `DISPLAY`, `WAYLAND_DISPLAY`,
`XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP`, `DESKTOP_SESSION` and friends.
"It works on X11" and "it works on Wayland" have the same answer: the engine
never asks.

No display server, toolkit, portal, or compositor crate may enter the
dependency graph, directly or transitively. `keyvibes compatibility-test`
checks both the `Cargo.toml` files and `cargo tree`.

## Installation

### Option A: AppImage (recommended download)

The [GitHub releases page](https://github.com/IndraJeet-09/keyvibes/releases)
publishes a single self-contained `KeyVibes-<version>-x86_64.AppImage`. It
contains the headless engine and all 21 sound packs; no install step, no root.

```bash
chmod +x KeyVibes-*-x86_64.AppImage
./KeyVibes-*-x86_64.AppImage doctor     # is this machine ready?
./KeyVibes-*-x86_64.AppImage run
```

The AppImage does **not** include the GTK desktop app (`keyvibes-ui`) - the
engine and the UI have different packaging needs, and bundling GTK into an
AppImage is not something this project will pretend to support. Use a distro
package or `make install` if you want the UI. PipeWire must be running; the
AppImage ships the client library, not the server. See
[docs/install.md](docs/install.md#appimage) for details and limitations.

### Option B: distro package or portable tarball

Releases also ship a `.deb` (Debian/Ubuntu), an RPM spec, a `PKGBUILD`
(Arch), and a portable `tar.gz` with an `install.sh`. The `.deb` and RPM
include the engine, the UI, the packs, the udev rule, and maintainer scripts
that reload udev on install. Full instructions, including upgrade and
uninstall, are in [docs/install.md](docs/install.md).

### Option C: build from source

```bash
git clone https://github.com/IndraJeet-09/keyvibes
cd keyvibes
cargo build --release
```

Rust 1.75 or newer. The PipeWire bindings compile a small C shim, so you need
a C toolchain and the PipeWire headers: `libpipewire-0.3-dev` on
Debian/Ubuntu, `pipewire-devel` on Fedora, `pipewire` on Arch.

Then grant keyboard access (see [Permissions](#permissions) - one-time, and
it does not need to happen before you build), build the sound packs, and
install:

```bash
make install PREFIX="$HOME/.local"    # per-user, no root
# or
sudo make install PREFIX=/usr/local   # system-wide
```

`make uninstall` removes exactly what install added and leaves your
configuration alone. `make help` lists the targets. Packaging into a staging
directory: `make install DESTDIR=/tmp/stage PREFIX=/usr`. Build all packs
with `cargo xtask packs`. Full detail - runtime dependencies, the systemd
unit, the optional udev rule, and troubleshooting - is in
[docs/install.md](docs/install.md).

### Check it

```bash
keyvibes doctor
```

`doctor` prints a fixed block of fields (version, OS, kernel, input, audio,
packs, runtime configuration) and then any problems, each with what is wrong,
why it thinks so, and the command that fixes it. It exits 1 when something
the engine needs is missing. On a machine that is not ready, exit 1 is the
tool working correctly.

## Permissions

`/dev/input/event*` nodes are `root:input 0660`. KeyVibes opens them as an
ordinary user, so one of these has to be true:

**You are in the `input` group** - the usual setup:

```bash
sudo usermod -aG input "$USER"
```

Log out and back in, then confirm with `keyvibes doctor`.

**Or a logind ACL grants your seat access** - no group membership, but only
for a local session. The optional rule matches keyboards only
(`ID_INPUT_KEYBOARD`), not mice or other input:

```bash
sudo install -Dm644 dist/keyvibes-udev.rules \
    /etc/udev/rules.d/70-keyvibes-uaccess.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Distro packages install this rule automatically. Neither option is needed at
runtime: once the permission exists, everything else happens as a normal
user. KeyVibes does not grab the device, does not grab the keyboard, and
does not inject events - it observes only.

More detail in [docs/linux-permissions.md](docs/linux-permissions.md).

## Usage

```bash
keyvibes                    # play keyboard sounds (same as `keyvibes run`)
keyvibes doctor              # is this machine ready? if not, what to do
keyvibes pack list           # which sound packs are installed
keyvibes config show         # every setting, and where each one came from
keyvibes --help              # the whole interface
```

Common variations:

```bash
keyvibes --pack "Holy Panda (kbsim)" run     # choose a pack for this run
keyvibes run --duration 10           # stop after ten seconds
keyvibes run --simulate --duration 5 # no keyboard needed: scripted events
keyvibes run --rate 44100            # a different output rate
keyvibes -v run                      # verbose logging
```

Running it as a service so it starts with your session:

```bash
systemctl --user daemon-reload
systemctl --user enable --now keyvibes
journalctl --user -u keyvibes -f
```

Every command also has a `*-test` counterpart; see
[Development](#development).

## Sound packs

A pack is a `.kvpack`: a manifest, a table of keys, and per-key sample
variants, in a single file that is validated once and then memory-mapped.
Sample rate, channel count, and format are normalised at build time, so the
runtime never converts anything.

**Provenance.** Twenty-one real-switch packs - linear, tactile, and
clicky, most with release sounds - are converted from the pinned
[rustyvibes-v2](https://github.com/KunalBagaria/rustyvibes-v2) catalog (audio
originating from [Mechvibes](https://github.com/hainguyents13/mechvibes) and
[kbsim](https://github.com/tplai/kbsim), all MIT) by
[`scripts/import_upstream_soundpacks.py`](scripts/import_upstream_soundpacks.py).
The converted sources live under `assets/soundpacks/upstream-src/` with their
per-pack `coverage.json`; attribution and license texts are in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and
[docs/soundpacks.md](docs/soundpacks.md). `keyvibes
soundpack-acceptance-test` verifies the whole family in one pass.

**Coverage.** The kbsim packs carry a clip for all 104 physical keys; the
Mechvibes packs lack `PrintScreen`/`Pause` (and one lacks `F8`); their
coverage is recorded honestly in `pack list -v` and the importer's
`coverage.json`. A key the selected pack does not carry makes no sound: the
press is counted (`Presses with no pack sound` in the stats, `coverage N/104`
in `pack list -v`, `pack inspect`, and `doctor`) and is never served another
key's clip. Pack discovery skips test fixtures (anything under a
`tests/fixtures` directory, or named `golden*`), so a checkout's golden test
packs can never become the default; `--pack <path>` still opens one
deliberately.

Listing and choosing:

```bash
keyvibes pack list
keyvibes pack inspect "assets/soundpacks/Alpaca.kvpack"
keyvibes pack validate "assets/soundpacks/Alpaca.kvpack"
keyvibes --pack "Holy Panda (kbsim)" run
```

Packs are searched in this order:

1. `$KEYVIBES_PACK_DIR`
2. `$XDG_DATA_HOME/keyvibes/packs`
3. `~/.local/share/keyvibes/packs`
4. `/usr/local/share/keyvibes/packs`
5. `/usr/share/keyvibes/packs`
6. `assets/soundpacks` (relative to the current directory)

A prefix outside that list needs `KEYVIBES_PACK_DIR` set.

The format is specified in
[docs/kvpack-format.md](docs/kvpack-format.md), and the builder is documented
in [docs/kvpack-builder.md](docs/kvpack-builder.md).

## Configuration

One TOML file:

```text
$KEYVIBES_CONFIG                     if set, this wins
$XDG_CONFIG_HOME/keyvibes/config.toml
~/.config/keyvibes/config.toml
```

```bash
keyvibes config path       # where it is
keyvibes config show       # every setting, and whether it came from the file
keyvibes config init       # write the defaults
keyvibes config init --force
```

A missing file is not an error - everything runs at its default:

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | start the engine |
| `pack` | unset | pack name; unset means "first installed pack" |
| `output_device` | unset | PipeWire sink name; unset means the default sink |
| `volume` | `0.7` | master gain, 0.0 to 1.0 |
| `pitch_variation` | `true` | randomise pitch per press |
| `gain_variation` | `true` | randomise gain per press |
| `release_sounds` | `false` | play a sound on key release |
| `spatial_audio` | `true` | pan keys by their physical position |
| `version` | `1` | file format version |

Precedence, highest first: `--pack` flag, `$KEYVIBES_PACK`, the `pack` key in
the file, first installed pack.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `keyvibes doctor` exits 1 | read what it printed - each entry has `why:` and `fix:` |
| `No keyboards found.` | not in the `input` group, or no keyboard attached |
| `keyvibes` is not found after `make install` | `~/.local/bin` is not on your `PATH` |
| PipeWire not reachable | `systemctl --user start pipewire` |
| `pack list` is empty | packs not built, or a non-standard prefix needs `KEYVIBES_PACK_DIR` |
| sound plays but is too quiet/loud | `volume` in the config file, or `keyvibes config show` |
| an XRUN appears under load | see [docs/performance.md](docs/performance.md) |

Anything that looks like a crash is worth running under `keyvibes doctor`
first: it checks the same things the engine checks before it starts.

## Development

### Layout

```text
crates/kv-core            shared types; no platform knowledge (audited)
crates/kv-ring            lock-free SPSC queue
crates/kv-mixer           32-voice mixer, interpolation, limiter
crates/kv-pack            KVPack format: parse, validate, mmap, build
crates/kv-input-linux     evdev: discovery, hotplug, keycode translation
crates/kv-audio-pipewire  PipeWire stream, engine supervisor, reconnect
crates/kv-runtime         wiring: input -> mixer -> audio, pack switching
crates/keyvibes           the binary: CLI, config, doctor, acceptance
xtask                     build helpers
assets/soundpacks         pack sources (`.kvpack` files are generated)
site/                     static landing page: downloads, pack catalog, previews
docs/                     format, environment, permissions, performance
dist/                     systemd unit, udev rule
```

### The gate

Every change runs the same three commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Then the acceptance command for whatever you touched. They print `PASS` /
`FAIL` / `NOT RUN` / `MANUAL` per check and exit non-zero only on a real
failure, so a check that needs hardware this host does not have never
masquerades as a pass.

Start with the whole-product check - one process, the full flow from launch
to speakers, twelve named checks:

```bash
cargo run --quiet --bin keyvibes -- acceptance-test
```

It covers KVPack loading, input discovery, the audio backend, event to
command, variant selection, voice allocation, audio rendering,
configuration, pack switching, recovery, RT safety, and privacy and
security. A single failure stops the run, and the project is not finished
until it is green.

The per-area ones:

```bash
cargo test -p kv-core                        # platform audit
cargo run --quiet --bin keyvibes -- cli-test
cargo run --quiet --bin keyvibes -- doctor
cargo run --quiet --bin keyvibes -- doctor-test
cargo run --quiet --bin keyvibes -- config-test
cargo run --quiet --bin keyvibes -- pack-test
cargo run --quiet --bin keyvibes -- pack-switch-test
cargo run --quiet --bin keyvibes -- hotplug-test
cargo run --quiet --bin keyvibes -- audio-recovery-test
cargo run --quiet --bin keyvibes -- idle-test
cargo run --quiet --bin keyvibes -- compatibility-test
cargo run --quiet --bin keyvibes -- security-test
cargo run --quiet --release --bin keyvibes -- benchmark
cargo run --quiet --release --bin keyvibes -- stress --duration 20
cargo run --quiet --release --bin keyvibes -- soak-test --duration 30m
```

`benchmark`, `stress` and `soak-test` need `--release`: their budgets are
real-time budgets and a debug build cannot meet them. `soak-test --duration`
accepts `45s` / `20m` / `2h` and nothing else - a bare number is a typo you
would otherwise find out thirty minutes later.

### The website

`site/` is static HTML, CSS and JavaScript - no build step, no framework.
Two files in it are generated and are committed alongside the pages:

```bash
cargo xtask dist    # writes site/releases.json: version + download links
cargo xtask site    # writes site/packs.json + site/assets/audio/*.wav
```

`cargo xtask site` reads the installed `.kvpack` files (the repository's
`assets/soundpacks` first, then the user and system pack directories) and
emits one catalog entry per pack - name, category, coverage, release
support, author, licence - plus one preview strip: the pack's own PCM for
26 press keys, with the slot offsets in seconds. A key the pack does not
cover is left out of the strip, so the page previews it as silent exactly
as the app plays it.

Preview with any static server, for example:

```bash
python3 -m http.server 8077 --directory site
```

CI's `site` job re-checks that every local link resolves and that the
catalog and its preview audio agree. The desktop surface, the website's
data flow and the verification commands are described in
[docs/ui-architecture.md](docs/ui-architecture.md).

### Architecture for contributors

`kv-core` is the one crate everything else shares. It defines `PhysicalKey`
(a keyboard-independent key identity), `PlayCommand` (which key, which
variant, what pitch and gain), `SoundSource` (how a command becomes
samples), and `Settings`. It may depend on nothing but a short portable
allow list, may reach no sibling crate, and may contain no platform API.
That is enforced by `crates/kv-core/tests/platform_audit.rs`, which parses
the `Cargo.toml` files and greps the source. Adding `std::os::unix` or
`libc` to `kv-core` fails the test; that is the point.

Everything below `kv-core` is a leaf with one job:

- `kv-input-linux` turns evdev events into `PhysicalKey`s. It owns
  discovery, hotplug, `SYN_DROPPED` recovery, repeat suppression, and the
  decision to never grab a device.
- `kv-mixer` owns the voices. It is the only crate the RT callback reads
  sample data from.
- `kv-ring` owns the queue between input and audio. Push and pop are
  wait-free.
- `kv-pack` owns the file format. Two representations: a parsed `Pack`
  (owned, mutable, build-time) and a memory-mapped `PackRuntime` (read-only,
  zero-copy, RT-safe lookup).
- `kv-audio-pipewire` owns the connection. `stream` is the RT side,
  `engine` supervises and reconnects with bounded backoff, `lifecycle`
  holds the observable state machine.
- `kv-runtime` wires them together and owns the one subtle piece of state:
  the swap between an old pack and a new one while the RT thread is
  rendering from the old one.

Where to make a change:

| You want to... | Start in |
|---|---|
| add a command or flag | `crates/keyvibes/src/cli.rs` |
| change a config key | `crates/keyvibes/src/config.rs` (and `config-test`) |
| change what `doctor` reports | `crates/keyvibes/src/doctor.rs` (and `doctor-test`) |
| support a new key | `crates/kv-core/src/geometry.rs`, `kv-input-linux` |
| change how a sample is chosen | `kv-runtime` variant selection |
| change the mix | `kv-mixer` |
| change the format | `kv-pack` (bump the format, `kvpack-format.md`) |
| change reconnect behaviour | `kv-audio-pipewire/src/lifecycle.rs` |

Rules that are not negotiable, because breaking one fails a test:

1. The RT callback does not allocate, lock, log, or touch the filesystem.
2. The input path is lock-free end to end: evdev read, variant selection,
   queue push.
3. No crate outside the leaves may name a platform API.
4. A pack is never freed while the RT thread can still be rendering from it.
5. A check that cannot run on this host reports `NOT RUN`, never `PASS`.

### Conventions

- MSRV 1.75. `cargo clippy -D warnings` is the bar, and the workspace denies
  a handful of lints by name in `Cargo.toml`.
- Test modules stay at the bottom of the file (`items_after_test_module`).
- Acceptance checks are built from `keyvibes/src/accept.rs`, so every
  `*-test` command reports the same way and exits the same way.
- Commands the CLI does not have are still referenced by some tests' command
  allow lists; `crates/keyvibes/src/security_test.rs` holds them.

## Security

KeyVibes observes keyboard events to decide which sound to play. It does not
store them, log them, transmit them, or act on them.

- No key logging, no text capture - only the keycode is read, and only long
  enough to pick a sample.
- No network access - the binary makes no socket connections of any kind.
- No keyboard injection, no keyboard grabbing - devices are opened read-only
  and never taken away from the desktop.
- Safe pack parsing - `.kvpack` files are validated before use, paths cannot
  escape the pack, and malformed input is rejected rather than trusted.
- No root - nothing in the codebase requests elevation.

`keyvibes security-test` audits this by searching the source for the
relevant patterns and their absence, and fails if any of them disappears.

## License

Dual-licensed under MIT or Apache 2.0.
