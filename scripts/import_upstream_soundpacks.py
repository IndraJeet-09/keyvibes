#!/usr/bin/env python3
"""Import the upstream mechanical sound packs into KeyVibes pack sources.

Reads the pinned `rustyvibes-v2` checkout (https://github.com/KunalBagaria/
rustyvibes-v2, commit b1f3584319a6c7a31f5f7c29f21baba4c08148d9), converts its
21 packs (11 Mechvibes sprite packs, 10 kbsim row packs) into per-key mono
16-bit WAVs plus a `pack.toml` manifest per pack, and writes a coverage
report. The conversion mirrors upstream's `cargo xtask packs` pipeline step
for step:

  decode (ffmpeg, downmixed to mono) -> one-pole DC blocker -> (mechvibes)
  sprite slicing with onset-aware `slice_end` -> silence trim with short
  fades -> one pack-wide gain so the median press loudness hits -30 dBFS
  (peak ceiling -1 dBFS) -> TPDF-dithered 16-bit quantisation.

Differences from upstream, on purpose:

  * Only a key's **primary** Mechvibes scan code is consulted. Upstream falls
    back to a similar key's clip, and finally to the letter A, for any key the
    pack does not define; KeyVibes never substitutes one key's sound for
    another, so undefined keys are left out of the manifest and reported as
    missing instead.
  * Dither uses a counter-based TPDF keyed by (pack id, clip, sample index)
    rather than upstream's advancing xorshift state, so each clip is
    independent of processing order. Both are deterministic; neither matches
    the other bit for bit, and neither is meant to.

The manifests this script writes use `[processing] enabled = false`, so
`keyvibes pack build` stores the converted WAVs verbatim (guards aside) and
does not re-trim or re-normalise them.

Usage:
    scripts/import_upstream_soundpacks.py \
        --source /path/to/rustyvibes-v2 \
        [--out assets/soundpacks/upstream-src]
"""

from __future__ import annotations

import argparse
import json
import math
import os
import struct
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

# --------------------------------------------------------------------------
# Physical key table: KeyVibes key name -> (primary libuiohook scan code,
# keyboard row). Rows follow upstream's `rvpack::keys` geometry: 0 is the
# function row, 5 the space-bar row. PC-only keys are assigned the nearest
# equivalent position (PrintScreen with the function row, ScrollLock and
# Pause next to NumLock).
# --------------------------------------------------------------------------

KEYS: dict[str, tuple[int, int]] = {
    "Escape": (1, 0),
    "F1": (59, 0),
    "F2": (60, 0),
    "F3": (61, 0),
    "F4": (62, 0),
    "F5": (63, 0),
    "F6": (64, 0),
    "F7": (65, 0),
    "F8": (66, 0),
    "F9": (67, 0),
    "F10": (68, 0),
    "F11": (87, 0),
    "F12": (88, 0),
    "PrintScreen": (99, 0),
    "Grave": (41, 1),
    "Digit1": (2, 1),
    "Digit2": (3, 1),
    "Digit3": (4, 1),
    "Digit4": (5, 1),
    "Digit5": (6, 1),
    "Digit6": (7, 1),
    "Digit7": (8, 1),
    "Digit8": (9, 1),
    "Digit9": (10, 1),
    "Digit0": (11, 1),
    "Minus": (12, 1),
    "Equal": (13, 1),
    "Backspace": (14, 1),
    "Insert": (3666, 1),
    "Home": (3655, 1),
    "PageUp": (3657, 1),
    "NumLock": (69, 1),
    "NumpadDivide": (3637, 1),
    "NumpadMultiply": (55, 1),
    "ScrollLock": (70, 1),
    "Pause": (119, 1),
    "Tab": (15, 2),
    "Q": (16, 2),
    "W": (17, 2),
    "E": (18, 2),
    "R": (19, 2),
    "T": (20, 2),
    "Y": (21, 2),
    "U": (22, 2),
    "I": (23, 2),
    "O": (24, 2),
    "P": (25, 2),
    "LeftBracket": (26, 2),
    "RightBracket": (27, 2),
    "Backslash": (43, 2),
    "Delete": (3667, 2),
    "End": (3663, 2),
    "PageDown": (3665, 2),
    "Numpad7": (71, 2),
    "Numpad8": (72, 2),
    "Numpad9": (73, 2),
    "NumpadSubtract": (74, 2),
    "CapsLock": (58, 3),
    "A": (30, 3),
    "S": (31, 3),
    "D": (32, 3),
    "F": (33, 3),
    "G": (34, 3),
    "H": (35, 3),
    "J": (36, 3),
    "K": (37, 3),
    "L": (38, 3),
    "Semicolon": (39, 3),
    "Apostrophe": (40, 3),
    "Enter": (28, 3),
    "Numpad4": (75, 3),
    "Numpad5": (76, 3),
    "Numpad6": (77, 3),
    "NumpadAdd": (78, 3),
    "LeftShift": (42, 4),
    "Z": (44, 4),
    "X": (45, 4),
    "C": (46, 4),
    "V": (47, 4),
    "B": (48, 4),
    "N": (49, 4),
    "M": (50, 4),
    "Comma": (51, 4),
    "Period": (52, 4),
    "Slash": (53, 4),
    "RightShift": (54, 4),
    "ArrowUp": (57416, 4),
    "Numpad1": (79, 4),
    "Numpad2": (80, 4),
    "Numpad3": (81, 4),
    "NumpadEnter": (3612, 4),
    "LeftCtrl": (29, 5),
    "LeftSuper": (3675, 5),
    "LeftAlt": (56, 5),
    "Space": (57, 5),
    "RightAlt": (3640, 5),
    "RightSuper": (3676, 5),
    "Menu": (3677, 5),
    "RightCtrl": (3613, 5),
    "ArrowLeft": (57419, 5),
    "ArrowDown": (57424, 5),
    "ArrowRight": (57421, 5),
    "Numpad0": (82, 5),
    "NumpadDecimal": (83, 5),
}

assert len(KEYS) == 104, f"expected 104 keys, got {len(KEYS)}"

# KeyVibes keys in `PhysicalKey` enum (discriminant) order, which is the order
# the pack builder writes them and the coverage report lists them in.
KEY_ORDER: list[str] = [
    "Escape",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "Grave",
    "Digit1",
    "Digit2",
    "Digit3",
    "Digit4",
    "Digit5",
    "Digit6",
    "Digit7",
    "Digit8",
    "Digit9",
    "Digit0",
    "Minus",
    "Equal",
    "Backspace",
    "Tab",
    "Q",
    "W",
    "E",
    "R",
    "T",
    "Y",
    "U",
    "I",
    "O",
    "P",
    "LeftBracket",
    "RightBracket",
    "Backslash",
    "CapsLock",
    "A",
    "S",
    "D",
    "F",
    "G",
    "H",
    "J",
    "K",
    "L",
    "Semicolon",
    "Apostrophe",
    "Enter",
    "LeftShift",
    "Z",
    "X",
    "C",
    "V",
    "B",
    "N",
    "M",
    "Comma",
    "Period",
    "Slash",
    "RightShift",
    "LeftCtrl",
    "LeftSuper",
    "LeftAlt",
    "Space",
    "RightAlt",
    "RightSuper",
    "Menu",
    "RightCtrl",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "NumLock",
    "NumpadDivide",
    "NumpadMultiply",
    "NumpadSubtract",
    "NumpadAdd",
    "NumpadEnter",
    "NumpadDecimal",
    "Numpad0",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "Numpad7",
    "Numpad8",
    "Numpad9",
    "PrintScreen",
    "ScrollLock",
    "Pause",
]

assert sorted(KEY_ORDER) == sorted(KEYS), "KEY_ORDER and KEYS disagree"

# --------------------------------------------------------------------------
# Upstream DSP constants (mirrors crates/xtask/src/dsp.rs and
# crates/xtask/src/sources/mechvibes.rs).
# --------------------------------------------------------------------------

TARGET_DB = -30.0
CEILING_DB = -1.0
ATTACK_MS = 60.0

NEAR_DUPLICATE_MS = 50.0
ONSET_TOLERANCE_MS = 10.0
ONSET_MARGIN_MS = 2.0

TRIM_FLOOR_DB = -55.0
TRIM_ONSET_REL_DB = -45.0
TRIM_TAIL_REL_DB = -50.0
TRIM_PREROLL_MS = 0.5
TRIM_FADE_OUT_MS = 6.0

DC_BLOCK_CORNER_HZ = 20.0


def db_to_lin(db: float) -> float:
    return 10.0 ** (db / 20.0)


def lin_to_db(x: float) -> float:
    return 20.0 * math.log10(max(x, 1e-9))


# --------------------------------------------------------------------------
# Decode
# --------------------------------------------------------------------------


def decode_audio(path: Path) -> tuple[int, np.ndarray]:
    """Decodes any ffmpeg-readable file to mono float32 and its sample rate.

    Channels are averaged to mono, matching upstream's symphonia decode.
    """
    proc = subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-i",
            str(path),
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-ac",
            "1",
            "-",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"{path}: ffmpeg failed: {proc.stderr.decode(errors='replace')}")

    rate_proc = subprocess.run(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "csv=p=0",
            str(path),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
    )
    rate = int(rate_proc.stdout.strip() or "0")
    samples = np.frombuffer(proc.stdout, dtype=np.float32).copy()
    if rate == 0 or samples.size == 0:
        raise RuntimeError(f"{path}: decoded no audio")
    return rate, samples


# --------------------------------------------------------------------------
# DSP
# --------------------------------------------------------------------------


def dc_block(samples: np.ndarray, rate: int) -> np.ndarray:
    """One-pole DC blocker with its corner near 20 Hz (upstream `dc_block`)."""
    r = 1.0 - 2.0 * math.pi * DC_BLOCK_CORNER_HZ / float(rate)
    out = np.empty_like(samples)
    prev_in = 0.0
    prev_out = 0.0
    for i in range(samples.size):
        x = float(samples[i])
        y = x - prev_in + r * prev_out
        prev_in = x
        prev_out = y
        out[i] = y
    return out


def onsets_ms(samples: np.ndarray, rate: int) -> list[float]:
    """Times (ms) where a new sound starts, per upstream's envelope rule."""
    block = max(rate // 1000, 1)
    n_blocks = (samples.size + block - 1) // block
    if n_blocks == 0:
        return []
    padded = np.zeros(n_blocks * block, dtype=samples.dtype)
    padded[: samples.size] = samples
    envelope = np.abs(padded).reshape(n_blocks, block).max(axis=1)
    loudest = float(envelope.max()) if n_blocks else 0.0
    if loudest <= 0.0:
        return []
    floor = loudest * db_to_lin(-35.0)
    jump = db_to_lin(15.0)
    ms_per_block = block * 1000.0 / rate

    onsets: list[float] = []
    for i in range(1, n_blocks):
        window = envelope[max(i - 10, 0) : i]
        quiet = float(window.min()) if window.size else float("inf")
        if envelope[i] >= floor and envelope[i] >= max(quiet, 1e-6) * jump:
            t = i * ms_per_block
            if not onsets or t - onsets[-1] > 20.0:
                onsets.append(t)
    return onsets


def slice_end(start: float, duration: float, starts: list[float], onsets: list[float]) -> float:
    """Where a mechvibes sprite slice should end (ms), per upstream."""
    end = start + duration
    next_starts = [s for s in starts if s > start + NEAR_DUPLICATE_MS]
    if not next_starts:
        return end
    nxt = next_starts[0]
    if nxt >= end:
        return end
    owns_a_sound = any(
        t >= start - ONSET_TOLERANCE_MS and t < nxt - ONSET_TOLERANCE_MS for t in onsets
    )
    if not owns_a_sound:
        return end
    next_onset = next((t for t in onsets if t >= nxt - ONSET_TOLERANCE_MS), nxt)
    return min(end, next_onset - ONSET_MARGIN_MS)


def trim_clip(samples: np.ndarray, rate: int) -> np.ndarray | None:
    """Removes leading/trailing silence and applies short fades (upstream).

    Returns None when the clip is silent.
    """
    peak = float(np.abs(samples).max()) if samples.size else 0.0
    floor = db_to_lin(TRIM_FLOOR_DB)
    if peak < floor:
        return None
    onset_threshold = max(floor, peak * db_to_lin(TRIM_ONSET_REL_DB))
    tail_threshold = peak * db_to_lin(TRIM_TAIL_REL_DB)

    abs_samples = np.abs(samples)
    above_onset = np.flatnonzero(abs_samples >= onset_threshold)
    if above_onset.size == 0:
        return None
    onset = int(above_onset[0])
    above_tail = np.flatnonzero(abs_samples >= tail_threshold)
    if above_tail.size == 0:
        return None
    last = int(above_tail[-1])

    to_samples = lambda ms: int(round(ms / 1000.0 * rate))

    start = max(onset - to_samples(TRIM_PREROLL_MS), 0)
    fade_out = to_samples(TRIM_FADE_OUT_MS)
    end = min(last + 1 + fade_out, samples.size)
    out = samples[start:end].copy()

    fade_in = onset - start
    if fade_in > 0:
        ramp = np.arange(fade_in, dtype=np.float32) / np.float32(fade_in)
        out[:fade_in] *= ramp

    n = out.size
    fade_out = min(fade_out, n)
    if fade_out > 0:
        t = (np.arange(fade_out, dtype=np.float32) + 0.5) / np.float32(fade_out)
        out[n - fade_out :] *= 0.5 * (1.0 + np.cos(math.pi * t))

    return out


def attack_rms_db(samples: np.ndarray, rate: int, ms: float) -> float:
    n = min(int(ms / 1000.0 * rate), samples.size)
    if n == 0:
        return lin_to_db(0.0)
    energy = float(np.dot(samples[:n], samples[:n]) / n)
    return lin_to_db(math.sqrt(energy))


def median(values: list[float]) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    mid = len(values) // 2
    if len(values) % 2 == 0:
        return (values[mid - 1] + values[mid]) / 2.0
    return values[mid]


def normalize(
    clips: list[np.ndarray], is_press: list[bool], rate: int
) -> float:
    """One pack-wide gain so median press loudness hits TARGET_DB."""
    levels = [
        attack_rms_db(clip, rate, ATTACK_MS)
        for clip, press in zip(clips, is_press)
        if press
    ]
    if not levels:
        levels = [attack_rms_db(clip, rate, ATTACK_MS) for clip in clips]
    gain_db = TARGET_DB - median(levels)
    gain = db_to_lin(gain_db)
    ceiling = db_to_lin(CEILING_DB)
    for clip in clips:
        projected = float(np.abs(clip).max()) * gain if clip.size else 0.0
        g = gain * ceiling / projected if projected > ceiling else gain
        clip *= np.float32(g)
    return gain_db


_FNV_OFFSET = 0x811C9DC5
_FNV_PRIME = 0x01000193


def fnv1a(text: str) -> int:
    h = _FNV_OFFSET
    for byte in text.encode("utf-8"):
        h = ((h ^ byte) * _FNV_PRIME) & 0xFFFFFFFF
    return h


def _counter_uniforms(seed: int, clip_index: int, n: int) -> np.ndarray:
    """Counter-based uniforms in [0, 1) for dither.

    Keyed by (seed, clip_index, sample index); fully vectorised and
    independent of clip processing order.
    """
    idx = np.arange(n, dtype=np.uint64)
    s = np.uint64((seed ^ ((clip_index * 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF)) & 0xFFFFFFFFFFFFFFFF)
    h = s + idx * np.uint64(0x9E3779B97F4A7C15)
    h ^= h >> np.uint64(30)
    h *= np.uint64(0xBF58476D1CE4E5B9)
    h ^= h >> np.uint64(27)
    h *= np.uint64(0x94D049BB133111EB)
    h ^= h >> np.uint64(31)
    # Top 24 bits -> uniform in [0, 1).
    return (h >> np.uint64(40)).astype(np.float64) / float(1 << 24)


def to_i16(samples: np.ndarray, seed: int, clip_index: int) -> np.ndarray:
    """TPDF-dithered 16-bit quantisation (upstream formula, counter dither)."""
    dither = _counter_uniforms(seed, clip_index, samples.size) - _counter_uniforms(
        seed ^ 0xA5A5A5A5, clip_index, samples.size
    )
    quantised = samples.astype(np.float64) * 32767.0 + dither.astype(np.float64)
    np.round(quantised, out=quantised)
    np.clip(quantised, -32768.0, 32767.0, out=quantised)
    return quantised.astype(np.int16)


def write_wav_i16(path: Path, rate: int, samples: np.ndarray) -> None:
    """Writes mono 16-bit PCM WAV (44-byte canonical header)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    data = samples.astype("<i2").tobytes()
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + len(data),
        b"WAVE",
        b"fmt ",
        16,
        1,
        1,
        rate,
        rate * 2,
        2,
        16,
        b"data",
        len(data),
    )
    path.write_bytes(header + data)


# --------------------------------------------------------------------------
# Pack assembly
# --------------------------------------------------------------------------


@dataclass
class ConvertedPack:
    entry: dict
    rate: int
    # key name -> press sample path(s) relative to the pack dir
    press: dict[str, list[str]] = field(default_factory=dict)
    # key name -> release sample path
    release: dict[str, str] = field(default_factory=dict)
    gain_db: float = 0.0
    clip_count: int = 0
    missing: list[str] = field(default_factory=list)


def convert_clips(
    raw_clips: list[np.ndarray],
    press_index: dict[str, int],
    release_index: dict[str, int],
    rate: int,
    pack_id: str,
    write_dir: Path,
    name_for_clip: dict[int, str],
) -> tuple[dict[str, list[str]], dict[str, str], float, int]:
    """Trims, normalises, quantises and writes the clips a pack actually uses.

    Returns (press paths by key, release path by key, gain_db, clip count).
    """
    used = sorted(set(press_index.values()) | set(release_index.values()))
    trimmed: dict[int, np.ndarray] = {}
    for index in used:
        out = trim_clip(raw_clips[index], rate)
        if out is not None:
            trimmed[index] = out
    if not trimmed:
        raise RuntimeError(f"{pack_id}: every clip is silent")

    ordered = [trimmed[i] for i in sorted(trimmed)]
    # A clip referenced by any press key counts as press for the gain decision.
    press_clips = set(press_index.values())
    is_press = [i in press_clips for i in sorted(trimmed)]

    gain_db = normalize(ordered, is_press, rate)

    seed = fnv1a(pack_id) | 1
    written: dict[int, str] = {}
    for clip_index, index in enumerate(sorted(trimmed)):
        samples = to_i16(trimmed[index], seed, clip_index)
        relative = name_for_clip[index]
        write_wav_i16(write_dir / relative, rate, samples)
        written[index] = relative

    press_paths = {key: [written[i]] for key, i in press_index.items() if i in written}
    release_paths = {key: written[i] for key, i in release_index.items() if i in written}
    return press_paths, release_paths, gain_db, len(ordered)


def convert_mechvibes(src_dir: Path, entry: dict, out_dir: Path) -> ConvertedPack:
    config = json.loads((src_dir / "config.json").read_text())
    defines = config.get("defines")
    if not isinstance(defines, dict) or not defines:
        raise RuntimeError(f"{src_dir}: missing defines")

    sound_name = config.get("sound")
    if not isinstance(sound_name, str):
        raise RuntimeError(f"{src_dir}: missing sound file")
    rate, samples = decode_audio(src_dir / sound_name)
    samples = dc_block(samples, rate)

    to_index = lambda ms: int(round(ms / 1000.0 * rate))

    ranges: list[tuple[int, bool, float, float]] = []
    for key, define in defines.items():
        up = key.endswith("-up")
        code_text = key[: -3] if up else key
        if not code_text.isdigit():
            continue
        if not isinstance(define, list) or len(define) < 2:
            continue
        start, duration = define[0], define[1]
        if not isinstance(start, (int, float)) or not isinstance(duration, (int, float)):
            continue
        ranges.append((int(code_text), up, float(start), float(duration)))
    if not ranges:
        raise RuntimeError(f"{src_dir}: no usable defines")

    starts = sorted({start for code, up, start, _ in ranges if not up})
    onsets = onsets_ms(samples, rate)

    # Deterministic order: by code, then release before press is irrelevant
    # because they are separate defines.
    ranges.sort(key=lambda r: (r[0], r[1]))

    slice_cache: dict[tuple[int, int], int] = {}
    clip_samples: list[np.ndarray] = []
    name_for_clip: dict[int, str] = {}
    press_index: dict[str, int] = {}
    release_index: dict[str, int] = {}

    code_to_key = {code: name for name, (code, _row) in KEYS.items()}

    for code, up, start, duration in ranges:
        end_ms = slice_end(start, duration, starts, onsets)
        begin = min(to_index(start), samples.size)
        end = min(to_index(end_ms), samples.size)
        if end <= begin:
            continue
        span = (begin, end)
        if span in slice_cache:
            index = slice_cache[span]
        else:
            index = len(clip_samples)
            clip_samples.append(samples[begin:end].copy())
            slice_cache[span] = index
            name_for_clip[index] = f"slices/code{code}_{begin}_{end}.wav"
        key_name = code_to_key.get(code)
        if key_name is None:
            continue  # scan code KeyVibes has no physical key for
        if up:
            release_index.setdefault(key_name, index)
        else:
            press_index.setdefault(key_name, index)

    press_paths, release_paths, gain_db, clip_count = convert_clips(
        clip_samples, press_index, release_index, rate, str(entry["id"]), out_dir, name_for_clip
    )
    return ConvertedPack(
        entry=entry,
        rate=rate,
        press=press_paths,
        release=release_paths,
        gain_db=gain_db,
        clip_count=clip_count,
        missing=sorted(set(KEY_ORDER) - set(press_paths)),
    )


def convert_kbsim(src_dir: Path, entry: dict, out_dir: Path) -> ConvertedPack:
    stems = {
        "press_r0": "press/GENERIC_R0",
        "press_r1": "press/GENERIC_R1",
        "press_r2": "press/GENERIC_R2",
        "press_r3": "press/GENERIC_R3",
        "press_r4": "press/GENERIC_R4",
        "press_space": "press/SPACE",
        "press_enter": "press/ENTER",
        "press_backspace": "press/BACKSPACE",
        "release_generic": "release/GENERIC",
        "release_space": "release/SPACE",
        "release_enter": "release/ENTER",
        "release_backspace": "release/BACKSPACE",
    }

    clip_samples: list[np.ndarray] = []
    name_for_clip: dict[int, str] = {}
    rates: set[int] = set()
    index_of: dict[str, int] = {}

    for name, stem in stems.items():
        path = None
        for ext in ("mp3", "wav", "ogg"):
            candidate = src_dir / f"{stem}.{ext}"
            if candidate.is_file():
                path = candidate
                break
        if path is None:
            continue
        rate, samples = decode_audio(path)
        rates.add(rate)
        samples = dc_block(samples, rate)
        index = len(clip_samples)
        clip_samples.append(samples)
        index_of[name] = index
        safe = stem.replace("/", "_")
        name_for_clip[index] = f"slices/{safe}.wav"

    if len(rates) != 1:
        raise RuntimeError(f"{src_dir}: mixed sample rates {sorted(rates)}")
    rate = rates.pop()

    rows = [index_of.get(f"press_r{i}") for i in range(5)]
    present_rows = [r for r in rows if r is not None]
    if not present_rows:
        raise RuntimeError(f"{src_dir}: no press/GENERIC_R* recordings")
    any_row = present_rows[0]
    space = index_of.get("press_space", any_row)
    enter = index_of.get("press_enter", any_row)
    backspace = index_of.get("press_backspace", any_row)
    up_generic = index_of.get("release_generic")
    up_space = index_of.get("release_space")
    up_enter = index_of.get("release_enter")
    up_backspace = index_of.get("release_backspace")

    press_index: dict[str, int] = {}
    release_index: dict[str, int] = {}
    for key_name, (_code, row) in KEYS.items():
        row_index = rows[min(row, 4)]
        clip = row_index if row_index is not None else any_row
        release = up_generic
        if key_name == "Space":
            clip = space
            release = up_space if up_space is not None else up_generic
        elif key_name in ("Enter", "NumpadEnter"):
            clip = enter
            release = up_enter if up_enter is not None else up_generic
        elif key_name == "Backspace":
            clip = backspace
            release = up_backspace if up_backspace is not None else up_generic
        press_index[key_name] = clip
        if release is not None:
            release_index[key_name] = release

    press_paths, release_paths, gain_db, clip_count = convert_clips(
        clip_samples, press_index, release_index, rate, str(entry["id"]), out_dir, name_for_clip
    )
    return ConvertedPack(
        entry=entry,
        rate=rate,
        press=press_paths,
        release=release_paths,
        gain_db=gain_db,
        clip_count=clip_count,
        missing=sorted(set(KEY_ORDER) - set(press_paths)),
    )


# --------------------------------------------------------------------------
# Manifest + report
# --------------------------------------------------------------------------


def toml_escape(text: str) -> str:
    return text.replace("\\", "\\\\").replace('"', '\\"')


# The CLI selects packs by name, so display names must stay unique across
# everything a user may have installed - including the repository's own
# synthesized demo packs (Default, Linear, Holy Panda).
DEMO_PACK_NAMES = {"Default", "Linear", "Holy Panda"}


def display_name(entry: dict) -> str:
    name = str(entry["name"])
    if entry.get("variant"):
        name = f"{name} {entry['variant']}"
    if name in DEMO_PACK_NAMES:
        name = f"{name} (kbsim)"
    return name


def write_manifest(pack: ConvertedPack, out_dir: Path) -> None:
    entry = pack.entry
    lines: list[str] = []
    lines.append("[pack]")
    lines.append(f'name = "{toml_escape(display_name(entry))}"')
    lines.append(f'author = "{toml_escape(str(entry.get("credit", "")))}"')
    lines.append('version = "1.0.0"')
    lines.append(
        'description = "'
        + toml_escape(
            f'{entry.get("variant") or entry["category"]} mechanical switch pack '
            f'converted from rustyvibes-v2/{entry["source"]["dir"]}'
        )
        + '"'
    )
    lines.append('license = "MIT"')
    lines.append(
        'source = "' + toml_escape(f'rustyvibes-v2/{entry["source"]["dir"]}') + '"'
    )
    lines.append(f'category = "{toml_escape(str(entry["category"]))}"')
    lines.append(f"sample_rate = {pack.rate}")
    lines.append("")
    lines.append("[samples]")
    lines.append('format = "wav"')
    lines.append("")
    lines.append("[processing]")
    lines.append("# The importer already trimmed, normalised and dithered these clips")
    lines.append("# exactly the way upstream's pack pipeline does. Building with")
    lines.append("# processing disabled stores them verbatim (guard samples aside).")
    lines.append("enabled = false")
    lines.append("")

    order = {name: i for i, name in enumerate(KEY_ORDER)}
    for key_name in sorted(pack.press, key=lambda k: order[k]):
        samples = pack.press[key_name]
        lines.append("[[keys]]")
        lines.append(f'physical_key = "{key_name}"')
        lines.append("samples = [" + ", ".join(f'"{s}"' for s in samples) + "]")
        release = pack.release.get(key_name)
        if release is not None:
            lines.append(f'release_samples = ["{release}"]')
        lines.append("")

    (out_dir / "pack.toml").write_text("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", required=True, type=Path, help="rustyvibes-v2 checkout")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("assets/soundpacks/upstream-src"),
        help="output directory for converted pack sources",
    )
    parser.add_argument(
        "--only", action="append", default=None, help="convert only these pack ids"
    )
    args = parser.parse_args()

    source: Path = args.source
    catalog_path = source / "assets" / "soundpacks" / "catalog.json"
    if not catalog_path.is_file():
        print(f"error: {catalog_path} not found", file=sys.stderr)
        return 1

    catalog = json.loads(catalog_path.read_text())
    entries = catalog["packs"]
    if args.only:
        wanted = set(args.only)
        entries = [e for e in entries if e["id"] in wanted]

    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)

    report = []
    for entry in entries:
        kind = entry["source"]["kind"]
        # `source.dir` already starts with the kind directory
        # (`mechvibes/cherrymx-black-abs`, `kbsim/alpaca`, ...).
        src_dir = source / "assets" / "soundpacks" / entry["source"]["dir"]
        pack_dir = out / entry["id"]
        if pack_dir.exists():
            for path in sorted(pack_dir.rglob("*")):
                if path.is_file():
                    path.unlink()
        pack_dir.mkdir(parents=True, exist_ok=True)

        if kind == "mechvibes":
            pack = convert_mechvibes(src_dir, entry, pack_dir)
        elif kind == "kbsim":
            pack = convert_kbsim(src_dir, entry, pack_dir)
        else:
            raise RuntimeError(f"{entry['id']}: unknown source kind {kind}")

        write_manifest(pack, pack_dir)
        release_keys = sorted(pack.release, key=lambda k: KEY_ORDER.index(k))
        report.append(
            {
                "id": entry["id"],
                "name": display_name(entry),
                "category": entry["category"],
                "source": f"rustyvibes-v2/{entry['source']['dir']}",
                "rate": pack.rate,
                "keys": len(pack.press),
                "release_keys": len(release_keys),
                "clips": pack.clip_count,
                "gain_db": round(pack.gain_db, 2),
                "covered": sorted(pack.press, key=lambda k: KEY_ORDER.index(k)),
                "release": release_keys,
                "missing": pack.missing,
            }
        )
        print(
            f"{entry['id']:<26} keys={len(pack.press):>3}/104 "
            f"release={len(release_keys):>3} clips={pack.clip_count:>3} "
            f"gain={pack.gain_db:+6.1f}dB rate={pack.rate}"
        )

    (out / "coverage.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    total_keys = sum(r["keys"] for r in report)
    print(
        f"{len(report)} packs, {total_keys} key mappings total, "
        f"coverage report at {out / 'coverage.json'}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
