# KVPack Builder

**Status:** Implemented  
**Date:** 2026-10-03  
**Crate:** `kv-pack` (feature `builder`, enabled by default)

The builder turns a text manifest plus WAV sources into a deterministic,
validated `.kvpack` file. The format itself is specified in
[`kvpack-format.md`](./kvpack-format.md).

## Pipeline

```
pack.toml ──manifest──▶ PackManifest ──resolve sources──▶ for each sample
                                                                │
                                   wav::load_wav (decode + validate + downmix)
                                                                │
                                                        ClipData::from_pcm
                                                        (guards applied)
                                                                │
                                  builder::plan() ──▶ PackPlan { metadata, keys, clips }
                                                                │
                          writer::write_plan_atomic() ──▶ layout::compute_layout
                                                                │
                                        encode_header + sections ──▶ <path>.tmp
                                                                │
                                  self-validate with loader::KvPack::open(tmp)
                                                                │
                                              fs::rename(tmp, final path)
```

Every stage returns `PackResult` (`Result<T, PackError>`). Failures during a
per-source step are reported as `PackError::BuildContext { key, file, reason }`
so the user sees which key and which file failed.

## Manifest (`pack.toml`)

```toml
[pack]
name = "My Pack"           # required
author = "Me"              # required
version = "1.0.0"          # informational only, never written to the binary
description = "..."        # optional
license = "MIT OR Apache-2.0"   # optional
source = "keyvibes-factory"     # optional
sample_rate = 48000        # optional; inferred from the first sample if omitted

[samples]
format = "wav"             # optional; only "wav" is accepted

[[keys]]
physical_key = "A"         # canonical name or alias, case-insensitive
samples = ["sounds/a-0.wav", "sounds/a-1.wav"]
```

Rules (enforced by `manifest::PackManifest::validate` + `resolve_source`):

- **Key names:** canonical names come from `PhysicalKey` (`Escape`, `Space`,
  `A`… `Z`, `Digit1`… `Digit0`, `F1`… `F12`, arrows, `Minus`, `Equal`,
  `BracketLeft`, …). Aliases: `"1"`…`"0"` → `Digit1`…`Digit0`, `"esc"` →
  `Escape`. Matching is case-insensitive (`"a"`, `"A"`, `"SPACE"` all work).
- **Duplicates:** two `[[keys]]` entries resolving to the same physical key are
  rejected (`DuplicatePhysicalKey`).
- **Empty samples:** a key with zero sample paths is rejected
  (`EmptyKeySamples`); an empty pack (no keys) is rejected (`EmptyPack`).
- **Paths:** each sample path is canonicalized against the manifest's directory.
  Absolute paths, `..` components, and symlinks escaping the pack root are
  rejected (`PathTraversal`, surfaced inside `BuildContext`). Missing files
  produce `SourceNotFound` (also inside `BuildContext`).
- **`physical_key` unknown** → `UnknownPhysicalKey`.

## WAV policy

Implemented in `wav.rs`:

- **Formats:** PCM integer 8/16/24/32-bit and IEEE float 32-bit; anything else
  → `UnsupportedSampleFormat`.
- **Channels:** 1–2 channels. Stereo is downmixed to mono as `(L + R) / 2`
  (`i32` sum with truncation, `f32` average). More than 2 channels →
  `ChannelLayoutUnsupported`.
- **Float conversion:** `clamp(x, -1.0, 1.0) * 32768.0`, rounded, then clamped
  to `i16` range (`-1.0` → `-32768`, `1.0` → `32767`). NaN and ±Inf are
  rejected with `NonFiniteSample`.
- **Truncated data:** if the file ends before the declared frame count, hound's
  read surfaces an error mapped to `TruncatedAudio { expected, actual }`.
- **Sample rate:** must be 8 000–192 000 Hz (`SampleRateOutOfRange`).

### Rate policy (one pack, one rate)

There is **no resampling**. `PackPlan` requires every clip to share a single
sample rate:

1. `manifest.pack.sample_rate`, if set, is authoritative.
2. Otherwise the first decoded clip fixes the rate.
3. Any later clip with a different rate → `SampleRateMismatch { expected, actual }`,
   wrapped in `BuildContext` during a manifest build so the offending file is
   named.

At playback time `KvPack::play_command` computes
`pitch_step = (source_rate << 32) / output_rate`, so packs built for one rate
play back correctly at the engine's output rate (a ratio, not a conversion).

## Guards

Each clip is stored as `[guard_before][logical frames][guard_after]` with
`GUARD_BEFORE = 2` and `GUARD_AFTER = 3` frames. Guards use **edge extension**:
they copy the first (resp. last) sample, so interpolation can read outside the
logical region without boundary artifacts. Guards are part of the written PCM
and are re-checked by the loader's validator (`InvalidGuardSamples`).

## Layout

`layout::compute_layout` places sections in order:

1. `Header` — 120 bytes (see format spec).
2. `Metadata` — length-prefixed UTF-8 strings, no trailing padding.
3. `Key table` — `key_count * 8` bytes, entries **sorted by physical key
   discriminant ascending**.
4. `Clip table` — `clip_count * 24` bytes, clips ordered per key in *manifest*
   order (variant order is part of the data, not sorted).
5. Padding up to the next 16-byte boundary (`SAMPLE_ALIGNMENT`).
6. `Sample data` — clip payloads back to back, no gaps.

`sample_data_offset` is therefore always `16`-byte aligned; everything before
it is byte-exact with no implicit padding except the section 4→6 gap.

## Atomic, deterministic write

`writer::write_plan_atomic`:

1. Writes `<output>.tmp` (same directory, so the rename stays on one
   filesystem).
2. Encodes header + sections with **zero padding** for any alignment gap —
   never uninitialized bytes.
3. `flush()` + `sync_all()` the file.
4. Re-opens the temporary file with the *real loader*
   (`crate::loader::KvPack::open`) and runs full validation. A pack that cannot
   be loaded by the runtime is never published.
5. `fs::rename(tmp, output)`.
6. On any error the temporary file is removed; the destination is left
   untouched (or absent if it did not exist).

**Determinism guarantees** (verified by tests):

- No timestamps, paths, hostnames, or random data in the output.
- Key table sorted by `PhysicalKey` discriminant (`BTreeMap` iteration).
- Clip order per key = manifest order.
- All padding bytes are zero.
- The same manifest + sources always produce byte-identical files
  (`repeated_builds_are_byte_identical`, and
  `golden_fixture_is_reproducible` compares against a committed fixture).

Consequently, rebuilding a pack and committing it makes diffs meaningful: any
byte change means the content actually changed.

## Programmatic API

```rust
use kv_pack::{ClipData, KvPack, PackBuilder};

let mut builder = PackBuilder::new();
builder.name = "Ad-hoc".into();
builder.set_sample_rate(48_000)?; // optional lock; otherwise inferred
builder.add_clip(
    kv_core::PhysicalKey::Space,
    ClipData::from_samples(vec![0i16; 480], 48_000)?, // guards applied here
)?;
builder.write("out.kvpack")?;
builder.write_with_progress("out2.kvpack", |event| { /* Progress/WriteStage */ })?;

let pack = KvPack::open("out.kvpack")?;
let mut state = kv_pack::VariantState::default();
if let Some(cmd) = pack.play_command(kv_core::PhysicalKey::Space, &mut state, 48_000, 1.0, 1.0) {
    // cmd.sample_ptr / sample_len / pitch_step …
}
```

`ClipData::from_wav` decodes a file directly; `from_pcm` takes already-validated
samples; `from_samples` applies guards to raw (guard-free) samples.

## CLI

```
keyvibes pack build <MANIFEST> -o <OUTPUT> [--verbose]
keyvibes pack validate <PACK>
keyvibes pack inspect <PACK> [--clips]
```

`pack build` streams progress (`[n/total] reading …`, `writing pack…`,
`validating pack…`) and reports the summary line
`Built <path>: N keys, N clips, N Hz, N bytes`.

`pack validate` runs the full loader validation plus structural checks and
prints `OK` or the failing check with context.

`pack inspect` prints metadata, header fields, section offsets/sizes, the key
table, and (`--clips`) per-clip offset/frame/guard information.

## Error reporting

Per-source failures are wrapped so the CLI can print
`Key/File/Reason` without unwrapping the cause:

| Cause | Reported as |
| --- | --- |
| Missing file | `BuildContext { key, file, reason: "Source file not found: …" }` |
| `..` / absolute / symlink escape | `BuildContext { …, reason: "Source path escapes pack root: …" }` |
| Wrong sample rate | `BuildContext { …, reason: "sample rate mismatch: expected … got …" }` |
| Bad WAV data | `BuildContext { …, reason: "<hound/wav message>" }` |
| Unknown / duplicate key | `UnknownPhysicalKey`, `DuplicatePhysicalKey` (manifest-level, no file) |
| Mixed rates in programmatic build | `SampleRateMismatch { expected, actual }` |

Failed builds never leave an output file or a `.tmp` behind.

## Testing

- Unit tests live in each module (`manifest`, `wav`, `layout`, `writer`,
  `builder`).
- End-to-end tests: `crates/kv-pack/tests/builder_roundtrip.rs` — golden pack
  reproducibility, sample/guard round-trip, stereo downmix, manifest variant
  order, determinism, error cases (missing source, rate mismatch, traversal,
  unknown/duplicate keys), corrupt/truncated pack rejection, programmatic
  builder.
- Fixtures: `crates/kv-pack/tests/fixtures/golden-pack/` (manifest + WAVs) and
  the committed `crates/kv-pack/tests/fixtures/golden.kvpack`, regenerated with:

  ```
  cargo run -p keyvibes -- pack build crates/kv-pack/tests/fixtures/golden-pack/pack.toml \
      -o crates/kv-pack/tests/fixtures/golden.kvpack --verbose
  ```

## Out of scope (Phase 5)

The builder performs no DSP: no dB thresholds, no fades, no dither, no loudness
normalization. Samples are written exactly as decoded (after downmix and
float→i16 quantization).
