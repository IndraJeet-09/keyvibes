//! Lock-free live settings shared between the control plane and the RT mixer.
//!
//! The control thread (unix socket, UI, `keyvibes ctl`) writes these atomics;
//! the PipeWire real-time callback reads them at the top of every render
//! block and at voice trigger time. No locks, no allocation, no blocking -
//! the RT thread only ever does relaxed atomic loads and one relaxed store
//! comparison per block.
//!
//! Master gain is stored as raw f32 bits so the mixer can compare it against
//! its ramp target without conversion. Variation is stored as the configured
//! max deviations (`cents` / `db`); `0.0` on both axes means "off", which
//! the existing `VariationConfig::enabled` path already understands.

use crate::variation::VariationConfig;
use kv_core::{GainVariation, PitchVariation, Settings};
use std::sync::atomic::{AtomicU32, Ordering};

/// Shared live settings, safe to read from the RT thread.
#[derive(Debug)]
pub struct MixerLive {
    /// Perceptual master gain as f32 bits.
    master_gain_bits: AtomicU32,
    /// Maximum pitch deviation in cents; `0.0` = pitch variation off.
    pitch_cents_bits: AtomicU32,
    /// Maximum gain deviation in decibels; `0.0` = gain variation off.
    gain_db_bits: AtomicU32,
}

impl MixerLive {
    /// Live settings seeded from `settings`.
    pub fn from_settings(settings: &Settings) -> Self {
        let config = VariationConfig::from_settings(settings);
        Self::from_parts(
            settings.perceptual_gain(),
            config.pitch.cents,
            config.gain.db,
        )
    }

    /// Live settings from explicit parts (control tests, tooling).
    pub fn from_parts(master_gain: f32, pitch_cents: f32, gain_db: f32) -> Self {
        Self {
            master_gain_bits: AtomicU32::new(master_gain.to_bits()),
            pitch_cents_bits: AtomicU32::new(pitch_cents.to_bits()),
            gain_db_bits: AtomicU32::new(gain_db.to_bits()),
        }
    }

    /// Current perceptual master gain.
    pub fn master_gain(&self) -> f32 {
        f32::from_bits(self.master_gain_bits.load(Ordering::Relaxed))
    }

    /// Updates the perceptual master gain; the mixer ramps to it.
    pub fn set_master_gain(&self, gain: f32) {
        self.master_gain_bits
            .store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Turns pitch and/or gain variation on or off, preserving the other.
    ///
    /// The magnitudes come from [`PitchVariation::default`] /
    /// [`GainVariation::default`], the same values the settings file uses.
    pub fn set_variation_flags(&self, pitch: bool, gain: bool) {
        let config = VariationConfig {
            pitch: if pitch {
                PitchVariation::default()
            } else {
                PitchVariation { cents: 0.0 }
            },
            gain: if gain {
                GainVariation::default()
            } else {
                GainVariation { db: 0.0 }
            },
        };
        self.set_variation_config(config);
    }

    /// Writes a full variation configuration.
    pub fn set_variation_config(&self, config: VariationConfig) {
        self.pitch_cents_bits
            .store(config.pitch.cents.to_bits(), Ordering::Relaxed);
        self.gain_db_bits
            .store(config.gain.db.to_bits(), Ordering::Relaxed);
    }

    /// Snapshot for the RT path: cheap, no locking.
    pub fn variation_config(&self) -> VariationConfig {
        VariationConfig {
            pitch: PitchVariation {
                cents: f32::from_bits(self.pitch_cents_bits.load(Ordering::Relaxed)),
            },
            gain: GainVariation {
                db: f32::from_bits(self.gain_db_bits.load(Ordering::Relaxed)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_round_trips() {
        let live = MixerLive::from_parts(0.0, 0.0, 0.0);
        live.set_master_gain(0.7);
        assert!((live.master_gain() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn variation_flags_are_independent() {
        let live = MixerLive::from_parts(1.0, 12.0, 3.0);
        live.set_variation_flags(true, false);
        let config = live.variation_config();
        assert!(config.pitch.cents > 0.0);
        assert_eq!(config.gain.db, 0.0);
        assert!(config.enabled());

        live.set_variation_flags(false, true);
        let config = live.variation_config();
        assert_eq!(config.pitch.cents, 0.0);
        assert!(config.gain.db > 0.0);
    }

    #[test]
    fn from_settings_matches_static_config() {
        let mut settings = Settings::new();
        settings.volume = 0.5;
        settings.gain_variation_enabled = false;
        let live = MixerLive::from_settings(&settings);
        assert!((live.master_gain() - settings.perceptual_gain()).abs() < 1e-6);
        let config = live.variation_config();
        assert!(config.pitch.cents > 0.0);
        assert_eq!(config.gain.db, 0.0);
    }
}
