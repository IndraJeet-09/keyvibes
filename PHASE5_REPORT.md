# Phase 5 — Audio Asset Processing & Sound Quality — IMPLEMENTATION COMPLETE

## Date: 2026-10-04

## Files Created

### kv-pack crate (6 new modules + 3 test suites)

| File | Purpose |
| --- | --- |
| `crates/kv-pack/src/dsp.rs` | Scalar DSP helpers: `db_to_amplitude`, `amplitude_to_db`, `ms_to_frames`, `mean`, `rms`, `max_abs`, `remove_dc` |
| `crates/kv-pack/src/analysis.rs` | `AudioAnalysis`: statistics (peak/RMS/DC/noise floor/crest) + onset/tail detection with attack-window confirmation |
| `crates/kv-pack/src/dither.rs` | `DitherSeed` (FNV-1a derivation, `from_u64`, `random`) + `TpdfDither` (splitmix64, one draw per sample) |
| `crates/kv-pack/src/processing.rs` | `ProcessingConfig` (all tunables + validation), `ProcessingWarning` (10 variants), `ProcessingReport`, `AudioProcessor::process`, `apply_fadeout`, `validate_samples`, `encode_i16` |
| `crates/kv-pack/tests/processing_pipeline.rs` | 9 end-to-end tests: valid build, report values, trim, determinism, disabled bypass, silence failure, config validation, processed golden fixture |
| `crates/kv-pack/tests/processing_props.rs` | 7 proptest properties: validity/ceiling, determinism, frame accounting, rate preservation, fade invariants, encode behavior |
| `crates/kv-pack/benches/processing_bench.rs` | Criterion: 10 ms / 100 ms / 1 s pipeline, dithered + plain encode, analysis pass |

### CLI

| File | Purpose |
| --- | --- |
| `crates/keyvibes/src/audio_cmd.rs` | `keyvibes analyze` (read-only full report for WAVs or a manifest) and `keyvibes process` (render one WAV through the pipeline) |

### Documentation

| File | Purpose |
| --- | --- |
| `docs/audio-processing.md` | Full specification: pipeline, stage algorithms, defaults table, manifest reference, determinism rules, warnings, errors, CLI, benchmarks, out-of-scope |
| `PHASE5_REPORT.md` | This report |

## Files Modified

| File | Change |
| --- | --- |
| `crates/kv-pack/src/wav.rs` | Added `SourceAudio` / `MonoAudio`, `decode_wav` (f32 path with per-format normalization), `write_wav_i16`. Legacy `load_wav` untouched (byte-compat) |
| `crates/kv-pack/src/manifest.rs` | `ProcessingSection` (`[processing]`, all fields optional), `PackManifest::processing_config()`, parse-time validation |
| `crates/kv-pack/src/builder.rs` | `BuildEvent::Processed { key, variant, report }`, processing branch in `build_from_manifest`, `dither_seed_for` |
| `crates/kv-pack/src/error.rs` | New `PackError::UnusableSignal { peak_dbfs, minimum_dbfs }` |
| `crates/kv-pack/src/lib.rs` | Exported `analysis`, `dither`, `dsp`, `processing` modules + re-exports |
| `crates/keyvibes/src/cli.rs` | `Analyze` and `Process` subcommands |
| `crates/keyvibes/src/main.rs` | Command dispatch, verbose per-clip `Processed` report printing |
| `crates/keyvibes/Cargo.toml` | (via kv-pack default features — no change needed) |
| `crates/kv-pack/Cargo.toml` | `criterion` dev-dependency + `[[bench]]` |
| `crates/kv-pack/tests/builder_roundtrip.rs` | Phase 4 tests pin `enabled = false` (byte-compat) |
| `crates/kv-pack/tests/fixtures/golden-pack/pack.toml` | `[processing] enabled = false` (golden bytes stay Phase 4) |
| `crates/kv-pack/tests/fixtures/golden-pack/pack-processed.toml` | New manifest: same sources, processing enabled, `dither_seed = 424242` |
| `crates/kv-pack/tests/fixtures/processed.kvpack` | New committed fixture built through the pipeline |
| `docs/kvpack-builder.md` | Pipeline diagram includes both branches, `[processing]` in manifest reference, CLI section, testing section, error table |

## Implementation Summary

### Decode to f32 ✓

- `decode_wav` normalizes PCM 8/16/24/32-bit and IEEE float 32-bit by their
  full-scale values; float samples must be finite.
- hound 3.5.1 already converts 8-bit unsigned storage to signed on read
  (`signed_from_u8`), so the f32 path divides by 128 like the others.
- Stereo stays interleaved until `SourceAudio::to_mono()` averages each
  frame; the source channel count survives into the report.
- Legacy `load_wav` (i16) is byte-for-byte unchanged for `enabled = false`.

### DC correction, analysis, detection ✓

- Global mean removed **before** detection; value reported and warned at
  `|mean| > 0.01`.
- Onset: first 1 ms peak-envelope window ≥ `max(peak·10^(−45/20), 10^(−55/20))`
  whose 60 ms attack window has `RMS ≥ threshold/√2` — accepts impulses,
  rejects isolated blips.
- Tail: start of the trailing below-threshold run, only once it lasts ≥ 5 ms.
- Noise floor: 10th percentile of 1 ms window RMS.
- All thresholds are `ProcessingConfig` fields; no scattered numbers.

### Trim, fade, loudness, peak protection ✓

- Leading cut = `onset − preroll` (saturated), trailing cut = tail; the
  fallback keeps the whole clip (`TrimFallback`) instead of panicking.
- Raised-cosine fade (`0.5·(1+cos(π·(j+1)/F))`), last sample exactly 0.
- Loudness = full trimmed+faded RMS scalar gain, capped at +40 dB.
- Peak protection clamps the gain so the final peak ≤ −1 dBFS
  (`10^(−1/20) = 0.8912501`).
- Validation rejects NaN/Inf and anything above ceiling + 1e-5.

### Dither + quantize ✓

- TPDF `(u1 − u2)/32768`, one draw per sample, splitmix64 state.
- Seed = `DitherSeed::derive(dither_seed, key, variant, manifest-relative
  path)` — reproducible across machines; `random_dither` opt-in for OS
  entropy; `dither = false` quantizes plainly.
- `f32::round` (half away from zero), clamp, `i16`; guards added afterwards
  by the unchanged `ClipData::from_pcm`.

### Manifest + builder integration ✓

- `[processing]` section: every field optional, defaults = documentation
  defaults, validated at parse time.
- `build_from_manifest` branches on `enabled`; both branches wrap failures
  in `BuildContext { key, file, reason }`.
- `BuildEvent::Processed` carries the full `ProcessingReport` per clip;
  `pack build --verbose` prints it.

### CLI ✓

- `keyvibes analyze <wav|pack.toml>...` — read-only report (frames, peak,
  RMS, DC, gain, onset/tail, warnings); a manifest uses its own config.
- `keyvibes process <in.wav> -o <out.wav>` — renders one clip through the
  default pipeline to a 16-bit mono WAV.

### Byte compatibility ✓

- `enabled = false` reproduces Phase 4 exactly: `golden_fixture_is_reproducible`
  still compares byte-for-byte against the committed `golden.kvpack`.
- Phase 4 tests pin `enabled = false` where they assert raw sample values.
- The golden fixture source files were **not** modified.

## Testing

| Suite | Tests | Status |
| --- | ---: | --- |
| `kv-pack` unit (dsp, dither, analysis, processing, wav, manifest, builder, layout, writer, parser) | 114 | ✓ |
| `builder_roundtrip` (Phase 4 golden + raw path) | 15 | ✓ |
| `processing_pipeline` (Phase 5 e2e + processed golden) | 9 | ✓ |
| `processing_props` (proptest properties) | 7 | ✓ |
| Workspace (kv-core, kv-ring, kv-mixer, kv-input-linux, kv-audio-pipewire, kv-runtime, keyvibes) | 83 | ✓ |
| **Total** | **228** | **✓ 0 failed** |

Highlights:

- `processed_golden_fixture_is_reproducible` — pipeline build is
  byte-identical to the committed `processed.kvpack`.
- `processing_builds_are_byte_identical` — two builds with dither enabled
  match byte-for-byte.
- `processing_trims_silent_padding` — report accounting:
  `leading + processed + trailing == original`.
- `silence_source_fails_with_actionable_error` — `UnusableSignal` inside
  `BuildContext`, no output file left behind.
- Property tests over random buffers/rates: ceiling, finiteness,
  determinism, frame accounting, rate preservation, fade invariants,
  encode length/seed behavior.

## Performance

Criterion benchmarks (`cargo bench -p kv-pack`), all offline build-time:

- `process/typical_keypress_10ms` — typical clip (480 frames)
- `process/typical_keypress_100ms` — 4 800 frames
- `process/long_1s` — 48 000 frames worst case
- `encode_i16_dither_100ms` / `encode_i16_no_dither_100ms`
- `analysis_measure_1s`

The pipeline is O(n) with two passes over the buffer (measure + envelope,
then trim/gain); no allocations per sample.

## Determinism

- Fixed stage order, pure `f32` arithmetic, no randomness, no time, no
  environment reads.
- Dither derived from `(dither_seed, key, variant, relative source path)`.
- Verified by tests and two committed fixtures (`golden.kvpack`,
  `processed.kvpack`).

## Runtime Safety (unchanged)

- The audio callback (`kv-audio-pipewire/src/stream.rs`) still does zero
  allocations, zero locks, zero I/O: processing runs only in the builder.
- Format, loader, guards (`GUARD_BEFORE = 2`, `GUARD_AFTER = 3`), and the
  mixer's four-tap interpolation are untouched.

## Explicitly Out of Scope

- No resampling (one pack, one rate).
- No runtime DSP.
- No `.kvpack` format changes or loader rewrite.
- No LUFS, spectral mastering, compression, or ML-based processing —
  full-clip RMS + scalar gain + peak ceiling only.

## Verification Commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --release
cargo bench --no-run
cargo doc --workspace --no-deps
cargo run -p keyvibes -- analyze crates/kv-pack/tests/fixtures/golden-pack/pack.toml
cargo run -p keyvibes -- pack build <manifest> -o out.kvpack
cargo run -p keyvibes -- pack validate out.kvpack
cargo run -p keyvibes -- pack inspect out.kvpack --clips
# determinism: two builds of the same manifest must be byte-identical
```
