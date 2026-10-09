# Performance Measurement and Optimization

This document describes how KeyVibes measures and optimizes for low latency.

## Latency Components

Total latency from physical key press to audible sound consists of:

```
Physical keystroke
  ↓
Hardware scan (USB polling, etc.)         ~1-8ms (hardware dependent)
  ↓
Kernel evdev delivery                     ~0.1-1ms
  ↓
KeyVibes input processing                 TARGET: <0.5ms
  ↓
Command queuing                           ~0.01ms
  ↓
Audio callback processing                 TARGET: <0.2ms
  ↓
PipeWire/device buffering                 ~1-5ms (quantum dependent)
  ↓
Speaker/DAC                               ~0.5-5ms (hardware dependent)
```

**Software-controlled latency** is the portion KeyVibes can optimize: input processing + audio callback + partial buffering.

**Target: <5ms software onset latency**

## Instrumentation

KeyVibes measures latency at several points:

### Event Timestamps

```rust
pub struct KeyEvent {
    pub timestamp_ns: u64,  // From evdev event
    // ...
}
```

The evdev event carries a kernel timestamp of when the key was pressed.

### Measurement Points

1. **t0**: Evdev event received (from kernel timestamp)
2. **t1**: PlayCommand pushed to queue
3. **t2**: Audio callback pops command
4. **t3**: First sample rendered

### Reported Metrics

```
input_to_queue:    t1 - t0   (key engine latency)
queue_to_audio:    t2 - t1   (queue wait time)
audio_onset:       t3 - t2   (render startup)
total_software:    t3 - t0   (end-to-end software)
```

Run with diagnostics:

```bash
keyvibes --diagnostics
```

## Benchmarking

KeyVibes includes Criterion benchmarks for critical paths:

```bash
cargo bench
```

Benchmarks measure:

- SPSC push/pop operations
- Voice allocation
- Sample interpolation
- Full mixer render (1/8/16/32 voices)
- Limiter processing

## Optimization Guidelines

### Input Thread

Keep the key-to-command path fast:

- ✅ Array lookup for key → clip mapping
- ✅ Pre-computed geometry positions
- ✅ Fast RNG for variation
- ❌ No HashMap lookups in hot path
- ❌ No string operations
- ❌ No file I/O

### Audio Thread

The mixer render must complete within the buffer quantum:

For a 128-frame buffer at 48kHz:
```
128 / 48000 = 2.67ms budget
```

With 32 voices active, per-voice budget:
```
2.67ms / 32 = 83μs per voice
```

**Critical optimizations:**

1. **Direct-copy fast path**: When no resampling/pitch, copy samples directly
2. **Fixed-point position**: No float→int conversion per sample
3. **Branchless interpolation**: Polynomial evaluation, no conditionals
4. **Cache-friendly voice layout**: Contiguous array, no pointer chasing

### Voice Stealing

Voice stealing uses generation counters, not amplitude scanning:

```rust
// Fast: O(n) scan for minimum
voices.iter_mut().min_by_key(|v| v.generation)

// Avoid: O(n) with per-voice sample access
voices.iter_mut().min_by_key(|v| read_amplitude(v))
```

## Profiling

### CPU Profiling

Use `perf` to profile the audio callback:

```bash
# Record with call graph
perf record -F 999 -g -- keyvibes

# Analyze
perf report
```

Focus on:
- `Mixer::render_block`
- `Voice::render_sample`
- `cubic_interpolate`

### Real-Time Analysis

Check for RT violations:

```bash
# Monitor for allocation/syscalls in audio thread
# (Requires RT analysis tools)
```

## Targets by Phase

### Phase 1 (Mixer without input)
- Render 128 samples: <100μs
- 32-voice mix: <300μs

### Phase 2 (PipeWire)
- Buffer delivery: <5ms
- No xruns under load

### Phase 3 (Input integrated)
- Event→command: <500μs
- Queue push: <1μs

### Phase 5 (Complete)
- Total software latency: <5ms
- Can type 20 keys/sec without drops

### Phase 7 (Production)
- Latency P50: <3ms
- Latency P99: <8ms
- Zero drops at 10 keys/sec
- <1% drops at 20 keys/sec

## Common Performance Issues

### Symptom: High P99 latency

**Cause**: Occasional slow frame due to cache miss, page fault, or OS scheduling.

**Solution**:
- Use `mlock` on audio buffers (if permitted)
- `madvise(WILLNEED)` on sound pack
- Increase real-time priority (if permitted)

### Symptom: Xruns (buffer underruns)

**Cause**: Audio thread missed deadline.

**Solution**:
- Profile the render loop
- Reduce buffer size only if headroom exists
- Check for allocations (should be zero)

### Symptom: Command drops

**Cause**: Queue full because audio thread is slow or blocked.

**Solution**:
- Check audio thread latency
- Verify audio thread is running
- Increase queue size (if drops are rare)

## Testing Latency

### Loopback Measurement

For precise measurement:

1. Generate impulse on key press
2. Record audio output with low-latency interface
3. Measure time delta between key press signal and audio output

### Subjective Testing

Most users can perceive latency above 10-15ms. Target is well below this threshold.

### Software Latency Gate (`keyvibes stress`)

```bash
keyvibes stress --duration 60
```

Drives a deterministic key-load generator (default ~80 keys/s, chords
included, seeded so runs are comparable) through a real PipeWire stream for
the given duration and exits non-zero unless **every** real-time check passes.

**What it measures — software / audio-engine only:**

| Stage | Statistic |
| --- | --- |
| event received → command enqueued | `command_latency_{p50,p95,p99,max}` |
| command enqueued → dequeued by the callback | `queue_latency_{p50,p95,p99,max}` |
| callback wall time vs. quantum and safety budget | `callback_{p50,p95,p99,max}` |

The report also attributes the single slowest callback to a section
(`dequeue` / `drain` / `render`), so an over-budget callback says which stage
paid rather than only that it happened.

Latency histograms resolve 1 µs below 2.048 ms and 32 µs above that, up to
67.552 ms; anything longer lands in an overflow bucket.

**What it deliberately does not measure:** DAC conversion, amplifier,
transducer, or acoustic propagation time. No number printed by
`keyvibes stress` means "time from key press to sound at the ear". Add the
hardware stages on top of the software path only.

**Failure conditions — any one of these fails the gate:**

- PipeWire XRUN count (the `ERR` column of `pw-top`) is unavailable or non-zero
- any audio-callback deadline miss, late callback, or safety-budget overrun
- any producer underrun (`no_buffer` / `empty_buffer`)
- any PipeWire stream error or reconnect
- zero frames rendered
- callback maximum ≥ 50% of the measured quantum (the safety budget)
- any dropped command, or no input-latency samples recorded

**Real-time safety proofs.** The load test proves timing under load; two
structural tests prove the properties an idle machine can never exercise:

- `crates/kv-mixer/tests/rt_safety.rs` wraps 10 000 real
  `Mixer::trigger` + `render_block` pairs in a counting
  `#[global_allocator]` and asserts the allocation counter never moves
  (including a control measurement that proves the counter counts).
- `crates/kv-audio-pipewire/tests/rt_safety.rs` extracts `process_callback`
  from `stream.rs` and fails on any lock, logging call, panic path, blocking
  API, or I/O, then repeats the audit over every file the audio thread
  reaches, asserting the single `eprintln!` in `stream.rs` stays inside the
  control-plane-only helper.

## Future Optimizations

- SIMD for batch mixing
- Lock-free pack switching
- Pre-warmed voice pool
- Adaptive buffer sizing

## Benchmarking Best Practices

1. **Consistent environment**: Disable CPU frequency scaling
2. **Isolated core**: Consider isolating audio thread CPU
3. **Realistic load**: Benchmark with 8-16 active voices (typical typing)
4. **Worst case**: Benchmark with 32 voices (full polyphony)
5. **Statistical rigor**: Run multiple iterations, report P50/P99

Example benchmark run:

```bash
# Set performance governor
sudo cpupower frequency-set -g performance

# Run benchmarks
cargo bench --bench mixer

# Results in target/criterion/
```
