//! Audio mixer with 32-voice polyphony.
//!
//! The mixer is the heart of the real-time audio engine. It:
//! - Manages 32 simultaneous voices
//! - Performs sample-rate conversion and pitch shifting
//! - Mixes voices into stereo output
//! - Applies master gain ramping
//! - Applies soft limiting
//!
//! The mixer is designed to be called from a real-time audio callback
//! and performs NO allocations, blocking operations, or mutex locks.

use crate::limiter::SoftLimiter;
use crate::voice::Voice;
use kv_core::PlayCommand;
use kv_ring::SpscRing;

/// Maximum number of simultaneous voices.
pub const MAX_VOICES: usize = 32;

/// Audio mixer with fixed polyphony.
pub struct Mixer {
    /// Voice array (fixed size, no allocation).
    voices: [Voice; MAX_VOICES],

    /// Monotonically increasing generation counter for voice stealing.
    generation: u64,

    /// Output sample rate.
    output_rate: u32,

    /// Current master gain.
    current_gain: f32,

    /// Target master gain (for ramping).
    target_gain: f32,

    /// Soft limiter.
    limiter: SoftLimiter,
}

impl Mixer {
    /// Creates a new mixer with the given output sample rate.
    pub fn new(output_rate: u32) -> Self {
        Self {
            voices: std::array::from_fn(|_| Voice::default()),
            generation: 0,
            output_rate,
            current_gain: 1.0,
            target_gain: 1.0,
            limiter: SoftLimiter::default(),
        }
    }

    /// Sets the target master gain.
    ///
    /// The gain will ramp smoothly to avoid zipper noise.
    pub fn set_master_gain(&mut self, gain: f32) {
        self.target_gain = gain.clamp(0.0, 2.0);
    }

    /// Returns the current master gain.
    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    /// Returns the number of currently active voices.
    pub fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Finds an inactive voice, or steals the oldest active voice.
    /// Returns the index of the allocated voice.
    fn allocate_voice(&mut self) -> usize {
        // First, try to find an inactive voice
        if let Some((idx, _)) = self.voices.iter_mut().enumerate().find(|(_, v)| !v.active) {
            return idx;
        }

        // All voices active, steal the oldest
        self.voices
            .iter()
            .enumerate()
            .min_by_key(|(_, v)| v.generation)
            .map(|(idx, _)| idx)
            .expect("MAX_VOICES > 0")
    }

    /// Triggers a new voice from a play command.
    ///
    /// This is called from the audio callback after dequeuing commands.
    pub fn trigger(&mut self, cmd: PlayCommand) {
        self.generation = self.generation.wrapping_add(1);

        let idx = self.allocate_voice();
        let generation = self.generation;

        self.voices[idx].activate(
            cmd.sample_ptr,
            cmd.sample_len,
            cmd.pitch_step,
            cmd.left_gain,
            cmd.right_gain,
            generation,
        );
    }

    /// Renders one frame (stereo sample pair) of audio.
    ///
    /// Returns (left, right).
    ///
    /// # Safety
    ///
    /// All active voices must have valid sample pointers.
    pub unsafe fn render_frame(&mut self) -> (f32, f32) {
        let mut left = 0.0f32;
        let mut right = 0.0f32;

        // Mix all active voices
        for voice in &mut self.voices {
            if voice.active {
                let (l, r) = voice.render_sample();
                left += l;
                right += r;
            }
        }

        (left, right)
    }

    /// Renders a block of audio frames.
    ///
    /// `output` is a stereo interleaved buffer: [L, R, L, R, ...]
    ///
    /// # Safety
    ///
    /// All active voices must have valid sample pointers.
    pub unsafe fn render_block(&mut self, output: &mut [f32]) {
        assert!(output.len() % 2 == 0, "Output buffer must be stereo interleaved");

        let frame_count = output.len() / 2;

        // Calculate gain ramp step
        let gain_delta = if frame_count > 0 {
            (self.target_gain - self.current_gain) / frame_count as f32
        } else {
            0.0
        };

        for i in 0..frame_count {
            // Render one frame
            let (mut left, mut right) = self.render_frame();

            // Apply ramped master gain
            let gain = self.current_gain + gain_delta * i as f32;
            left *= gain;
            right *= gain;

            // Apply soft limiting
            let (left, right) = self.limiter.process_stereo(left, right);

            // Write to output
            output[i * 2] = left;
            output[i * 2 + 1] = right;
        }

        // Update current gain
        self.current_gain = self.target_gain;
    }

    /// Processes play commands from the queue and renders a block.
    ///
    /// This is the main entry point called from the audio callback.
    ///
    /// # Safety
    ///
    /// All play commands must have valid sample pointers.
    pub unsafe fn process_block(&mut self, queue: &SpscRing<PlayCommand>, output: &mut [f32]) {
        // Drain all pending play commands
        queue.drain(|cmd| {
            self.trigger(cmd);
        });

        // Render the block
        self.render_block(output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixer_new() {
        let mixer = Mixer::new(48000);
        assert_eq!(mixer.active_voice_count(), 0);
        assert_eq!(mixer.current_gain(), 1.0);
    }

    #[test]
    fn test_set_master_gain() {
        let mut mixer = Mixer::new(48000);
        mixer.set_master_gain(0.5);
        assert_eq!(mixer.target_gain, 0.5);
    }

    #[test]
    fn test_allocate_voice() {
        let mut mixer = Mixer::new(48000);

        // First allocation should succeed
        let idx = mixer.allocate_voice();
        assert!(!mixer.voices[idx].active);

        // Fill all voices
        for i in 0..MAX_VOICES {
            mixer.voices[i].active = true;
            mixer.voices[i].generation = i as u64;
        }

        // Should steal the oldest
        let idx = mixer.allocate_voice();
        assert_eq!(mixer.voices[idx].generation, 0);
    }

    #[test]
    fn test_render_silence() {
        let mut mixer = Mixer::new(48000);
        let mut output = [0.0f32; 128];

        unsafe {
            mixer.render_block(&mut output);
        }

        // Should produce silence (or near silence due to float precision)
        for sample in output {
            assert!(sample.abs() < 0.0001);
        }
    }

    #[test]
    fn test_trigger_voice() {
        let mut mixer = Mixer::new(48000);
        let samples = [0i16, 8000, 16000, 8000, 0, 0, 0];

        let cmd = unsafe {
            PlayCommand::new(
                samples.as_ptr(),
                4,
                48000,
                1u64 << 32, // 1.0 step
                1.0,
                1.0,
                false,
            )
        };

        mixer.trigger(cmd);
        assert_eq!(mixer.active_voice_count(), 1);
    }

    #[test]
    fn test_gain_ramping() {
        let mut mixer = Mixer::new(48000);
        mixer.current_gain = 0.0;
        mixer.target_gain = 1.0;

        let samples = [16000i16; 10];
        let cmd = unsafe {
            PlayCommand::new(
                samples.as_ptr(),
                8,
                48000,
                1u64 << 32,
                1.0,
                1.0,
                false,
            )
        };

        mixer.trigger(cmd);

        let mut output = [0.0f32; 128];
        unsafe {
            mixer.render_block(&mut output);
        }

        // Gain should have ramped to target
        assert!((mixer.current_gain - 1.0).abs() < 0.01);
    }
}
