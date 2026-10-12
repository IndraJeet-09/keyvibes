#!/usr/bin/env python3
"""Generate the source WAVs for the KeyVibes default sound packs.

Provenance
----------
The clips written by this script are **modeled**, not recorded. There is no
microphone, no field recording and no third-party sample library anywhere in
this repository: every sample is synthesized from a small physical model of a
mechanical key press (impact transient, leaf-spring click, keycap/stem
resonance, plate body resonance, spring rattle) and then written to a 48 kHz
mono 16-bit WAV for `keyvibes pack build` to process.

That distinction is stated in each pack manifest's `description` and must stay
there. Replacing these files with genuine switch recordings is a content
change, not a code change - see docs/soundpacks.md.

Determinism
-----------
Output depends only on (key name, variant, MODEL_VERSION). Re-running the
script reproduces the same bytes, so a fresh clone builds the same packs.

Usage
-----
    python3 scripts/generate_sounds.py [--out DIR] [--keys KEY,KEY] [--variants N]
"""

from __future__ import annotations

import argparse
import hashlib
import math
import sys
import wave
from pathlib import Path

import numpy as np

# ---------------------------------------------------------------------------
# Model parameters
# ---------------------------------------------------------------------------

SAMPLE_RATE = 48000
DURATION_S = 0.120
MODEL_VERSION = "switch-v1"
PEAK_TARGET = 0.95

# Every `PhysicalKey` variant, in discriminant order (kv-core/src/physical_key.rs).
# The Rust side owns this list; a mismatch is caught by the pack coverage test.
PHYSICAL_KEYS = [
    "Escape", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    "Grave", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
    "Digit8", "Digit9", "Digit0", "Minus", "Equal", "Backspace",
    "Tab", "Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P",
    "LeftBracket", "RightBracket", "Backslash",
    "CapsLock", "A", "S", "D", "F", "G", "H", "J", "K", "L",
    "Semicolon", "Apostrophe", "Enter",
    "LeftShift", "Z", "X", "C", "V", "B", "N", "M",
    "Comma", "Period", "Slash", "RightShift",
    "LeftCtrl", "LeftSuper", "LeftAlt", "Space", "RightAlt", "RightSuper", "Menu", "RightCtrl",
    "Insert", "Delete", "Home", "End", "PageUp", "PageDown",
    "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
    "NumLock", "NumpadDivide", "NumpadMultiply", "NumpadSubtract", "NumpadAdd",
    "NumpadEnter", "NumpadDecimal",
    "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4",
    "Numpad5", "Numpad6", "Numpad7", "Numpad8", "Numpad9",
    "PrintScreen", "ScrollLock", "Pause",
]

# ANSI row of each key (matches kv-core KeyGeometry's y), used for the plate
# body resonance: higher rows sit further from the stabilised bottom of the
# plate and ring at a slightly higher frequency.
ROW = {}
for _keys, _row in (
    (["Escape", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
      "PrintScreen", "ScrollLock", "Pause"], 0),
    (["Grave", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
      "Digit8", "Digit9", "Digit0", "Minus", "Equal", "Backspace",
      "Insert", "Home", "PageUp", "NumLock", "NumpadDivide", "NumpadMultiply",
      "NumpadSubtract"], 1),
    (["Tab", "Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P",
      "LeftBracket", "RightBracket", "Backslash", "Delete", "End", "PageDown",
      "Numpad7", "Numpad8", "Numpad9", "NumpadAdd"], 2),
    (["CapsLock", "A", "S", "D", "F", "G", "H", "J", "K", "L",
      "Semicolon", "Apostrophe", "Enter",
      "Numpad4", "Numpad5", "Numpad6"], 3),
    (["LeftShift", "Z", "X", "C", "V", "B", "N", "M",
      "Comma", "Period", "Slash", "RightShift",
      "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
      "Numpad1", "Numpad2", "Numpad3", "NumpadEnter"], 4),
    (["LeftCtrl", "LeftSuper", "LeftAlt", "Space", "RightAlt", "RightSuper", "Menu",
      "RightCtrl", "Numpad0", "NumpadDecimal"], 5),
):
    for _k in _keys:
        ROW[_k] = _row

# Extra keycap mass. Larger caps are driven by the same stem into a heavier
# plastic mass: lower resonance, longer ring, stabiliser rattle on top.
CAP_MASS = {
    "Space": 2.75,
    "Enter": 1.80,
    "Backspace": 1.80,
    "LeftShift": 1.80,
    "RightShift": 1.80,
    "NumpadEnter": 1.50,
    "NumpadAdd": 1.50,
    "Numpad0": 1.40,
    "Tab": 1.40,
    "CapsLock": 1.50,
    "Backslash": 1.40,
    "ArrowUp": 1.20,
    "ArrowDown": 1.20,
    "ArrowLeft": 1.20,
    "ArrowRight": 1.20,
}


def _u32(*parts: str) -> int:
    """Stable 32-bit digest of the given strings (never Python's hash())."""
    digest = hashlib.sha256("|".join(parts).encode("utf-8")).digest()
    return int.from_bytes(digest[:4], "little")


# ---------------------------------------------------------------------------
# Filters
# ---------------------------------------------------------------------------

def bandpass(x: np.ndarray, low: float, high: float, order: int = 4) -> np.ndarray:
    """Zero-phase Butterworth-magnitude band-pass, applied in the frequency domain.

    Applied offline at generation time; nothing like this runs in the product.
    """
    spectrum = np.fft.rfft(x)
    freqs = np.fft.rfftfreq(x.size, 1.0 / SAMPLE_RATE)
    with np.errstate(divide="ignore", invalid="ignore"):
        hp = 1.0 / np.sqrt(1.0 + (low / freqs) ** (2 * order))
        lp = 1.0 / np.sqrt(1.0 + (freqs / high) ** (2 * order))
    gain = np.nan_to_num(hp * lp, nan=0.0, posinf=0.0, neginf=0.0)
    return np.fft.irfft(spectrum * gain, n=x.size)


# ---------------------------------------------------------------------------
# The model
# ---------------------------------------------------------------------------

def synth_key(key: str, variant: int) -> np.ndarray:
    """One modeled press of `key`: impact, click, resonance, rattle, decay."""
    seed = _u32(MODEL_VERSION, key, str(variant))
    rng = np.random.default_rng(seed)

    row = ROW.get(key, 3)
    mass = CAP_MASS.get(key, 1.0)

    # Per-variant drift: the same switch pressed twice is never identical.
    drift = lambda lo, hi: lo + (hi - lo) * ((seed >> (variant * 8)) % 251) / 250.0
    detune = 1.0 + rng.uniform(-0.04, 0.04)
    level = rng.uniform(0.88, 1.0)
    tau_scale = rng.uniform(0.92, 1.08)

    n = int(SAMPLE_RATE * DURATION_S)
    t = np.arange(n, dtype=np.float64) / SAMPLE_RATE
    out = np.zeros(n, dtype=np.float64)

    # 1. Plate/body resonance: the "thock". Two lower modes of the plate the
    #    switch is mounted in, excited by the bottom-out impact. Deliberately
    #    held below the transient - a press is an impact with a ringing tail,
    #    not a tone with a click on top of it.
    body = (230.0 - 16.0 * row) / math.sqrt(mass)
    for multiple, amp, tau in ((1.00, 0.62, 0.030), (1.61, 0.30, 0.019), (2.72, 0.14, 0.011)):
        freq = body * multiple * detune
        phase = rng.uniform(0.0, 2.0 * math.pi)
        out += amp * np.sin(2.0 * math.pi * freq * t + phase) * np.exp(-t / (tau * mass * tau_scale))

    # 2. Keycap/stem resonance: the plastic the finger actually hits.
    cap = (1420.0 / math.sqrt(mass)) * drift(0.86, 1.16)
    for multiple, amp, tau in ((1.00, 0.34, 0.022), (1.53, 0.17, 0.013)):
        freq = cap * multiple * detune
        phase = rng.uniform(0.0, 2.0 * math.pi)
        out += amp * np.sin(2.0 * math.pi * freq * t + phase) * np.exp(-t / (tau * tau_scale))

    # 3. Bottom-out impact: broadband plastic-on-plate burst, sub-millisecond
    #    rise. This is the transient that makes a press read as a press, and
    #    it has to be the loudest thing in the clip.
    impact = bandpass(rng.standard_normal(n), 1200.0, 8000.0)
    impact *= np.exp(-t / (0.0016 * tau_scale))
    out += 2.40 * impact

    # 4. Leaf-spring click: the switch's own contact mechanism, arriving just
    #    after the stem bottoms out.
    click = bandpass(rng.standard_normal(n), 3500.0, 9500.0)
    click *= np.exp(-np.maximum(t - 0.0006, 0.0) / (0.0028 * tau_scale))
    click[t < 0.0006] = 0.0
    out += 1.35 * click

    # 5. Spring compression/rattle: short, mid-band, well below the impact.
    spring = bandpass(rng.standard_normal(n), 700.0, 2600.0)
    spring *= np.exp(-np.maximum(t - 0.004, 0.0) / 0.009)
    spring[t < 0.004] = 0.0
    out += 0.16 * spring

    # 6. Stabiliser rattle, only on the wide caps that have one.
    if mass >= 1.4:
        stab = np.zeros(n, dtype=np.float64)
        for at, amp, tau in ((0.006, 0.30, 0.004), (0.012, 0.20, 0.005)):
            burst = bandpass(rng.standard_normal(n), 2000.0, 6000.0)
            burst *= np.exp(-np.maximum(t - at, 0.0) / tau)
            burst[t < at] = 0.0
            stab += amp * burst
        out += stab / math.sqrt(mass)

    # 7. Fast attack (a press is an impulse, not a swell) and a guaranteed
    #    silent end so the clip never starts or stops on a discontinuity.
    attack = int(0.0004 * SAMPLE_RATE)
    ramp = 0.5 - 0.5 * np.cos(np.linspace(0.0, math.pi, attack))
    out[:attack] *= ramp
    fade = int(0.003 * SAMPLE_RATE)
    out[-fade:] *= 0.5 + 0.5 * np.cos(np.linspace(0.0, math.pi, fade))

    out *= level
    peak = float(np.max(np.abs(out)))
    if peak <= 0.0:
        raise RuntimeError(f"{key}-{variant} synthesized to silence")
    out *= PEAK_TARGET / peak
    return out


def write_wav(path: Path, samples: np.ndarray) -> None:
    pcm = np.round(np.clip(samples, -1.0, 1.0) * 32767.0).astype(np.int16)
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(SAMPLE_RATE)
        handle.writeframes(pcm.tobytes())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parent.parent
        / "assets"
        / "soundpacks"
        / "default-src"
        / "sounds",
        help="directory to write the WAVs into",
    )
    parser.add_argument(
        "--keys",
        default=None,
        help="comma-separated PhysicalKey names (default: every key)",
    )
    parser.add_argument("--variants", type=int, default=2, help="variants per key")
    args = parser.parse_args()

    keys = PHYSICAL_KEYS if not args.keys else [k.strip() for k in args.keys.split(",")]
    unknown = [k for k in keys if k not in ROW]
    if unknown:
        print(f"error: unknown key name(s): {', '.join(unknown)}", file=sys.stderr)
        return 2

    written = 0
    for key in keys:
        for variant in range(args.variants):
            samples = synth_key(key, variant)
            path = args.out / f"{key.lower()}-{variant}.wav"
            write_wav(path, samples)
            written += 1

    print(
        f"{written} clip(s) written to {args.out}\n"
        f"  model:    {MODEL_VERSION} (modeled switch press, NOT a recording)\n"
        f"  keys:     {len(keys)} x {args.variants} variant(s)\n"
        f"  format:   {SAMPLE_RATE} Hz mono i16, {DURATION_S * 1000:.0f} ms"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
