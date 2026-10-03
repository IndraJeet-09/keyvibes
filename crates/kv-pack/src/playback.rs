//! Playback command structure for mixer integration.

/// Command to play a sound clip.
///
/// This structure is passed from the loader to the mixer's RT thread.
/// All pointers must remain valid for the voice's lifetime.
#[derive(Debug, Clone, Copy)]
pub struct PlayCommand {
    /// Pointer to the first logical sample frame (after guard samples).
    pub sample_ptr: *const i16,
    /// Number of logical frames (excludes guards).
    pub frame_count: u32,
    /// Original sample rate of the clip.
    pub sample_rate: u32,
    /// Fixed-point pitch step (Q32 format).
    pub pitch_step: u64,
    /// Left channel gain [0.0, 1.0].
    pub left_gain: f32,
    /// Right channel gain [0.0, 1.0].
    pub right_gain: f32,
    /// Whether to loop (false for one-shot keyboard sounds).
    pub looping: bool,
}

unsafe impl Send for PlayCommand {}
