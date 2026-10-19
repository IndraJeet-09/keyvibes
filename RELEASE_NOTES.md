# KeyVibes v0.1.1 — Real Soundpacks and a Rebuilt Desktop App

KeyVibes adds mechanical keyboard sounds to your existing keyboard on Linux, using evdev for input and PipeWire for low-latency audio.

This release replaces the placeholder soundpacks with the real catalog, and rebuilds the desktop app on top of it.

### Changes
- **The 21 imported mechanical-switch soundpacks are now the whole catalog.** The three synthesized demo packs (`Default`, `Holy Panda`, `Linear`) are gone; every pack you can hear is a recording. `Holy Panda` is now `Holy Panda (kbsim)`, the kbsim recording, and nothing plays a placeholder.
- **The desktop app can play a pack.** `keyvibes-ui` now previews a soundpack as a short burst of typing, built from that pack's own keypress clips and its own key-release sounds, so you can hear a switch before you install it.
- **A System page in the desktop app.** Diagnostics, logs, and the suggested fix for what the engine is unhappy about are one click away, and the app says plainly when the engine is not ready yet.
- **Restore sound defaults** puts the volume and the three variation toggles back to the shipped values, from the same source the engine uses, so it cannot drift from a fresh install.
- **The website was rebuilt.** A larger hero, a soundpack browser that lists every installed pack with its real metadata, and a play button on each pack that plays a three-to-four-second burst of that pack's own typing.

### Requirements
- Linux with evdev and a running PipeWire session.
- Rust is not required when installing a prebuilt release.
- Permission to read keyboard input devices; see the installation documentation.

### Installation
1. Download `install.sh` and the matching portable archive from the release assets.
2. Keep both files in the same directory.
3. Run `chmod +x install.sh` and then `./install.sh`.
4. Run `keyvibes doctor` to verify your system setup.

Upgrading from v0.1.0: the three synthesized packs are removed by the upgrade. Any pack you installed yourself is left alone.

### Notes
- Release sounds are disabled by default.
- Some soundpacks do not contain every key; missing keys remain silent rather than playing another key's sound.
- This is an early release. Real-hardware listening and validation are still recommended.

See the repository README and `docs/` for setup, troubleshooting, soundpack provenance, and licensing details.
