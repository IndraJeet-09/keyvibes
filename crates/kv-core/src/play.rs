//! Play command and key event types.
//!
//! These types flow from the input thread to the audio engine via a lock-free queue.

use crate::physical_key::PhysicalKey;

/// Key event kind: press or release.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum KeyEventKind {
    Press = 0,
    Release = 1,
}

/// Key event from the input backend.
///
/// This is a compact, copyable event that flows from the evdev thread
/// to the key engine.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct KeyEvent {
    pub key: PhysicalKey,
    pub kind: KeyEventKind,
    pub timestamp_ns: u64,
}

impl KeyEvent {
    #[inline]
    pub fn new(key: PhysicalKey, kind: KeyEventKind, timestamp_ns: u64) -> Self {
        Self {
            key,
            kind,
            timestamp_ns,
        }
    }

    #[inline]
    pub fn is_press(&self) -> bool {
        matches!(self.kind, KeyEventKind::Press)
    }

    #[inline]
    pub fn is_release(&self) -> bool {
        matches!(self.kind, KeyEventKind::Release)
    }
}

/// Play command sent to the audio engine.
///
/// This is a fixed-size, copyable command that contains all information
/// needed to play a sound. It must remain allocation-free and suitable
/// for passing through a lock-free queue.
///
/// # Safety
///
/// The `sample_ptr` must point to valid, immutable PCM data that outlives
/// this command. The mixer assumes the pointer remains valid until the
/// voice completes playback.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PlayCommand {
    /// Pointer to the first sample of the clip.
    pub sample_ptr: *const i16,

    /// Number of samples in the clip (mono).
    pub sample_len: u32,

    /// Original sample rate of the clip.
    pub source_rate: u32,

    /// Pitch playback step in 32.32 fixed-point.
    ///
    /// This combines sample-rate conversion and pitch variation:
    /// step = (source_rate / output_rate) * pitch_ratio
    ///
    /// Stored in fixed-point for efficient fractional playback.
    pub pitch_step: u64,

    /// Left channel gain multiplier.
    pub left_gain: f32,

    /// Right channel gain multiplier.
    pub right_gain: f32,

    /// Whether this is a release sound.
    pub release: bool,
}

// SAFETY: PlayCommand is Copy and contains only primitive types and a raw pointer.
// The pointer safety is documented and enforced by the pack lifetime management.
unsafe impl Send for PlayCommand {}

impl PlayCommand {
    /// Creates a new play command.
    ///
    /// # Safety
    ///
    /// The caller must ensure `sample_ptr` points to valid memory containing
    /// at least `sample_len` samples, and that the memory remains valid for
    /// the lifetime of this command and any derived playback.
    #[inline]
    pub unsafe fn new(
        sample_ptr: *const i16,
        sample_len: u32,
        source_rate: u32,
        pitch_step: u64,
        left_gain: f32,
        right_gain: f32,
        release: bool,
    ) -> Self {
        Self {
            sample_ptr,
            sample_len,
            source_rate,
            pitch_step,
            left_gain,
            right_gain,
            release,
        }
    }
}

impl std::fmt::Debug for PlayCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayCommand")
            .field("sample_ptr", &self.sample_ptr)
            .field("sample_len", &self.sample_len)
            .field("source_rate", &self.source_rate)
            .field("pitch_step", &format!("0x{:016x}", self.pitch_step))
            .field("left_gain", &self.left_gain)
            .field("right_gain", &self.right_gain)
            .field("release", &self.release)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_event_size() {
        // Ensure KeyEvent remains compact
        assert!(std::mem::size_of::<KeyEvent>() <= 16);
    }

    #[test]
    fn test_play_command_is_copy() {
        let dummy = [0i16; 4];
        let cmd = unsafe {
            PlayCommand::new(
                dummy.as_ptr(),
                4,
                48000,
                1u64 << 32,
                1.0,
                1.0,
                false,
            )
        };
        let _cmd2 = cmd; // Copy
        let _cmd3 = cmd; // Can copy again
    }

    #[test]
    fn test_key_event_kind() {
        let press = KeyEvent::new(PhysicalKey::A, KeyEventKind::Press, 0);
        assert!(press.is_press());
        assert!(!press.is_release());

        let release = KeyEvent::new(PhysicalKey::A, KeyEventKind::Release, 0);
        assert!(release.is_release());
        assert!(!release.is_press());
    }
}
