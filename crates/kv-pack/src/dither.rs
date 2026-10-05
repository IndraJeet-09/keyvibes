//! TPDF (triangular probability density) dither for the final `f32 → i16`
//! quantization step.
//!
//! Dither is applied **only** immediately before quantization, never before
//! gain processing, never twice, and never to floating-point runtime samples
//! (the runtime only ever sees the quantized `i16` PCM stored in the pack).
//!
//! # Why TPDF
//!
//! Quantization without dither correlates the truncation error with the
//! signal (audible as harmonic/distortion on quiet decays). Adding a
//! triangularly distributed noise of ±1 LSB uncorrelates the error: the sum of
//! two independent uniform variables
//!
//! ```text
//! dither = (u1 - u2) * LSB        u1, u2 ~ Uniform[0, 1)
//! ```
//!
//! has a triangular PDF spanning (-1, +1) LSB, which is the standard choice
//! for rendering to a fixed-point format.
//!
//! # Determinism
//!
//! [`TpdfDither`] is seeded explicitly. In reproducible builds the seed is
//! derived from the pack seed, key identity, and variant index
//! ([`DitherSeed::derive`]), so two builds of identical inputs produce
//! byte-identical packs. OS entropy is never used unless the caller
//! explicitly asks for [`DitherSeed::random`].

use kv_core::PhysicalKey;

/// One least-significant bit of the `i16` quantizer in the `[-1.0, +1.0]`
/// float domain: the quantizer maps `x * 32768` onto integers, so its step
/// size is `1 / 32768`.
pub(crate) const I16_LSB: f32 = 1.0 / 32768.0;

/// Seed for the dither PRNG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DitherSeed(u64);

impl DitherSeed {
    /// Wraps an explicit 64-bit seed (useful in tests).
    pub fn from_u64(seed: u64) -> Self {
        Self(seed)
    }

    /// The raw seed value.
    pub fn value(&self) -> u64 {
        self.0
    }

    /// Derives a stable seed from pack + clip identity.
    ///
    /// Uses FNV-1a over the pack seed, the physical key ID, the variant
    /// index, and the manifest-relative source path, so:
    ///
    /// - the derivation is pure and platform-independent (no `Hash` impls,
    ///   no `HashMap` iteration order, no OS entropy),
    /// - two different clips never share a dither sequence by accident,
    /// - rebuilding the same manifest always produces the same bytes.
    pub fn derive(pack_seed: u64, key: PhysicalKey, variant: u16, source: &str) -> Self {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

        let mut hash = FNV_OFFSET;
        let mut mix = |bytes: &[u8]| {
            for &b in bytes {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };

        mix(&pack_seed.to_le_bytes());
        mix(&key.as_u16().to_le_bytes());
        mix(&variant.to_le_bytes());
        mix(source.as_bytes());
        Self(hash)
    }

    /// A non-reproducible seed from OS entropy (via `RandomState`).
    ///
    /// Only used when a manifest explicitly opts into random dithering;
    /// the default build path is always [`DitherSeed::derive`].
    pub fn random() -> Self {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};

        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u8(0);
        Self(hasher.finish())
    }
}

/// Deterministic splitmix64 PRNG + TPDF dither generator.
///
/// splitmix64 is a small, well-known generator with a 64-bit state; it is
/// deterministic across platforms and needs no external crate.
#[derive(Debug, Clone)]
pub struct TpdfDither {
    state: u64,
}

impl TpdfDither {
    /// Creates a generator from a seed.
    pub fn new(seed: DitherSeed) -> Self {
        Self { state: seed.0 }
    }

    /// splitmix64 step.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform sample in `[0, 1)` with 53 bits of precision.
    fn next_uniform(&mut self) -> f64 {
        // Top 53 bits of a u64 convert to f64 without loss.
        let bits = (self.next_u64() >> 11) as f64;
        bits * (1.0 / ((1u64 << 53) as f64))
    }

    /// Draws two independent uniform values and returns their difference
    /// scaled to one `i16` LSB:
    ///
    /// ```text
    /// dither = (u1 - u2) * (1 / 32768)       ∈ (-1/32768, 1/32768)
    /// ```
    pub fn draw(&mut self) -> f32 {
        let u1 = self.next_uniform();
        let u2 = self.next_uniform();
        ((u1 - u2) * f64::from(I16_LSB)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_seed_same_sequence() {
        let mut a = TpdfDither::new(DitherSeed::from_u64(42));
        let mut b = TpdfDither::new(DitherSeed::from_u64(42));
        for _ in 0..100 {
            assert_eq!(a.draw(), b.draw());
        }
    }

    #[test]
    fn test_different_seed_different_sequence() {
        let mut a = TpdfDither::new(DitherSeed::from_u64(1));
        let mut b = TpdfDither::new(DitherSeed::from_u64(2));
        let seq_a: Vec<f32> = (0..32).map(|_| a.draw()).collect();
        let seq_b: Vec<f32> = (0..32).map(|_| b.draw()).collect();
        assert_ne!(
            seq_a, seq_b,
            "different seeds must produce different dither"
        );
    }

    #[test]
    fn test_tpdf_shape() {
        // The difference of two uniforms is triangular: it must stay within
        // ±1 LSB and be biased towards zero (mean ≈ 0, |mean| small).
        let mut d = TpdfDither::new(DitherSeed::from_u64(0xdead_beef));
        let n = 20_000;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut max_abs = 0.0f32;
        for _ in 0..n {
            let v = d.draw();
            assert!(v.abs() < I16_LSB + 1e-9, "dither out of range: {v}");
            sum += f64::from(v);
            sum_sq += f64::from(v) * f64::from(v);
            max_abs = max_abs.max(v.abs());
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64 - mean * mean;
        // Variance of (u1-u2)*step is step^2 / 6.
        let expected_var = (f64::from(I16_LSB) * f64::from(I16_LSB)) / 6.0;
        assert!(mean.abs() < 1e-7, "mean too far from zero: {mean}");
        assert!(
            (var - expected_var).abs() / expected_var < 0.05,
            "variance {var} not close to triangular {expected_var}"
        );
        // Triangular distribution: some mass away from zero (max should be a
        // decent fraction of the ±1 LSB support).
        assert!(
            max_abs > 0.5 * I16_LSB,
            "samples collapse to zero: {max_abs}"
        );
    }

    #[test]
    fn test_derive_is_stable_and_distinct() {
        let a = DitherSeed::derive(0, PhysicalKey::A, 0, "sounds/a.wav");
        let b = DitherSeed::derive(0, PhysicalKey::A, 0, "sounds/a.wav");
        assert_eq!(a, b, "derivation must be pure");

        let variants = [
            DitherSeed::derive(1, PhysicalKey::A, 0, "sounds/a.wav"),
            DitherSeed::derive(0, PhysicalKey::B, 0, "sounds/a.wav"),
            DitherSeed::derive(0, PhysicalKey::A, 1, "sounds/a.wav"),
            DitherSeed::derive(0, PhysicalKey::A, 0, "sounds/b.wav"),
        ];
        for v in variants {
            assert_ne!(a, v, "identity inputs must change the seed");
        }
    }

    #[test]
    fn test_random_seeds_differ() {
        let a = DitherSeed::random();
        let b = DitherSeed::random();
        assert_ne!(a, b, "random seeds must differ");
    }
}
