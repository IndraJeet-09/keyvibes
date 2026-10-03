# Linux Input Backend

## Overview

The Linux input backend uses the kernel's **evdev** interface to capture keyboard events across all Linux distributions and desktop environments (X11, Wayland, etc.).

## Architecture

```
Physical Keyboard
      ↓
Linux kernel input subsystem
      ↓
/dev/input/event*
      ↓
evdev crate
      ↓
kv-input-linux
      ↓
PhysicalKey
      ↓
PlayCommand
      ↓
SPSC queue (lock-free)
      ↓
PipeWire RT callback
      ↓
Mixer
```

## Key Design Decisions

### No Hardcoded Device Paths

The backend **never** assumes `/dev/input/event0` is a keyboard. Instead:

1. Enumerate all `/dev/input/event*` devices
2. Query each device's capabilities
3. Apply heuristic: device must support 20+ standard keyboard keys
4. Filter out mice, touchpads, game controllers

### Device Classification

A device is considered a keyboard if it:
- Supports `EV_KEY` events
- Has at least 20 standard letter/number/modifier keys
- Is not primarily a mouse (< 3 mouse buttons or many keyboard keys)

### Multiple Keyboards

Each physical keyboard gets its own:
- `KeyboardDevice` instance
- Reader thread
- Event stream

All keyboards feed the same lock-free SPSC queue.

### Hotplug Support

The hotplug monitor:
- Polls `/dev/input` every 1 second (fallback when udev unavailable)
- Detects added/removed keyboards
- Spawns/terminates reader threads automatically
- **No restart required**

### Event Handling

Linux evdev key events have three values:
- `1` = press → **generate sound**
- `0` = release → recorded, no sound (current implementation)
- `2` = repeat → **ignored** (prevents repeated sounds when holding)

### SYN_DROPPED Handling

When the kernel event queue overflows:
1. `SYN_DROPPED` event arrives
2. Statistics counter increments
3. evdev crate handles resynchronization automatically
4. Normal processing resumes

### Real-time Safety

Input thread → audio thread communication:
- **Lock-free** SPSC ring buffer
- **Zero allocations** per key event
- **Zero mutex locks** per key event
- **Non-blocking** queue push (drops on full)

### Security Model

The backend:
- **Does NOT** grab the keyboard (`EVIOCGRAB`)
- **Does NOT** inject events (`uinput`)
- **Does NOT** require root
- **Does NOT** log key sequences
- Only observes key identity and press/release state

## Permissions

Access to `/dev/input/event*` typically requires membership in the `input` group:

```bash
# Check current groups
groups

# Add user to input group (requires re-login)
sudo usermod -a -G input $USER
```

Alternative: Install a udev rule (not implemented yet).

## Keyboard Mapping

The `mapping` module provides the canonical Linux KeyCode → PhysicalKey mapping:

- `KEY_A` → `PhysicalKey::A`
- `KEY_SPACE` → `PhysicalKey::Space`
- `KEY_LEFTSHIFT` → `PhysicalKey::LeftShift`
- etc.

Mapping is based on **physical position**, not character output.

## Performance

Typical latency budget:
- evdev event received: **< 1ms**
- KeyCode → PhysicalKey mapping: **< 1µs**
- SPSC queue push: **< 100ns**
- **Total input latency: sub-millisecond**

## Testing

### Unit Tests

```bash
cargo test -p kv-input-linux
```

Tests cover:
- KeyCode mapping (all standard keys)
- Event processing (press/release/repeat)
- Device classification heuristics
- Queue saturation behavior

### Manual Testing

```bash
# Discover keyboards
keyvibes input-test

# Verbose mode (shows key events - use with caution)
keyvibes input-test --verbose
```

## Limitations

### Current Implementation

- No release sounds (press-only)
- Dummy PlayCommands (no actual samples until Phase 4)
- Polling-based hotplug (1 second interval)

### Future Improvements

- udev-based hotplug monitoring
- Per-key press/release state tracking
- Configurable key filtering
- USB device serial number tracking
- Better permission diagnostic messages

## Troubleshooting

### No keyboards found

**Symptom:** `InputError::NoKeyboardsFound`

**Causes:**
1. No physical keyboards connected
2. Permission denied (not in `input` group)
3. Keyboard not detected by heuristic (too few keys)

**Fix:**
```bash
# Check permissions
ls -l /dev/input/event*

# Try manual enumeration
evtest
```

### Permission denied

**Symptom:** `InputError::PermissionDenied`

**Fix:**
```bash
sudo usermod -a -G input $USER
# Log out and log back in
```

### Keyboard detected but no sounds

**Causes:**
1. PipeWire not running
2. Queue saturated (commands dropped)
3. Audio device disconnected

**Check statistics:**
```bash
keyvibes input-test
# Look for "commands_dropped" counter
```

### USB reconnect not detected

**Symptom:** Unplug/replug keyboard, no new sounds

**Cause:** Hotplug not enabled

**Fix:**
```rust
backend.enable_hotplug()?;
```

## Platform Compatibility

Tested on:
- Ubuntu 22.04+ (Wayland + X11)
- Fedora 38+ (Wayland + X11)
- Arch Linux (current)
- Debian 12+

Works with:
- GNOME (Wayland/X11)
- KDE Plasma (Wayland/X11)
- i3
- Sway
- Hyprland

The evdev interface is **compositor-independent** and works everywhere Linux input subsystem is present.
