//! Per-trigger pitch and gain variation.
//!
//! Repeated presses of the same key must never sound identical. The mixer
//! owns this because it is the last stage before the DAC and runs on a single
//! thread, so the rotator and generator need no atomics, locks, or allocation.
//!
//! Two mechanisms are combined:
//!
//! 1. A **preset rotation** over a small fixed table of offsets. The index
//!    comes from [`kv_core::VariantState`], which never selects the same
//!    preset twice in a row, so consecutive triggers are guaranteed to differ.
//! 2. **Deterministic jitter** from a seeded xorshift generator, so the same
//!    preset still yields slightly different values from press to press.
//!
//! Both are applied at [`Voice`](crate::voice::Voice) activation time and cost
//! nothing per rendered sample.

use kv_core::variation::rand_core::RngCore;
use kv_core::{GainVariation, PitchVariation, VariantState};

/// Number of distinct presets the rotator cycles through.
pub const VARIATION_PRESETS: u16 = 8;

/// Fraction of the configured range the random jitter may cover.
const JITTER_FRACTION: f32 = 0.15;

/// Normalised offsets in `[-1.0, 1.0]`, one per preset.
///
/// The index is the rotation value, so every preset is a distinct, audible
/// step rather than pure noise.
const PRESET_OFFSETS: [f32; VARIATION_PRESETS as usize] =
    [0.0, 0.55, -0.8, 0.95, -0.35, 0.7, -1.0, 0.25];

/// Pitch and gain variation applied to each triggered voice.
#[derive(Debug, Copy, Clone, PartialEq, Default)]
pub struct VariationConfig {
    /// Maximum pitch deviation in cents.
    pub pitch: PitchVariation,
    /// Maximum gain deviation in decibels.
    pub gain: GainVariation,
}

impl VariationConfig {
    /// A configuration that changes nothing: unity pitch and unity gain.
    pub const fn none() -> Self {
        Self {
            pitch: PitchVariation { cents: 0.0 },
            gain: GainVariation { db: 0.0 },
        }
    }

    /// Whether this configuration produces any audible change.
    pub fn enabled(&self) -> bool {
        self.pitch.cents > 0.0 || self.gain.db > 0.0
    }

    /// Derives a configuration from the user settings toggles.
    pub fn from_settings(settings: &kv_core::Settings) -> Self {
        Self {
            pitch: if settings.pitch_variation_enabled {
                PitchVariation::default()
            } else {
                PitchVariation { cents: 0.0 }
            },
            gain: if settings.gain_variation_enabled {
                GainVariation::default()
            } else {
                GainVariation { db: 0.0 }
            },
        }
    }
}

/// One applied variation: the preset that produced it and the raw offsets.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Variation {
    /// Index into the preset table, `0..VARIATION_PRESETS`.
    pub preset: u16,
    /// Signed pitch deviation in cents.
    pub pitch_cents: f32,
    /// Signed gain deviation in decibels.
    pub gain_db: f32,
}

impl Variation {
    /// The no-op variation.
    pub const IDENTITY: Self = Self {
        preset: 0,
        pitch_cents: 0.0,
        gain_db: 0.0,
    };

    /// Pitch ratio, `2^(cents / 1200)`.
    #[inline]
    pub fn pitch_ratio(&self) -> f32 {
        PitchVariation::cents_to_ratio(self.pitch_cents)
    }

    /// Linear gain multiplier, `10^(dB / 20)`.
    #[inline]
    pub fn gain(&self) -> f32 {
        GainVariation::db_to_gain(self.gain_db)
    }
}

/// xorshift32 generator: three shifts, one multiply, no state beyond a word.
#[derive(Debug, Clone, Copy)]
pub struct Rng32 {
    state: u32,
}

impl Rng32 {
    /// Creates a generator. A zero seed is mapped to a non-zero one because
    /// xorshift is degenerate at zero.
    pub fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    /// Raw 32-bit draw.
    #[inline]
    pub fn next_u32_raw(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Draw mapped to `[0.0, 1.0)`.
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        self.next_u32_raw() as f32 / (u32::MAX as f32 + 1.0)
    }
}

impl RngCore for Rng32 {
    #[inline]
    fn next_u32(&mut self) -> u32 {
        self.next_u32_raw()
    }
}

/// Per-mixer rotation and jitter source.
///
/// One instance lives on the audio thread inside [`Mixer`](crate::Mixer);
/// it is advanced exactly once per triggered voice.
#[derive(Debug, Clone, Copy)]
pub struct VariationState {
    rotation: VariantState,
    rng: Rng32,
}

impl VariationState {
    /// Creates a state seeded with `seed`.
    pub fn new(seed: u32) -> Self {
        Self {
            rotation: VariantState::default(),
            rng: Rng32::new(seed),
        }
    }

    /// The preset the previous call selected, or `0` before the first call.
    pub fn last_preset(&self) -> u16 {
        self.rotation.last_variant
    }

    /// Produces the variation for the next triggered voice.
    ///
    /// The preset index advances through [`VARIATION_PRESETS`] and never
    /// repeats consecutively; the jitter then perturbs that preset within
    /// the configured range.
    pub fn next(&mut self, config: &VariationConfig) -> Variation {
        let preset = self.rotation.select(VARIATION_PRESETS);
        let index = preset as usize % PRESET_OFFSETS.len();
        let base = PRESET_OFFSETS[index];

        if !config.enabled() {
            return Variation {
                preset,
                ..Variation::IDENTITY
            };
        }

        // A different table phase for gain keeps pitch and gain independent.
        let gain_base = PRESET_OFFSETS[(index * 5) % PRESET_OFFSETS.len()];

        let pitch_span = config.pitch.cents;
        let gain_span = config.gain.db;

        let jitter_pitch = (self.rng.next_f32() * 2.0 - 1.0) * JITTER_FRACTION * pitch_span;
        let jitter_gain = (self.rng.next_f32() * 2.0 - 1.0) * JITTER_FRACTION * gain_span;

        Variation {
            preset,
            pitch_cents: (base * pitch_span + jitter_pitch).clamp(-pitch_span, pitch_span),
            gain_db: (gain_base * gain_span + jitter_gain).clamp(-gain_span, gain_span),
        }
    }
}

impl Default for VariationState {
    fn default() -> Self {
        Self::new(0xA5A5_5A5A)
    }
}

/// Scales a 32.32 fixed-point pitch step by a ratio.
///
/// Non-finite or non-positive ratios leave the step untouched; everything else
/// is clamped to at least one output sample of advance so a voice can never
/// freeze mid-clip.
#[inline]
pub fn scale_pitch_step(step: u64, ratio: f32) -> u64 {
    if !ratio.is_finite() || ratio <= 0.0 {
        return step;
    }
    let scaled = step as f64 * ratio as f64;
    if !scaled.is_finite() {
        return step;
    }
    scaled.max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_index_rotates_without_immediate_repeats() {
        let mut state = VariationState::new(7);
        let config = VariationConfig::default();

        let mut seen = [false; VARIATION_PRESETS as usize];
        let mut previous = state.next(&config).preset;
        for _ in 0..(VARIATION_PRESETS as usize * 8) {
            let next = state.next(&config).preset;
            assert_ne!(next, previous, "preset repeated consecutively");
            previous = next;
            seen[next as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "every preset must be reached");
    }

    #[test]
    fn disabled_config_yields_identity() {
        let mut state = VariationState::new(1);
        let config = VariationConfig::none();
        for _ in 0..32 {
            let v = state.next(&config);
            assert_eq!(v.pitch_cents, 0.0);
            assert_eq!(v.gain_db, 0.0);
            assert_eq!(v.pitch_ratio(), 1.0);
            assert_eq!(v.gain(), 1.0);
        }
    }

    #[test]
    fn zero_seed_is_not_degenerate() {
        let mut rng = Rng32::new(0);
        assert_ne!(rng.next_u32_raw(), 0);
    }

    #[test]
    fn scale_pitch_step_handles_bad_ratios() {
        assert_eq!(scale_pitch_step(1 << 32, 0.0), 1 << 32);
        assert_eq!(scale_pitch_step(1 << 32, f32::NAN), 1 << 32);
        assert_eq!(scale_pitch_step(1 << 32, 2.0), 2 << 32);
    }
}
