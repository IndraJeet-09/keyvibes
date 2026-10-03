# KeyVibes

**Linux Native Low-Latency Keyboard Sound Engine**

KeyVibes is a production-quality application that makes your keyboard produce realistic mechanical keyboard sounds with extremely low latency. Inspired by Rustyvibes v2, it's built from the ground up for Linux with a focus on real-time performance and compatibility.

## Status

🚧 **Phase 0 Complete** - Repository structure initialized

## Features (Planned)

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

## Building

```bash
cargo build --release
```

## Testing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Development Phases

- [x] **Phase 0**: Repository setup
- [ ] **Phase 1**: Audio engine without keyboard
- [ ] **Phase 2**: PipeWire integration
- [ ] **Phase 3**: Evdev input
- [ ] **Phase 4**: Pack loader
- [ ] **Phase 5**: Complete mixer
- [ ] **Phase 6**: Real soundpacks
- [ ] **Phase 7**: Production hardening

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
