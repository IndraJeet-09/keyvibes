//! Variation parameters for randomizing playback.
//!
//! KeyVibes applies random pitch and gain variation to each key press
//! to make repeated sounds feel more natural and less mechanical.

/// Pitch variation in cents (1/100th of a semitone).
///
/// Default: ±35 cents.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct PitchVariation {
    /// Maximum deviation in cents (positive value).
    pub cents: f32,
}

impl PitchVariation {
    /// Default pitch variation: ±35 cents.
    pub const DEFAULT_CENTS: f32 = 35.0;

    /// Creates a new pitch variation with the given range.
    #[inline]
    pub fn new(cents: f32) -> Self {
        Self { cents: cents.abs() }
    }

    /// Converts cents to a pitch ratio.
    ///
    /// Formula: ratio = 2^(cents / 1200)
    #[inline]
    pub fn cents_to_ratio(cents: f32) -> f32 {
        2.0f32.powf(cents / 1200.0)
    }

    /// Generates a random pitch ratio within the variation range.
    ///
    /// Returns a ratio in the range [2^(-cents/1200), 2^(cents/1200)].
    #[inline]
    pub fn random_ratio(&self, rng: &mut impl rand_core::RngCore) -> f32 {

        // Generate random cents in [-cents, +cents]
        let random_u32 = rng.next_u32();
        let random_f32 = (random_u32 as f32) / (u32::MAX as f32); // [0, 1]
        let random_cents = (random_f32 * 2.0 - 1.0) * self.cents; // [-cents, +cents]

        Self::cents_to_ratio(random_cents)
    }
}

impl Default for PitchVariation {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CENTS)
    }
}

/// Gain variation in decibels.
///
/// Default: ±1.5 dB.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct GainVariation {
    /// Maximum deviation in dB (positive value).
    pub db: f32,
}

impl GainVariation {
    /// Default gain variation: ±1.5 dB.
    pub const DEFAULT_DB: f32 = 1.5;

    /// Creates a new gain variation with the given range.
    #[inline]
    pub fn new(db: f32) -> Self {
        Self { db: db.abs() }
    }

    /// Converts dB to a linear gain multiplier.
    ///
    /// Formula: gain = 10^(dB / 20)
    #[inline]
    pub fn db_to_gain(db: f32) -> f32 {
        10.0f32.powf(db / 20.0)
    }

    /// Generates a random gain multiplier within the variation range.
    ///
    /// Returns a gain in the range [10^(-dB/20), 10^(dB/20)].
    #[inline]
    pub fn random_gain(&self, rng: &mut impl rand_core::RngCore) -> f32 {

        // Generate random dB in [-db, +db]
        let random_u32 = rng.next_u32();
        let random_f32 = (random_u32 as f32) / (u32::MAX as f32); // [0, 1]
        let random_db = (random_f32 * 2.0 - 1.0) * self.db; // [-db, +db]

        Self::db_to_gain(random_db)
    }
}

impl Default for GainVariation {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DB)
    }
}

/// Combined variation parameters.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct VariationParams {
    pub pitch: PitchVariation,
    pub gain: GainVariation,
}

impl Default for VariationParams {
    fn default() -> Self {
        Self {
            pitch: PitchVariation::default(),
            gain: GainVariation::default(),
        }
    }
}

// Minimal rand_core trait for internal use
pub mod rand_core {
    pub trait RngCore {
        fn next_u32(&mut self) -> u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRng(u32);

    impl rand_core::RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            // Simple LCG for testing
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            self.0
        }
    }

    #[test]
    fn test_cents_to_ratio() {
        // 0 cents = ratio 1.0
        assert!((PitchVariation::cents_to_ratio(0.0) - 1.0).abs() < 0.001);

        // 1200 cents = 1 octave = ratio 2.0
        assert!((PitchVariation::cents_to_ratio(1200.0) - 2.0).abs() < 0.001);

        // -1200 cents = -1 octave = ratio 0.5
        assert!((PitchVariation::cents_to_ratio(-1200.0) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_db_to_gain() {
        // 0 dB = gain 1.0
        assert!((GainVariation::db_to_gain(0.0) - 1.0).abs() < 0.001);

        // 6 dB ≈ gain 2.0
        assert!((GainVariation::db_to_gain(6.0) - 2.0).abs() < 0.01);

        // -6 dB ≈ gain 0.5
        assert!((GainVariation::db_to_gain(-6.0) - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_random_variation_range() {
        let mut rng = TestRng(12345);
        let pitch = PitchVariation::new(35.0);

        // Generate many random ratios and ensure they're in range
        for _ in 0..1000 {
            let ratio = pitch.random_ratio(&mut rng);
            let min_ratio = PitchVariation::cents_to_ratio(-35.0);
            let max_ratio = PitchVariation::cents_to_ratio(35.0);
            assert!(ratio >= min_ratio && ratio <= max_ratio);
        }
    }
}
