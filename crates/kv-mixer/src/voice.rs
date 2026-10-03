//! Voice playback state.
//!
//! Each voice represents one currently playing sound in the mixer.

use crate::interpolation::cubic_interpolate_i16;

/// A single voice in the mixer.
///
/// Represents one currently-playing sound. The mixer maintains a fixed
/// array of voices and allocates them as sounds are triggered.
#[derive(Debug)]
pub struct Voice {
    /// Pointer to the first sample (mono i16).
    pub sample_ptr: *const i16,

    /// Total length in samples.
    pub sample_len: u32,

    /// Current playback position (32.32 fixed-point).
    pub position: u64,

    /// Playback step per output sample (32.32 fixed-point).
    ///
    /// Combines sample-rate conversion and pitch variation:
    /// step = (source_rate / output_rate) * pitch_ratio
    pub step: u64,

    /// Left channel gain.
    pub left_gain: f32,

    /// Right channel gain.
    pub right_gain: f32,

    /// Whether this voice is currently active.
    pub active: bool,

    /// Generation counter for voice stealing (higher = newer).
    pub generation: u64,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            sample_ptr: std::ptr::null(),
            sample_len: 0,
            position: 0,
            step: 0,
            left_gain: 0.0,
            right_gain: 0.0,
            active: false,
            generation: 0,
        }
    }
}

impl Voice {
    /// Resets the voice to an inactive state.
    #[inline]
    pub fn reset(&mut self) {
        self.active = false;
        self.sample_ptr = std::ptr::null();
        self.sample_len = 0;
        self.position = 0;
    }

    /// Activates the voice with new playback parameters.
    #[inline]
    pub fn activate(
        &mut self,
        sample_ptr: *const i16,
        sample_len: u32,
        step: u64,
        left_gain: f32,
        right_gain: f32,
        generation: u64,
    ) {
        self.sample_ptr = sample_ptr;
        self.sample_len = sample_len;
        self.position = 0;
        self.step = step;
        self.left_gain = left_gain;
        self.right_gain = right_gain;
        self.active = true;
        self.generation = generation;
    }

    /// Extracts the integer and fractional parts of the position.
    #[inline]
    fn position_parts(&self) -> (u32, f32) {
        let int_part = (self.position >> 32) as u32;
        let frac_part = (self.position & 0xFFFF_FFFF) as f32 / (1u64 << 32) as f32;
        (int_part, frac_part)
    }

    /// Advances the playback position.
    #[inline]
    pub fn advance(&mut self) {
        self.position = self.position.wrapping_add(self.step);
    }

    /// Checks if the voice has finished playing.
    #[inline]
    pub fn is_finished(&self) -> bool {
        let (int_pos, _) = self.position_parts();
        int_pos >= self.sample_len
    }

    /// Renders one sample from this voice into stereo output.
    ///
    /// Returns (left, right) mixed samples as f32.
    ///
    /// # Safety
    ///
    /// The caller must ensure `sample_ptr` points to valid memory with
    /// at least `sample_len + 3` samples (for interpolation guard samples).
    #[inline]
    pub unsafe fn render_sample(&mut self) -> (f32, f32) {
        let (int_pos, frac) = self.position_parts();

        // Check if we're near the end
        if int_pos + 2 >= self.sample_len {
            // Not enough samples for interpolation, voice is done
            self.active = false;
            return (0.0, 0.0);
        }

        let sample = if frac < 0.001 && self.step == (1u64 << 32) {
            // Fast path: direct copy when no resampling/pitch shift
            let s = *self.sample_ptr.add(int_pos as usize);
            s as f32 / 32768.0
        } else {
            // Cubic interpolation
            let idx = int_pos as usize;

            // Read 4 samples for interpolation
            let xm1 = if int_pos > 0 {
                *self.sample_ptr.add(idx - 1)
            } else {
                0 // Guard sample at start
            };
            let x0 = *self.sample_ptr.add(idx);
            let x1 = *self.sample_ptr.add(idx + 1);
            let x2 = *self.sample_ptr.add(idx + 2);

            cubic_interpolate_i16(xm1, x0, x1, x2, frac)
        };

        // Advance position
        self.advance();

        // Check if finished
        if self.is_finished() {
            self.active = false;
        }

        // Apply stereo gains
        let left = sample * self.left_gain;
        let right = sample * self.right_gain;

        (left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_default() {
        let voice = Voice::default();
        assert!(!voice.active);
        assert_eq!(voice.position, 0);
    }

    #[test]
    fn test_voice_activate() {
        let mut voice = Voice::default();
        let samples = [0i16, 1000, 2000, 3000, 0];

        voice.activate(
            samples.as_ptr(),
            3,
            1u64 << 32, // 1.0 step
            0.5,
            0.5,
            1,
        );

        assert!(voice.active);
        assert_eq!(voice.generation, 1);
        assert_eq!(voice.sample_len, 3);
    }

    #[test]
    fn test_voice_position() {
        let mut voice = Voice::default();
        voice.position = (1u64 << 32) | (1u64 << 31); // 1.5

        let (int_part, frac) = voice.position_parts();
        assert_eq!(int_part, 1);
        assert!((frac - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_voice_advance() {
        let mut voice = Voice::default();
        voice.step = 1u64 << 32; // 1.0

        voice.advance();
        assert_eq!(voice.position, 1u64 << 32);

        voice.advance();
        assert_eq!(voice.position, 2u64 << 32);
    }

    #[test]
    fn test_voice_is_finished() {
        let mut voice = Voice::default();
        voice.sample_len = 100;

        voice.position = 99u64 << 32;
        assert!(!voice.is_finished());

        voice.position = 100u64 << 32;
        assert!(voice.is_finished());

        voice.position = 150u64 << 32;
        assert!(voice.is_finished());
    }
}
