//! User configuration file.
//!
//! One TOML file at `$XDG_CONFIG_HOME/keyvibes/config.toml` (or
//! `$KEYVIBES_CONFIG` when set). Everything a user can change about how
//! KeyVibes behaves lives here; nothing here is read from a desktop
//! environment or a portal.
//!
//! Design rules:
//!
//! * **backward compatible** - a missing `version` means the earliest
//!   schema, every field has a default, and unknown keys are ignored, so an
//!   old file keeps working after an upgrade.
//! * **safe against malformed values** - every field is validated on load
//!   and a bad value produces a message naming the file, the field, and the
//!   reason instead of a half-applied configuration.
//! * **atomic** - saves write a temporary file and rename it into place, so
//!   a crash mid-save can never truncate the user's configuration.

use kv_core::Settings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Schema version written by this build.
pub const CONFIG_VERSION: u32 = 1;

/// Errors produced while locating, reading, or validating a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read or written.
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying OS error.
        source: std::io::Error,
    },
    /// The file is not valid TOML, or is not the expected shape.
    Parse {
        /// File involved.
        path: PathBuf,
        /// What is wrong with the contents.
        reason: String,
    },
    /// A field held a value that cannot be used.
    Invalid {
        /// File involved.
        path: PathBuf,
        /// Field that failed validation.
        field: String,
        /// Why the value was rejected.
        reason: String,
    },
    /// The file was written by a newer KeyVibes than this one.
    TooNew {
        /// File involved.
        path: PathBuf,
        /// Schema version the file declares.
        found: u32,
        /// Highest schema version this build understands.
        supported: u32,
    },
    /// No config location could be derived from the environment.
    NoLocation,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "cannot read or write {}: {source}", path.display())
            }
            ConfigError::Parse { path, reason } => {
                write!(f, "invalid config {}: {reason}", path.display())
            }
            ConfigError::Invalid {
                path,
                field,
                reason,
            } => write!(f, "invalid config {}: {field} {reason}", path.display()),
            ConfigError::TooNew {
                path,
                found,
                supported,
            } => write!(
                f,
                "config {} declares schema version {found}, but this KeyVibes \
                 only understands up to {supported}; upgrade KeyVibes or \
                 lower the `version` field",
                path.display()
            ),
            ConfigError::NoLocation => write!(
                f,
                "no configuration directory: set $KEYVIBES_CONFIG or $XDG_CONFIG_HOME"
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The on-disk configuration.
///
/// Every field is optional on disk: absent values fall back to the
/// documented default, so a file that only sets `volume` is valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version. See [`CONFIG_VERSION`].
    pub version: u32,
    /// Master switch; `false` means KeyVibes refuses to start the engine.
    pub enabled: bool,
    /// Pack to load: a name discovered by `keyvibes pack list`, or a path.
    ///
    /// `None` means "first installed pack".
    pub pack: Option<String>,
    /// PipeWire node name or object serial to route output to.
    ///
    /// `None` means the session manager's default sink.
    pub output_device: Option<String>,
    /// Master volume, linear, `0.0 ..= 1.0`.
    pub volume: f32,
    /// Randomise pitch per press.
    pub pitch_variation: bool,
    /// Randomise gain per press.
    pub gain_variation: bool,
    /// Play a sound when a key is released.
    pub release_sounds: bool,
    /// Pan sounds across the stereo field.
    pub spatial_audio: bool,
}

impl Default for Config {
    fn default() -> Self {
        let settings = Settings::default();
        Self {
            version: CONFIG_VERSION,
            enabled: true,
            pack: None,
            output_device: None,
            volume: settings.volume,
            pitch_variation: settings.pitch_variation_enabled,
            gain_variation: settings.gain_variation_enabled,
            release_sounds: settings.release_sounds_enabled,
            spatial_audio: settings.spatial_audio_enabled,
        }
    }
}

impl Config {
    /// Where the configuration would be read from right now.
    ///
    /// `$KEYVIBES_CONFIG` wins, then `$XDG_CONFIG_HOME`, then
    /// `~/.config`. Returns [`ConfigError::NoLocation`] when none of them
    /// can be resolved.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        if let Some(path) = std::env::var_os("KEYVIBES_CONFIG") {
            return Ok(PathBuf::from(path));
        }
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                return Ok(dir.join("keyvibes").join("config.toml"));
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home)
                .join(".config")
                .join("keyvibes")
                .join("config.toml"));
        }
        Err(ConfigError::NoLocation)
    }

    /// Loads the configuration from an explicit path.
    ///
    /// A missing file is not an error: it means "everything at its default",
    /// which is exactly what a fresh install has.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        Self::parse(path, &text)
    }

    /// Parses and validates configuration text.
    fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|error| ConfigError::Parse {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        config.validate(path)?;
        Ok(config)
    }

    /// Checks every field against its allowed range.
    ///
    /// Called on every load, so a hand-edited file cannot reach the engine
    /// with a value it cannot honour.
    pub fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        let invalid = |field: &str, reason: String| ConfigError::Invalid {
            path: path.to_path_buf(),
            field: field.to_string(),
            reason,
        };

        if self.version > CONFIG_VERSION {
            return Err(ConfigError::TooNew {
                path: path.to_path_buf(),
                found: self.version,
                supported: CONFIG_VERSION,
            });
        }

        if !self.volume.is_finite() {
            return Err(invalid(
                "`volume`",
                format!("must be a finite number, found {}", self.volume),
            ));
        }
        if !(0.0..=1.0).contains(&self.volume) {
            return Err(invalid(
                "`volume`",
                format!("must be between 0.0 and 1.0, found {}", self.volume),
            ));
        }

        for (field, value) in [
            ("`pack`", self.pack.as_deref()),
            ("`output_device`", self.output_device.as_deref()),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(invalid(field, "must not be empty".to_string()));
                }
            }
        }

        Ok(())
    }

    /// Writes the configuration atomically, creating parent directories.
    ///
    /// The text is written to a sibling temporary file and renamed, so a
    /// crash can leave either the old file or the new one - never a
    /// half-written one.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate(path)?;

        let text = toml::to_string_pretty(self).map_err(|error| ConfigError::Parse {
            path: path.to_path_buf(),
            reason: format!("cannot serialize: {error}"),
        })?;

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");
        let temp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));

        std::fs::write(&temp, text).map_err(|source| ConfigError::Io {
            path: temp.clone(),
            source,
        })?;
        if let Err(source) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
        Ok(())
    }

    /// Converts the configuration into engine settings.
    pub fn settings(&self) -> Settings {
        Settings {
            volume: self.volume,
            release_sounds_enabled: self.release_sounds,
            pitch_variation_enabled: self.pitch_variation,
            gain_variation_enabled: self.gain_variation,
            spatial_audio_enabled: self.spatial_audio,
        }
    }
}
