//! Pack-backed sound source.
//!
//! [`PackPlayer`] turns key presses into [`kv_core::PlayCommand`] values by
//! consulting an immutable, memory-mapped [`KvPack`]. It is shared (via
//! `Arc`) by every input thread; variant rotation state stays thread-local.

use kv_core::{KeyGeometry, PhysicalKey, PlayCommand, SoundSource, VariantState};
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
    spatial: bool,
}

impl PackPlayer {
    /// Creates a player for `pack` rendering at `output_rate`.
    ///
    /// Stereo spatialization is on by default; see [`spatial`](Self::spatial).
    pub fn new(pack: Arc<KvPack>, output_rate: u32) -> Self {
        Self {
            pack,
            output_rate,
            left_gain: 1.0,
            right_gain: 1.0,
            spatial: true,
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
            spatial: true,
        }
    }

    /// Enables or disables key-position stereo spatialization.
    ///
    /// When enabled, each key's horizontal position on the ANSI layout is
    /// converted to an equal-power pan before the command is queued, so sounds
    /// sit roughly where the fingers are.
    pub fn spatial(&mut self, enabled: bool) -> &mut Self {
        self.spatial = enabled;
        self
    }

    /// Whether key-position spatialization is applied.
    pub fn is_spatial(&self) -> bool {
        self.spatial
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
        let mut command = self.pack.play_command(
            key,
            state,
            self.output_rate,
            self.left_gain,
            self.right_gain,
        )?;

        if self.spatial {
            let pan = KeyGeometry::default_position(key).calculate_pan();
            let (left, right) = KeyGeometry::pan_to_gains(pan);
            command.left_gain *= left;
            command.right_gain *= right;
        }

        Some(command)
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
        for key in [PhysicalKey::Q, PhysicalKey::P] {
            builder
                .add_clip(key, ClipData::from_samples(vec![50i16; 32], 48000).unwrap())
                .unwrap();
        }

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
    fn spatial_pans_left_keys_to_the_left_channel() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        assert!(player.is_spatial(), "spatialization is on by default");
        let mut state = VariantState::default();

        let cmd = player
            .play(PhysicalKey::Q, &mut state)
            .expect("key Q has a sound");
        assert!(
            cmd.left_gain > cmd.right_gain,
            "Q sits on the left of the board: left={}, right={}",
            cmd.left_gain,
            cmd.right_gain
        );
    }

    #[test]
    fn spatial_pans_right_keys_to_the_right_channel() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        let mut state = VariantState::default();

        let cmd = player
            .play(PhysicalKey::P, &mut state)
            .expect("key P has a sound");
        assert!(
            cmd.right_gain > cmd.left_gain,
            "P sits on the right of the board: left={}, right={}",
            cmd.left_gain,
            cmd.right_gain
        );
    }

    #[test]
    fn spatial_disabled_keeps_both_channels_equal() {
        let mut player = PackPlayer::new(build_test_pack(), 48000);
        player.spatial(false);
        let mut state = VariantState::default();

        for key in [PhysicalKey::Q, PhysicalKey::P, PhysicalKey::A] {
            let cmd = player.play(key, &mut state).expect("key has a sound");
            assert_eq!(
                cmd.left_gain, cmd.right_gain,
                "spatialization must be off for {key:?}"
            );
        }
    }

    #[test]
    fn spatial_equal_power_gains_stay_below_unity_swell() {
        let player = PackPlayer::new(build_test_pack(), 48000);
        let mut state = VariantState::default();

        let cmd = player
            .play(PhysicalKey::A, &mut state)
            .expect("key A has a sound");
        assert!(
            cmd.left_gain > 0.0 && cmd.right_gain > 0.0,
            "panning must never mute a channel"
        );
        // Equal-power pan keeps the sum of squares at most 2.0 for a
        // centered source, and never exceeds the hard-panned extremes.
        let power = cmd.left_gain * cmd.left_gain + cmd.right_gain * cmd.right_gain;
        assert!(
            power <= 2.0001,
            "equal-power pan exceeded unity power: {power}"
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
