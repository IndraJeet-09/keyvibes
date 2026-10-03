//! Pack-backed sound source.
//!
//! [`PackPlayer`] turns key presses into [`kv_core::PlayCommand`] values by
//! consulting an immutable, memory-mapped [`KvPack`]. It is shared (via
//! `Arc`) by every input thread; variant rotation state stays thread-local.

use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_pack::KvPack;
use std::sync::Arc;

/// Plays sounds from a loaded KVPack.
///
/// Construction performs no real-time work; [`SoundSource::play`] is a binary
/// search over validated pack data with no allocation, locking, or I/O.
pub struct PackPlayer {
    pack: Arc<KvPack>,
    output_rate: u32,
    left_gain: f32,
    right_gain: f32,
}

impl PackPlayer {
    /// Creates a player for `pack` rendering at `output_rate`.
    pub fn new(pack: Arc<KvPack>, output_rate: u32) -> Self {
        Self {
            pack,
            output_rate,
            left_gain: 1.0,
            right_gain: 1.0,
        }
    }

    /// Creates a player with explicit stereo gains (for future panning).
    pub fn with_gains(
        pack: Arc<KvPack>,
        output_rate: u32,
        left_gain: f32,
        right_gain: f32,
    ) -> Self {
        Self {
            pack,
            output_rate,
            left_gain,
            right_gain,
        }
    }

    /// The pack this player reads samples from.
    pub fn pack(&self) -> &KvPack {
        &self.pack
    }

    /// Output sample rate commands are pitched for.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }
}

impl SoundSource for PackPlayer {
    #[inline]
    fn play(&self, key: PhysicalKey, state: &mut VariantState) -> Option<PlayCommand> {
        self.pack.play_command(
            key,
            state,
            self.output_rate,
            self.left_gain,
            self.right_gain,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_pack::{ClipData, PackBuilder};

    fn build_test_pack() -> Arc<KvPack> {
        static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let mut builder = PackBuilder::new();
        builder.name = "Player Test".to_string();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![100i16; 32], 48000).unwrap(),
            )
            .unwrap();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![-100i16; 32], 48000).unwrap(),
            )
            .unwrap();

        // Unique per call: tests run in parallel and would otherwise clobber
        // each other's file.
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kvpack-player-test-{}-{id}.kvpack",
            std::process::id()
        ));
        builder.write(&path).unwrap();

        let pack = Arc::new(KvPack::open(&path).unwrap());
        let _ = std::fs::remove_file(&path);
        pack
    }

    #[test]
    fn test_play_returns_command_for_mapped_key() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        let mut state = VariantState::default();

        let cmd = player
            .play(PhysicalKey::A, &mut state)
            .expect("key A has a sound");
        assert_eq!(cmd.sample_len, 32);
        assert_eq!(cmd.source_rate, 48000);
        assert_eq!(cmd.pitch_step, 1u64 << 32); // 48000/48000
        assert!(!cmd.release);
    }

    #[test]
    fn test_play_returns_none_for_unmapped_key() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        let mut state = VariantState::default();

        assert!(player.play(PhysicalKey::B, &mut state).is_none());
    }

    #[test]
    fn test_play_rotates_variants() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        let mut state = VariantState::default();

        let first = player.play(PhysicalKey::A, &mut state).unwrap();
        let second = player.play(PhysicalKey::A, &mut state).unwrap();

        assert_ne!(
            first.sample_ptr, second.sample_ptr,
            "different clips selected"
        );
    }

    #[test]
    fn test_pitch_step_scales_for_other_output_rates() {
        let player = PackPlayer::new(build_test_pack(), 96000);
        let mut state = VariantState::default();

        let cmd = player.play(PhysicalKey::A, &mut state).unwrap();
        assert_eq!(cmd.pitch_step, (48000u64 << 32) / 96000);
    }
}
