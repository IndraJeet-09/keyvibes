# Audio Processing (Phase 5)

**Status:** Implemented  
**Date:** 2026-10-04  
**Crate:** `kv-pack` (feature `builder`, enabled by default)

Offline, deterministic audio processing for pack sources: decode to `f32`,
correct DC, detect and trim silence, fade out, normalize loudness, protect
the peak, and encode back to dithered `i16` — all **at build time**. The
runtime never performs DSP: it loads the final PCM exactly as Phase 4
designed, guards included.

## Pipeline

```text
Source WAV
  → decode to f32                     (wav::decode_wav)
  → channel normalization to mono     (SourceAudio::to_mono)
  → DC correction                     (dsp::remove_dc, mean reported)
  → analysis                          (AudioAnalysis::measure + detect)
  → leading trim + preroll
  → tail trim
  → fade-out (raised cosine)
  → loudness normalization (RMS)
  → peak protection
  → validation                        (finite + ceiling check)
  → TPDF dither + i16 quantization    (processing::encode_i16)
  → guard samples                     (ClipData::from_pcm)
  → KVPack writer                     (Phase 4, unchanged)
```

Every stage runs in `AudioProcessor::process`
(`crates/kv-pack/src/processing.rs`); thresholds live in one
`ProcessingConfig` and the algorithms in `analysis.rs`, `dsp.rs`, and
`dither.rs`. Nothing numeric is scattered through the code.

### Stage details

**Decode** — `decode_wav` normalizes each format by its full-scale value
(8-bit `/128`, 16-bit `/32768`, 24-bit `/8388608`, 32-bit `/2147483648`,
float as stored) and rejects non-finite float samples. Unlike the legacy
`load_wav`, no quantization happens here — the DSP works at full `f32`
precision. Stereo is averaged to mono (`(L + R) / 2`).

**DC correction** — the global mean is subtracted *before* detection, so
thresholds and statistics describe the AC content of the clip. The removed
value is reported (`dc_correction`) and a warning fires when
`|mean| > 0.01`.

**Analysis** (`AudioAnalysis`) — one pass computes min/max/peak/RMS/DC and a
windowed (1 ms) envelope; the 10th-percentile window RMS becomes the noise
floor. Detection then works on the DC-corrected buffer:

- *Onset*: the first 1 ms peak-envelope window at or above
  `max(peak · 10^(onset_threshold_db / 20), 10^(silence_threshold_dbfs / 20))`
  whose following attack window (60 ms, clamped to the clip) has
  `RMS ≥ threshold / √2`. The RMS confirmation accepts impulse attacks
  (which have no sustained peak envelope) while rejecting isolated
  single-sample blips.
- *Tail*: the start of the trailing run of below-threshold windows, but only
  when that run lasts at least `tail_sustain_ms` (5 ms ⇒ 1 window at 48 kHz
  with a 1 ms envelope). Otherwise the clip runs to its end.

**Trim + preroll** — the output starts `preroll_ms` (0.5 ms = 24 frames at
48 kHz) *before* the detected onset, saturated at 0, and ends at the tail.
If the two disagree (`end ≤ start`), the whole clip is kept and a
`TrimFallback` warning is emitted rather than fabricating a zero-length
clip. Every index uses saturating arithmetic, so clips shorter than
preroll + fade never underflow or panic.

**Fade-out** — raised cosine over the final `fadeout_ms` (6 ms = 288 frames
at 48 kHz), clamped to the buffer length:

```text
gain(j) = 0.5 · (1 + cos(π · (j + 1) / F))    j = 0 … F-1
```

The last sample is multiplied by exactly `0.0` (silent end, no click) and a
raised cosine is used instead of a linear ramp because its slope also starts
at zero.

**Loudness normalization** — a single scalar gain toward the full-clip RMS
target:

```text
gain = 10^(loudness_target_db / 20) / rms(buffer)
```

RMS instead of LUFS: LUFS-style gated measurement needs ≥ 400 ms blocks and
is unstable on 50–300 ms keyboard clicks; full-clip RMS is deterministic,
defined for any length, and — being one scalar — preserves the
transient/decay contrast inside each clip (no compression). The gain is
capped at `max_gain_db` (40 dB, `ExcessiveGain` warning when hit).

**Peak protection** — if `peak(buffer) · gain` would exceed
`10^(peak_ceiling_dbfs / 20)` (0.8912501 at −1 dBFS), the gain is reduced to
`ceiling / peak`. The final samples therefore never exceed the ceiling
before dithering.

**Validation** — every sample must be finite and within
`ceiling + 1e-5`; otherwise the build fails (`NonFiniteSample` /
`ValidationFailed`) before anything reaches the encoder.

**Dither + quantize** (`encode_i16`) — TPDF dither
`(u1 − u2) / 32768` from a splitmix64 PRNG, one draw per sample, only when
seeded: `clamp → ×32768 → round (half away from zero) → clamp to i16`.
`-1.0` maps to `i16::MIN`, `+1.0` to `i16::MAX`. Guard samples are added
afterwards by `ClipData::from_pcm`, exactly as in Phase 4.

**Guards + writer** — unchanged Phase 4 code paths. Processing produces the
logical frames; the writer stores `GUARD_BEFORE = 2` / `GUARD_AFTER = 3`
edge-extension guards around them.

## Defaults

| Setting | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | `false` selects the legacy raw path (byte-identical to Phase 4) |
| `silence_threshold_dbfs` | −55 | Absolute silence floor for detection |
| `onset_threshold_db` | −45 | Onset threshold, relative to the clip peak |
| `tail_threshold_db` | −50 | Tail threshold, relative to the clip peak |
| `preroll_ms` | 0.5 | Audio kept before the detected onset (24 frames @ 48 kHz) |
| `fadeout_ms` | 6 | Raised-cosine fade-out length (288 frames @ 48 kHz) |
| `attack_rms_window_ms` | 60 | Onset confirmation window (clamped to clip length) |
| `loudness_target_db` | −30 | Target full-clip RMS |
| `peak_ceiling_dbfs` | −1 | Final peak ceiling |
| `min_signal_dbfs` | −80 | Usable peak below this fails the build |
| `tail_sustain_ms` | 5 | Quiet duration required before the clip "ends" |
| `max_gain_db` | 40 | Upper bound on the loudness gain |
| `dither` | `true` | TPDF dither before quantization |
| `dither_seed` | `0` | Base seed (see determinism below) |
| `random_dither` | `false` | OS entropy instead (breaks reproducibility) |

All values are validated at manifest parse time: non-finite values and
ranges (e.g. `loudness_target_db ≤ 0`, `fadeout_ms ≥ 0`) are rejected with a
`ValidationFailed` message naming the field.

## Manifest reference

```toml
[processing]
enabled = true
silence_threshold_dbfs = -55.0
onset_threshold_db = -45.0
tail_threshold_db = -50.0
preroll_ms = 0.5
fadeout_ms = 6.0
attack_rms_window_ms = 60.0
loudness_target_db = -30.0
peak_ceiling_dbfs = -1.0
min_signal_dbfs = -80.0
tail_sustain_ms = 5.0
max_gain_db = 40.0
dither = true
dither_seed = 424242
random_dither = false
```

The whole section is optional. With no `[processing]` section the defaults
above apply (processing **enabled**). To reproduce the pre-Phase-5 behavior
exactly:

```toml
[processing]
enabled = false
```

`enabled = false` bypasses the whole pipeline: sources go through the
original `wav::load_wav` i16 path and the output is byte-for-byte what
Phase 4 produced (the committed `tests/fixtures/golden.kvpack` relies on
this).

## Determinism

- All DSP is plain `f32` arithmetic in a fixed stage order — no randomness,
  no time, no paths, no environment.
- The dither seed for each clip is
  `DitherSeed::derive(pack_seed = dither_seed, key, variant,
  manifest_relative_source_path)`. The *manifest-relative* path is used (not
  the absolute one), so the same pack builds identically on any machine.
  `variant` is the index of the sample within its key's list.
- `random_dither = true` replaces the derived seed with OS entropy —
  documented as a non-reproducible mode for production masterings.
- Verified by `processing_builds_are_byte_identical` and the committed
  `tests/fixtures/processed.kvpack` (rebuilt and compared byte-for-byte by
  `processed_golden_fixture_is_reproducible`).

## Warnings

Warnings never fail a build; they are printed by `keyvibes pack build
--verbose` and by `keyvibes analyze`:

| Warning | Condition |
| --- | --- |
| `ClippedSource` | source peak ≥ 0.999 (≈ 0 dBFS) |
| `HighDcOffset` | `|mean| > 0.01` |
| `LowSignal` | peak within 10 dB of `min_signal_dbfs` |
| `ShortClip` | shorter than 5 ms |
| `LongClip` | longer than 5 s |
| `NoTransient` | no onset crossed the threshold with confirmation |
| `HighNoiseFloor` | noise floor within 25 dB of the peak |
| `ExcessiveGain` | requested gain hit `max_gain_db` |
| `AbnormalCrest` | peak/RMS outside 3–96 dB |
| `TrimFallback` | onset/tail disagreed; whole clip kept |

## Errors

| Cause | Reported as |
| --- | --- |
| Silence or pure DC after correction | `UnusableSignal { peak_dbfs, minimum_dbfs }` (inside `BuildContext`) |
| Bad `[processing]` value | `ValidationFailed` at manifest parse |
| Non-finite / over-ceiling result | `NonFiniteSample` / `ValidationFailed` (inside `BuildContext`) |
| Wrong sample rate | `InvalidSampleRate`, then the usual rate-mismatch check |

`UnusableSignal` fires *after* DC correction: a constant buffer becomes
exact zeros and must not silently turn into a rendered-silent clip.

## CLI

```
keyvibes analyze <wav|pack.toml>...     # read-only: show what would happen
keyvibes process <input.wav> -o <out.wav>  # render one clip through the pipeline
keyvibes pack build <pack.toml> -o <out.kvpack> --verbose
```

`analyze` decodes its inputs (a WAV uses the default config; a manifest uses
its own `[processing]`) and prints the full report — frames/duration before
and after, peak, RMS, DC, gain, onset/tail frames, and warnings — without
writing anything. `process` runs one file through the pipeline (default
config) and writes a 16-bit mono PCM WAV so the result can be auditioned.
`pack build --verbose` prints a one-line summary plus warnings per clip.

Example:

```
$ keyvibes analyze sounds/key.wav
sounds/key.wav  (16-bit PCM, 48000 Hz, 1ch -> mono)
  frames:      12000 -> 2496   (lead 4776, tail 4728, preroll 24, fade 288)
  duration:    250.0 ms -> 52.0 ms
  peak:        -6.02 -> -1.00 dBFS
  rms:        -18.99 -> -30.00 dBFS
  dc:         +0.0002 corrected (original +0.0002, final -0.0001)
  gain:       +11.01 dB (onset frame 4800, tail frame 7200)
  warnings:   none
```

## Performance

Processing is O(n) per clip with small constants (one windowed pass, one
trim/gain pass) and runs offline. Criterion benchmarks live in
`crates/kv-pack/benches/processing_bench.rs`:

```
cargo bench -p kv-pack
```

Covering a 10 ms key press, a 100 ms clip, a 1 s worst case, dithered and
undithered encode, and the analysis pass.

## Out of scope (by design)

- **No resampling** — one pack, one rate (unchanged from Phase 4).
- **No runtime DSP** — the audio callback stays allocation/lock/I/O-free.
- **No format changes** — the `.kvpack` layout, loader, and guards are
  untouched; processing only changes the logical frames that go in.
- **No LUFS/spectral mastering/ML** — full-clip RMS and a peak ceiling only.
- **No compression or limiting** beyond the single scalar peak protection.

## Testing

- Unit tests: `src/dsp.rs`, `src/dither.rs`, `src/analysis.rs`,
  `src/processing.rs`, `src/wav.rs` (per-format decode), `src/manifest.rs`
  (`[processing]` parsing/validation).
- End-to-end: `tests/processing_pipeline.rs` — valid build, report values,
  trim accounting, byte determinism, disabled-path bypass, silence failure,
  config validation, processed golden fixture.
- Property tests: `tests/processing_props.rs` — validity/ceiling,
  determinism, frame accounting, rate preservation, fade invariants,
  encode length/seed behavior over randomized inputs.
- Fixtures: `tests/fixtures/golden-pack/pack-processed.toml` +
  `tests/fixtures/processed.kvpack`, regenerated with:

  ```
  cargo run -p keyvibes -- pack build \
      crates/kv-pack/tests/fixtures/golden-pack/pack-processed.toml \
      -o crates/kv-pack/tests/fixtures/processed.kvpack --verbose
  ```
