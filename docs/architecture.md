# KeyVibes Architecture

## Overview

KeyVibes is a Linux-native keyboard sound engine designed for extremely low latency. The architecture prioritizes real-time performance through careful separation of concerns and strict rules about what operations can occur in different threads.

## Data Flow

```
┌─────────────────────┐
│  Physical Keyboard  │
└──────────┬──────────┘
           │ evdev events
           ▼
┌─────────────────────┐
│  Input Thread       │
│  (kv-input-linux)   │
│                     │
│  - Device discovery │
│  - Hotplug          │
│  - Event parsing    │
│  - PhysicalKey map  │
└──────────┬──────────┘
           │ KeyEvent
           ▼
┌─────────────────────┐
│  Key Engine         │
│  (kv-runtime)       │
│                     │
│  - Variant select   │
│  - Pitch/gain calc  │
│  - Spatialization   │
│  - Pack lookup      │
└──────────┬──────────┘
           │ PlayCommand
           ▼
┌─────────────────────┐
│  Lock-free Queue    │
│  (kv-ring)          │
│                     │
│  - SPSC ring        │
│  - 256 capacity     │
│  - Copy-only        │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│  Audio Thread       │
│  (kv-mixer)         │
│                     │
│  - 32 voices        │
│  - Fixed-point pos  │
│  - Interpolation    │
│  - Mixing           │
│  - Gain ramp        │
│  - Soft limiter     │
└──────────┬──────────┘
           │ Stereo PCM
           ▼
┌─────────────────────┐
│  PipeWire RT        │
│  (kv-audio-pipewire)│
└─────────────────────┘
```

## Core Invariants

### Real-Time Safety

The audio callback is the most critical path. It must NEVER:

1. Allocate memory (`Vec::push`, `String`, `Box::new`, etc.)
2. Block on locks (`Mutex`, `RwLock`)
3. Access the filesystem
4. Log to stdout/files
5. Call non-RT-safe PipeWire APIs
6. Perform expensive DSP (decode, heavy filters)

The audio callback ONLY:

- Dequeues pre-built `PlayCommand`s
- Activates voices with existing pointers
- Renders samples using fixed-point math
- Writes to the output buffer

### Memory Safety

Sound pack PCM data is memory-mapped once at load time. `PlayCommand` contains raw pointers to this mapped region. The pack lifetime management ensures:

1. Packs are mapped before any commands reference them
2. Packs remain mapped while commands are queued
3. Packs remain mapped while voices are active
4. Switching packs uses generation/epoch tracking

### Lock-Free Communication

The SPSC ring buffer connects the input and audio threads:

- **Producer** (input thread): Pushes `PlayCommand`s
- **Consumer** (audio thread): Pops and processes them
- **Overflow policy**: Drop commands, never block
- **Ordering**: `Release`/`Acquire` for correctness, `Relaxed` for owned data

## Module Responsibilities

### kv-core

Defines fundamental types used across the entire system:

- `PhysicalKey`: Hardware-independent key enum (104 keys)
- `KeyEvent`: Input event (key + press/release + timestamp)
- `PlayCommand`: Audio command (samples + playback params)
- `VariationParams`: Pitch/gain randomization
- `KeyGeometry`: Spatial position for panning
- `Settings`: User preferences

### kv-ring

Lock-free SPSC ring buffer optimized for real-time use:

- Power-of-two capacity (default: 256)
- Cache-line separation of producer/consumer indices
- Bitmask for fast modulo
- Copy-only semantics (no `Clone` trait)

### kv-mixer

Real-time audio engine:

- **Voice**: Single sound playback state
  - 32.32 fixed-point position
  - Cubic interpolation for fractional playback
  - Direct-copy fast path when no resampling
  - Per-voice stereo gains
  
- **Mixer**: Voice allocation and mixing
  - 32 simultaneous voices
  - Oldest-first voice stealing
  - Master gain ramping (no zipper noise)
  - Soft limiter (prevent clipping)

- **Interpolation**: 4-point Hermite for quality resampling
- **Limiter**: Tanh-style soft saturation

### kv-pack

Sound pack format and loading:

- `.kvpack` format: versioned, memory-mappable
- `PackHeader`: Magic, version, sample rate, offsets
- `Clip`: Offset + length into PCM data
- `ClipRange`: Multiple variants per key
- Memory mapping with `memmap2`
- Guard samples for interpolation safety

### kv-input-linux

Linux input backend:

- Direct evdev device access (no X11 dependency)
- Device discovery by capabilities
- Hotplug support
- Physical key translation from evdev codes
- SYN_DROPPED handling
- No keyboard grab (observe-only mode)

### kv-audio-pipewire

PipeWire audio backend:

- Native PipeWire stream
- RT_PROCESS flag for real-time callback
- Dynamic quantum/buffer size
- Device change recovery
- Idle timeout and suspend

### kv-runtime

Runtime coordination:

- Connects input → engine → audio
- Pack management and switching
- Settings storage and atomics
- Diagnostics collection

### keyvibes (main binary)

CLI application:

- Argument parsing
- Device listing
- Pack validation
- Self-test suite
- Benchmarking
- Diagnostics display

### xtask

Build-time tooling:

- Sound pack building
- DSP preprocessing pipeline
- Format conversion (WAV/MP3/OGG → i16)
- Transient detection and trimming
- Loudness normalization
- TPDF dithering
- Pack validation

## Key Design Decisions

### Why PhysicalKey instead of keycodes?

Platform independence. The input backend translates evdev codes to `PhysicalKey` once, and the entire engine works with a clean enum. This makes testing easier and enables future Windows/macOS support.

### Why 32.32 fixed-point?

Combines integer sample indexing with fractional position in a single `u64`. No separate position tracking, and the math is simple:

```rust
position += step;
int_index = (position >> 32) as u32;
fraction = (position & 0xFFFF_FFFF) as f32 / (1u64 << 32) as f32;
```

### Why SPSC not MPSC?

Single producer (input thread) and single consumer (audio thread) is all we need. SPSC is simpler, faster, and easier to verify for correctness than MPSC.

### Why copy PlayCommands instead of Arc?

`Arc` involves atomic operations and potential allocations. `PlayCommand` is small (48 bytes), and copying is cheaper than atomic ref-counting in the RT path.

### Why memory-map packs?

- No copying PCM into heap
- OS handles paging
- Minimal startup time
- Can `madvise` for page warming
- Multiple packs can coexist

### Why separate press/release sounds?

Some mechanical switches have distinct release sounds. Supporting them is straightforward with a release flag on `PlayCommand`.

## Performance Targets

- **Software latency**: < 5ms (input event → audio buffer)
- **Voice allocation**: < 1μs
- **Interpolation**: < 100ns per sample
- **Mixer render**: < 1ms per 128-frame block
- **Queue overflow**: Never in normal typing (< 20 keys/sec)

## Compatibility

- **Distributions**: Ubuntu, Fedora, Arch, Debian, openSUSE
- **Display servers**: X11, Wayland
- **Desktops**: GNOME, KDE, i3, Hyprland, Sway
- **Audio**: PipeWire (primary), ALSA (future fallback)
- **Permissions**: udev rules, no root required

## Phase Implementation

Development proceeds in phases to validate each subsystem:

1. **Phase 0**: Repository structure ✅
2. **Phase 1**: Mixer without input (synthetic test)
3. **Phase 2**: PipeWire integration
4. **Phase 3**: Evdev input
5. **Phase 4**: Pack loader
6. **Phase 5**: Complete integration
7. **Phase 6**: Real soundpacks
8. **Phase 7**: Production hardening

Each phase includes tests and benchmarks before proceeding.
