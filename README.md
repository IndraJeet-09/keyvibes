# KeyVibes

**Linux Native Low-Latency Keyboard Sound Engine**

KeyVibes is a production-quality application that makes your keyboard produce realistic mechanical keyboard sounds with extremely low latency. Inspired by Rustyvibes v2, it's built from the ground up for Linux with a focus on real-time performance and compatibility.

## Status

✅ **Phases 0-23 complete** - the engine, the CLI, an install path, and every
phase acceptance command run green on a stock Linux host. `keyvibes doctor`
reports what the host is missing rather than failing mysteriously.

## Features

- **Instant-feeling key sounds** - Sub-millisecond software latency
- **32-voice polyphony** - Smooth overlapping sounds
- **Realistic variation** - Pitch and gain randomization per keystroke
- **Stereo spatialization** - Keys positioned by physical location
- **Linux native** - Direct evdev input, PipeWire audio
- **Desktop agnostic** - Works on X11, Wayland, GNOME, KDE, i3, Hyprland
- **No root required** - Runs as normal user with udev rules

## Architecture

```
Physical Keyboard
  ↓ (Linux kernel evdev)
Linux Input Backend
  ↓ (PhysicalKey events)
Key Engine
  ↓ (PlayCommand)
Lock-free SPSC Queue
  ↓
Real-time Audio Engine (32-voice mixer)
  ↓ (PipeWire RT callback)
Audio Device
```

Everything above the platform line is `kv-core`: `PhysicalKey`, `PlayCommand`,
`Settings` and nothing that names an operating system. The Linux pieces live
in leaf crates, so a Windows port is a matter of writing two more:

```text
kv-core                        shared types, no platform
   ├── kv-input-linux          evdev       (future: kv-input-windows)
   └── kv-audio-pipewire       PipeWire    (future: kv-audio-wasapi)
```

`cargo test -p kv-core` enforces it: the crate may depend on nothing outside
a short portable allow list, may reach no sibling crate, and may contain no
use of a platform API.

## Building

```bash
cargo build --release
```

## Installing

Per-user, no root:

```bash
make install PREFIX="$HOME/.local"
keyvibes doctor
```

That installs the binary, the sound packs, the docs and a systemd user
service. `sudo make install PREFIX=/usr/local` does the same system-wide,
and `make uninstall` takes it all back off. Full requirements, the keyboard
permission, PipeWire, the configuration and pack locations, and
troubleshooting are in [docs/install.md](docs/install.md).

## Running

```bash
keyvibes                 # play keyboard sounds - same as `keyvibes run`
keyvibes doctor          # is this machine ready? and if not, what to do
keyvibes pack list       # which sound packs are installed
keyvibes config show     # every setting, and where each one came from
keyvibes --help          # the whole interface
```

`keyvibes doctor` is the thing to run first on a new machine: it prints one
fixed block of fields and lists any problem with what is wrong, why it thinks
so, and the command that fixes it.

## Sound packs

The `.kvpack` archives are generated, so a fresh clone ships only their
sources under `assets/soundpacks/default-src/`. Build the three packs once
after cloning:

```bash
cargo run --quiet --bin keyvibes -- pack build assets/soundpacks/default-src/pack.toml \
    -o assets/soundpacks/Default.kvpack
cargo run --quiet --bin keyvibes -- pack build assets/soundpacks/default-src/holy-panda.toml \
    -o "assets/soundpacks/Holy Panda.kvpack"
cargo run --quiet --bin keyvibes -- pack build assets/soundpacks/default-src/linear.toml \
    -o assets/soundpacks/Linear.kvpack
```

Pick one by name - no path needed:

```bash
cargo run --quiet --bin keyvibes -- pack list
cargo run --quiet --bin keyvibes -- --pack "Holy Panda" run
```

## Testing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Each phase also has a runnable acceptance command. They print `PASS` / `FAIL`
/ `NOT RUN` / `MANUAL` per check and exit non-zero only on a real failure, so
a check that needs hardware or a PipeWire session this host does not have
never masquerades as a pass:

```bash
cargo test -p kv-core                        # includes the platform audit
cargo run --quiet --bin keyvibes -- cli-test
cargo run --quiet --bin keyvibes -- hotplug-test
cargo run --quiet --bin keyvibes -- audio-recovery-test
cargo run --quiet --bin keyvibes -- idle-test
cargo run --quiet --bin keyvibes -- config-test
cargo run --quiet --bin keyvibes -- pack-test
cargo run --quiet --bin keyvibes -- pack-switch-test
cargo run --quiet --bin keyvibes -- compatibility-test
cargo run --quiet --bin keyvibes -- security-test
cargo run --quiet --bin keyvibes -- doctor
cargo run --quiet --bin keyvibes -- doctor-test
cargo run --quiet --release --bin keyvibes -- benchmark
cargo run --quiet --release --bin keyvibes -- soak-test --duration 30m
cargo run --quiet --release --bin keyvibes -- stress --duration 20
```

`benchmark`, `soak-test` and `stress` need `--release`: their budgets are
real-time budgets, and a debug build cannot meet them. `soak-test --duration`
accepts `45s` / `20m` / `2h` and nothing else - a bare number is a typo you
would otherwise discover thirty minutes later.

See [docs/environment.md](docs/environment.md) for the supported
environment - notably, no display server or compositor is required.

## Development Phases

Phases 0-23 are complete. The gate for each of them is the workspace test
suite plus the acceptance commands under [Testing](#testing) - a phase is not
done until those are green.

## License

Dual-licensed under MIT or Apache 2.0.

## Contributing

This project is currently in early development. Contributions are welcome once the core architecture stabilizes.

## Security

KeyVibes:
- Never logs or transmits keyboard input
- Never requires root privileges
- Only observes keyboard events (does not grab or inject)
- Runs entirely locally

See [docs/linux-permissions.md](docs/linux-permissions.md) for setup.
