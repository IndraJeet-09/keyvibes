//! Cubic interpolation for high-quality sample-rate conversion.
//!
//! Uses a 4-point Hermite interpolation for smooth resampling.

/// Performs cubic (Hermite) interpolation between 4 samples.
///
/// Given samples x[-1], x[0], x[1], x[2] and a fractional position
/// t in [0, 1], returns the interpolated value.
///
/// This is a 4-point, 3rd-order interpolation that provides good
/// quality for pitch shifting and sample-rate conversion.
#[inline]
pub fn cubic_interpolate(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    // Hermite interpolation coefficients
    let c0 = x0;
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);

    // Evaluate polynomial: c0 + c1*t + c2*t^2 + c3*t^3
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Performs cubic interpolation on i16 samples.
///
/// Converts to f32, interpolates, and returns f32 for mixing.
#[inline]
pub fn cubic_interpolate_i16(xm1: i16, x0: i16, x1: i16, x2: i16, t: f32) -> f32 {
    const SCALE: f32 = 1.0 / 32768.0;

    let xm1_f = xm1 as f32 * SCALE;
    let x0_f = x0 as f32 * SCALE;
    let x1_f = x1 as f32 * SCALE;
    let x2_f = x2 as f32 * SCALE;

    cubic_interpolate(xm1_f, x0_f, x1_f, x2_f, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cubic_interpolate_at_samples() {
        let xm1 = -1.0;
        let x0 = 0.0;
        let x1 = 1.0;
        let x2 = 0.0;

        // At t=0, should return x0
        let result = cubic_interpolate(xm1, x0, x1, x2, 0.0);
        assert!((result - x0).abs() < 0.001);

        // At t=1, should return x1
        let result = cubic_interpolate(xm1, x0, x1, x2, 1.0);
        assert!((result - x1).abs() < 0.001);
    }

    #[test]
    fn test_cubic_interpolate_smooth() {
        // Interpolating a sine-like curve should be smooth
        let xm1 = 0.0;
        let x0 = 0.5;
        let x1 = 1.0;
        let x2 = 0.5;

        let v0 = cubic_interpolate(xm1, x0, x1, x2, 0.0);
        let v1 = cubic_interpolate(xm1, x0, x1, x2, 0.5);
        let v2 = cubic_interpolate(xm1, x0, x1, x2, 1.0);

        // Should be monotonically increasing
        assert!(v1 > v0);
        assert!(v2 > v1);
    }

    #[test]
    fn test_cubic_interpolate_i16() {
        let xm1 = -16384;
        let x0 = 0;
        let x1 = 16384;
        let x2 = 0;

        let result = cubic_interpolate_i16(xm1, x0, x1, x2, 0.5);

        // Should be between x0 and x1 when normalized
        assert!(result > -0.1 && result < 0.6);
    }

    #[test]
    fn test_interpolation_finite() {
        // Ensure interpolation always produces finite values
        for _ in 0..1000 {
            let result = cubic_interpolate(0.5, 0.3, -0.2, 0.1, 0.7);
            assert!(result.is_finite());
        }
    }
}
