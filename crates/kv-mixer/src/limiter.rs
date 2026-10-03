//! Soft limiter for preventing clipping.
//!
//! A lightweight, deterministic soft limiter that prevents clipping
//! without requiring look-ahead or complex compression.

/// A simple soft limiter using tanh-style saturation.
pub struct SoftLimiter {
    /// Threshold below which no limiting occurs (default: 0.9).
    pub threshold: f32,
}

impl Default for SoftLimiter {
    fn default() -> Self {
        Self { threshold: 0.9 }
    }
}

impl SoftLimiter {
    /// Creates a new soft limiter with the given threshold.
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold: threshold.clamp(0.5, 1.0),
        }
    }

    /// Applies soft limiting to a single sample.
    ///
    /// Below the threshold, the signal passes through unmodified.
    /// Above the threshold, soft saturation is applied.
    #[inline]
    pub fn process(&self, sample: f32) -> f32 {
        if sample.abs() <= self.threshold {
            sample
        } else {
            // Soft saturation using tanh-like curve
            let sign = sample.signum();
            let abs = sample.abs();
            let excess = (abs - self.threshold) / (1.0 - self.threshold);
            let limited = self.threshold + (1.0 - self.threshold) * self.soft_clip(excess);
            sign * limited
        }
    }

    /// Soft clipping function that maps [0, inf) to [0, 1).
    #[inline]
    fn soft_clip(&self, x: f32) -> f32 {
        // Use a simple rational function for soft clipping
        x / (1.0 + x)
    }

    /// Processes a stereo sample pair.
    #[inline]
    pub fn process_stereo(&self, left: f32, right: f32) -> (f32, f32) {
        (self.process(left), self.process(right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_below_threshold() {
        let limiter = SoftLimiter::default();

        // Samples below threshold should pass through unchanged
        assert_eq!(limiter.process(0.0), 0.0);
        assert_eq!(limiter.process(0.5), 0.5);
        assert_eq!(limiter.process(-0.5), -0.5);
        assert!((limiter.process(0.89) - 0.89).abs() < 0.01);
    }

    #[test]
    fn test_above_threshold() {
        let limiter = SoftLimiter::default();

        // Samples above threshold should be reduced
        let high = limiter.process(1.5);
        assert!(high < 1.5);
        assert!(high < 1.0); // Should not exceed 1.0

        let low = limiter.process(-1.5);
        assert!(low > -1.5);
        assert!(low > -1.0);
    }

    #[test]
    fn test_extreme_values() {
        let limiter = SoftLimiter::default();

        // Even extreme values should produce reasonable output
        let result = limiter.process(10.0);
        assert!(result < 1.0 && result > 0.9);

        let result = limiter.process(-10.0);
        assert!(result > -1.0 && result < -0.9);
    }

    #[test]
    fn test_finite_output() {
        let limiter = SoftLimiter::default();

        // All outputs should be finite
        for i in -100..100 {
            let input = i as f32 * 0.1;
            let output = limiter.process(input);
            assert!(output.is_finite());
        }
    }

    #[test]
    fn test_stereo() {
        let limiter = SoftLimiter::default();

        let (left, right) = limiter.process_stereo(1.5, -1.5);
        assert!(left < 1.0 && left > 0.9);
        assert!(right > -1.0 && right < -0.9);
    }
}
