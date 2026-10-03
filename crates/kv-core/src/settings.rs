//! User settings and configuration.

/// Application settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Master volume [0.0, 1.0].
    pub volume: f32,

    /// Enable release sounds.
    pub release_sounds_enabled: bool,

    /// Enable pitch variation.
    pub pitch_variation_enabled: bool,

    /// Enable gain variation.
    pub gain_variation_enabled: bool,

    /// Enable spatial audio (stereo panning).
    pub spatial_audio_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            release_sounds_enabled: false,
            pitch_variation_enabled: true,
            gain_variation_enabled: true,
            spatial_audio_enabled: true,
        }
    }
}

impl Settings {
    /// Creates settings with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts linear volume to perceptual gain.
    ///
    /// Uses quadratic scaling: gain = volume^2
    #[inline]
    pub fn perceptual_gain(&self) -> f32 {
        self.volume * self.volume
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = Settings::default();
        assert!(settings.volume > 0.0 && settings.volume <= 1.0);
        assert!(settings.pitch_variation_enabled);
        assert!(settings.gain_variation_enabled);
    }

    #[test]
    fn test_perceptual_gain() {
        let mut settings = Settings {
            volume: 1.0,
            ..Default::default()
        };
        assert!((settings.perceptual_gain() - 1.0).abs() < 0.001);

        settings.volume = 0.5;
        assert!((settings.perceptual_gain() - 0.25).abs() < 0.001);

        settings.volume = 0.0;
        assert!((settings.perceptual_gain() - 0.0).abs() < 0.001);
    }
}
