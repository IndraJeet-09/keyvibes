# KeyVibes v0.1.0 — First Linux Release

KeyVibes adds mechanical keyboard sounds to your existing keyboard on Linux, using evdev for input and PipeWire for low-latency audio.

### Features
- 21 imported mechanical-switch soundpacks, plus 3 synthesized demo packs.
- Key-down sounds and optional key-release sounds.
- Low-latency audio mixing with PipeWire.
- Per-user installation without running the engine as root.
- Soundpack management, configuration, diagnostics, and acceptance tests.
- No keylogging, input injection, keyboard grabbing, or telemetry.

### Requirements
- Linux with evdev and a running PipeWire session.
- Rust is not required when installing a prebuilt release.
- Permission to read keyboard input devices; see the installation documentation.

### Installation
1. Download `install.sh` and the matching portable archive from the release assets.
2. Keep both files in the same directory.
3. Run `chmod +x install.sh` and then `./install.sh`.
4. Run `keyvibes doctor` to verify your system setup.

### Notes
- Release sounds are disabled by default.
- Some soundpacks do not contain every key; missing keys remain silent rather than playing another key's sound.
- This is an early release. Real-hardware listening and validation are still recommended.

See the repository README and `docs/` for setup, troubleshooting, soundpack provenance, and licensing details.
