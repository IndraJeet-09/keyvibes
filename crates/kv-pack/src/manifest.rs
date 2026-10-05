//! Pack manifest (`pack.toml`) parsing and validation.
//!
//! The manifest is the single source of truth for pack contents. Physical key
//! identity is always explicit: filenames are never inspected to infer which
//! key a sample belongs to.

use crate::error::{PackError, PackResult};
use crate::format::*;
use kv_core::PhysicalKey;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Root section describing the pack itself.
#[derive(Debug, Clone, Deserialize)]
pub struct PackSection {
    /// Human readable pack name.
    pub name: String,
    /// Pack author.
    #[serde(default)]
    pub author: String,
    /// Pack version (informational; the v1 format has no version field).
    #[serde(default)]
    pub version: String,
    /// Pack description.
    #[serde(default)]
    pub description: String,
    /// License identifier.
    #[serde(default)]
    pub license: String,
    /// Source/project identifier (logical, never an absolute filesystem path).
    #[serde(default)]
    pub source: String,
    /// Required sample rate for every source clip. When omitted the rate is
    /// taken from the first clip and every other clip must match it.
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

/// Optional `[samples]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SamplesSection {
    /// Source audio format. Only `"wav"` is supported.
    #[serde(default)]
    pub format: Option<String>,
}

/// Optional `[processing]` section.
///
/// Every field is optional; omitted fields fall back to the documented
/// defaults in [`ProcessingConfig`](crate::processing::ProcessingConfig). The whole section is optional too: a
/// manifest without it builds with processing enabled and all defaults.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProcessingSection {
    /// Master switch. `false` selects the legacy raw-decode path, which is
    /// byte-for-byte the Phase 4 behavior.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Silence floor in dBFS (absolute).
    #[serde(default)]
    pub silence_threshold_dbfs: Option<f32>,
    /// Onset threshold in dB relative to the clip peak.
    #[serde(default)]
    pub onset_threshold_db: Option<f32>,
    /// Tail threshold in dB relative to the clip peak.
    #[serde(default)]
    pub tail_threshold_db: Option<f32>,
    /// Preroll kept before the detected onset, in milliseconds.
    #[serde(default)]
    pub preroll_ms: Option<f32>,
    /// Fade-out length in milliseconds.
    #[serde(default)]
    pub fadeout_ms: Option<f32>,
    /// Attack RMS window for onset confirmation, in milliseconds.
    #[serde(default)]
    pub attack_rms_window_ms: Option<f32>,
    /// Target loudness (full-clip RMS) in dB.
    #[serde(default)]
    pub loudness_target_db: Option<f32>,
    /// Final peak ceiling in dBFS.
    #[serde(default)]
    pub peak_ceiling_dbfs: Option<f32>,
    /// Minimum usable peak in dBFS.
    #[serde(default)]
    pub min_signal_dbfs: Option<f32>,
    /// Required duration of below-tail-threshold quiet before clip end, ms.
    #[serde(default)]
    pub tail_sustain_ms: Option<f32>,
    /// Maximum loudness gain in dB.
    #[serde(default)]
    pub max_gain_db: Option<f32>,
    /// Whether to apply TPDF dither before quantization.
    #[serde(default)]
    pub dither: Option<bool>,
    /// Base seed for deterministic dither.
    #[serde(default)]
    pub dither_seed: Option<u64>,
    /// Use OS entropy instead of deterministic derivation (breaks
    /// reproducible builds).
    #[serde(default)]
    pub random_dither: Option<bool>,
}

/// A `[[keys]]` entry mapping one physical key to its ordered variants.
#[derive(Debug, Clone, Deserialize)]
pub struct KeySection {
    /// Canonical physical key name (for example `"A"`, `"Space"`, `"LeftShift"`).
    pub physical_key: String,
    /// Sample paths relative to the pack root, in variant order.
    pub samples: Vec<String>,
}

/// Parsed, structurally validated manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct PackManifest {
    pub pack: PackSection,
    #[serde(default)]
    pub samples: SamplesSection,
    #[serde(default)]
    pub processing: ProcessingSection,
    pub keys: Vec<KeySection>,
}

/// A manifest entry with its physical key already resolved.
#[derive(Debug, Clone)]
pub struct ManifestKey {
    pub physical_key: PhysicalKey,
    /// Sample paths exactly as written in the manifest (relative to pack root).
    pub samples: Vec<String>,
}

impl PackManifest {
    /// Parses manifest text.
    pub fn parse(text: &str) -> PackResult<Self> {
        let manifest: PackManifest =
            toml::from_str(text).map_err(|e| PackError::ManifestError(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reads and parses a manifest file.
    pub fn load(path: &Path) -> PackResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| PackError::SourceFailed {
            file: path.display().to_string(),
            reason: format!("cannot read manifest: {e}"),
        })?;
        Self::parse(&text)
    }

    /// Validates manifest structure (independent of the filesystem).
    fn validate(&self) -> PackResult<()> {
        if self.pack.name.trim().is_empty() {
            return Err(PackError::ManifestError(
                "pack.name must not be empty".to_string(),
            ));
        }

        if let Some(format) = &self.samples.format {
            if !format.eq_ignore_ascii_case("wav") {
                return Err(PackError::UnsupportedFormat(format!(
                    "unsupported source format: {format} (only \"wav\" is supported)"
                )));
            }
        }

        if let Some(rate) = self.pack.sample_rate {
            if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate) {
                return Err(PackError::InvalidSampleRate(
                    rate,
                    MIN_SAMPLE_RATE,
                    MAX_SAMPLE_RATE,
                ));
            }
        }

        if self.keys.is_empty() {
            return Err(PackError::ManifestError(
                "manifest must define at least one [[keys]] entry".to_string(),
            ));
        }

        // Reject invalid [processing] values at parse time, even when
        // processing is disabled (a typo should never silently rot).
        self.processing_config()?;

        // Resolve keys, rejecting unknown names and duplicates explicitly.
        let mut seen: HashSet<PhysicalKey> = HashSet::new();
        for entry in &self.keys {
            let key = parse_physical_key(&entry.physical_key)?;

            if !seen.insert(key) {
                return Err(PackError::DuplicatePhysicalKey(key.to_string()));
            }

            if entry.samples.is_empty() {
                return Err(PackError::EmptyKeySamples(key.to_string()));
            }

            for sample in &entry.samples {
                if sample.trim().is_empty() {
                    return Err(PackError::ManifestError(format!(
                        "empty sample path for key {}",
                        key
                    )));
                }
            }
        }

        Ok(())
    }

    /// Returns keys with resolved `PhysicalKey` values, preserving manifest order.
    pub fn resolved_keys(&self) -> PackResult<Vec<ManifestKey>> {
        self.keys
            .iter()
            .map(|entry| {
                Ok(ManifestKey {
                    physical_key: parse_physical_key(&entry.physical_key)?,
                    samples: entry.samples.clone(),
                })
            })
            .collect()
    }

    /// Builds the effective [`ProcessingConfig`](crate::processing::ProcessingConfig) for this manifest.
    ///
    /// Omitted `[processing]` fields keep their defaults; a manifest with no
    /// `[processing]` section at all gets
    /// [`ProcessingConfig::default`](crate::processing::ProcessingConfig::default)
    /// (processing enabled). The result is already validated.
    pub fn processing_config(&self) -> PackResult<crate::processing::ProcessingConfig> {
        use crate::processing::ProcessingConfig;

        let s = &self.processing;
        let mut cfg = ProcessingConfig::default();
        if let Some(v) = s.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = s.silence_threshold_dbfs {
            cfg.silence_threshold_dbfs = v;
        }
        if let Some(v) = s.onset_threshold_db {
            cfg.onset_threshold_db = v;
        }
        if let Some(v) = s.tail_threshold_db {
            cfg.tail_threshold_db = v;
        }
        if let Some(v) = s.preroll_ms {
            cfg.preroll_ms = v;
        }
        if let Some(v) = s.fadeout_ms {
            cfg.fadeout_ms = v;
        }
        if let Some(v) = s.attack_rms_window_ms {
            cfg.attack_rms_window_ms = v;
        }
        if let Some(v) = s.loudness_target_db {
            cfg.loudness_target_db = v;
        }
        if let Some(v) = s.peak_ceiling_dbfs {
            cfg.peak_ceiling_dbfs = v;
        }
        if let Some(v) = s.min_signal_dbfs {
            cfg.min_signal_dbfs = v;
        }
        if let Some(v) = s.tail_sustain_ms {
            cfg.tail_sustain_ms = v;
        }
        if let Some(v) = s.max_gain_db {
            cfg.max_gain_db = v;
        }
        if let Some(v) = s.dither {
            cfg.dither = v;
        }
        if let Some(v) = s.dither_seed {
            cfg.dither_seed = v;
        }
        if let Some(v) = s.random_dither {
            cfg.random_dither = v;
        }
        cfg.validate()?;
        Ok(cfg)
    }
}

/// Parses a manifest physical key name into a `PhysicalKey`.
///
/// Matching is case-insensitive against the canonical variant names. A small
/// set of unambiguous aliases is accepted. Unknown names are an error.
pub fn parse_physical_key(name: &str) -> PackResult<PhysicalKey> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(PackError::UnknownPhysicalKey(name.to_string()));
    }

    if let Some(key) = lookup_physical_key(trimmed) {
        return Ok(key);
    }

    // Numeric aliases: "1" → Digit1 ... "0" → Digit0
    match trimmed.as_bytes() {
        [b'1'] => return Ok(PhysicalKey::Digit1),
        [b'2'] => return Ok(PhysicalKey::Digit2),
        [b'3'] => return Ok(PhysicalKey::Digit3),
        [b'4'] => return Ok(PhysicalKey::Digit4),
        [b'5'] => return Ok(PhysicalKey::Digit5),
        [b'6'] => return Ok(PhysicalKey::Digit6),
        [b'7'] => return Ok(PhysicalKey::Digit7),
        [b'8'] => return Ok(PhysicalKey::Digit8),
        [b'9'] => return Ok(PhysicalKey::Digit9),
        [b'0'] => return Ok(PhysicalKey::Digit0),
        _ => {}
    }

    if trimmed.eq_ignore_ascii_case("Esc") {
        return Ok(PhysicalKey::Escape);
    }

    Err(PackError::UnknownPhysicalKey(name.to_string()))
}

/// Looks up a canonical `PhysicalKey` name (case-insensitive).
fn lookup_physical_key(name: &str) -> Option<PhysicalKey> {
    (0..PhysicalKey::COUNT as u16)
        .map(|id| PhysicalKey::from_u16(id).expect("id < COUNT"))
        .find(|key| key.to_string().eq_ignore_ascii_case(name))
}

/// Resolves a manifest source path against the pack root.
///
/// # Security
///
/// The resolved file must stay inside the pack root:
/// - absolute paths are rejected,
/// - `..` components are rejected,
/// - after canonicalization (which resolves symlinks) the path must still be
///   inside the canonical pack root.
pub fn resolve_source(root: &Path, relative: &str) -> PackResult<PathBuf> {
    let rel = Path::new(relative);

    if relative.is_empty() {
        return Err(PackError::ManifestError("empty source path".to_string()));
    }

    if rel.is_absolute() || rel.has_root() {
        return Err(PackError::PathTraversal(relative.to_string()));
    }

    if rel.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(PackError::PathTraversal(relative.to_string()));
    }

    let candidate = root.join(rel);

    let canonical_root = std::fs::canonicalize(root).map_err(|e| PackError::SourceFailed {
        file: root.display().to_string(),
        reason: format!("cannot resolve pack root: {e}"),
    })?;

    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|_| PackError::SourceNotFound(relative.to_string()))?;

    if !canonical.starts_with(&canonical_root) {
        return Err(PackError::PathTraversal(relative.to_string()));
    }

    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[pack]
name = "Test Pack"
author = "KeyVibes"
version = "1.0.0"

[samples]
format = "wav"

[[keys]]
physical_key = "A"
samples = ["sounds/A/001.wav"]

[[keys]]
physical_key = "Space"
samples = ["sounds/Space/001.wav", "sounds/Space/002.wav"]
"#;

    #[test]
    fn test_parse_valid_manifest() {
        let manifest = PackManifest::parse(VALID).unwrap();
        assert_eq!(manifest.pack.name, "Test Pack");
        assert_eq!(manifest.keys.len(), 2);
        assert_eq!(manifest.keys[1].samples.len(), 2);
    }

    #[test]
    fn test_missing_pack_section() {
        let text = r#"
[[keys]]
physical_key = "A"
samples = ["a.wav"]
"#;
        assert!(PackManifest::parse(text).is_err());
    }

    #[test]
    fn test_missing_keys() {
        let text = r#"
[pack]
name = "Empty"
"#;
        assert!(PackManifest::parse(text).is_err());
    }

    #[test]
    fn test_duplicate_key_rejected() {
        let text = r#"
[pack]
name = "Dup"

[[keys]]
physical_key = "A"
samples = ["a1.wav"]

[[keys]]
physical_key = "a"
samples = ["a2.wav"]
"#;
        match PackManifest::parse(text) {
            Err(PackError::DuplicatePhysicalKey(name)) => assert_eq!(name, "A"),
            other => panic!("expected duplicate key error, got {other:?}"),
        }
    }

    #[test]
    fn test_empty_samples_rejected() {
        let text = r#"
[pack]
name = "Empty"

[[keys]]
physical_key = "A"
samples = []
"#;
        match PackManifest::parse(text) {
            Err(PackError::EmptyKeySamples(name)) => assert_eq!(name, "A"),
            other => panic!("expected empty samples error, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_key_rejected() {
        let text = r#"
[pack]
name = "Unknown"

[[keys]]
physical_key = "FooBar"
samples = ["x.wav"]
"#;
        match PackManifest::parse(text) {
            Err(PackError::UnknownPhysicalKey(name)) => assert_eq!(name, "FooBar"),
            other => panic!("expected unknown key error, got {other:?}"),
        }
    }

    #[test]
    fn test_unsupported_sample_format_rejected() {
        let text = r#"
[pack]
name = "Flac"

[samples]
format = "flac"

[[keys]]
physical_key = "A"
samples = ["a.wav"]
"#;
        assert!(matches!(
            PackManifest::parse(text),
            Err(PackError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn test_invalid_sample_rate_rejected() {
        let text = r#"
[pack]
name = "BadRate"
sample_rate = 100

[[keys]]
physical_key = "A"
samples = ["a.wav"]
"#;
        assert!(matches!(
            PackManifest::parse(text),
            Err(PackError::InvalidSampleRate(..))
        ));
    }

    #[test]
    fn test_processing_defaults_when_section_absent() {
        let manifest = PackManifest::parse(VALID).unwrap();
        let cfg = manifest.processing_config().unwrap();
        assert!(cfg.enabled, "processing is on by default");
        assert_eq!(cfg.fadeout_ms, crate::processing::DEFAULT_FADEOUT_MS);
        assert_eq!(
            cfg.loudness_target_db,
            crate::processing::DEFAULT_LOUDNESS_TARGET_DB
        );
        assert!(cfg.dither, "dither on by default");
        assert!(!cfg.random_dither);
    }

    #[test]
    fn test_processing_section_overrides() {
        let text = r#"
[pack]
name = "P"

[processing]
enabled = false
fadeout_ms = 12.5
loudness_target_db = -23.0
dither = false
dither_seed = 42

[[keys]]
physical_key = "A"
samples = ["a.wav"]
"#;
        let manifest = PackManifest::parse(text).unwrap();
        let cfg = manifest.processing_config().unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.fadeout_ms, 12.5);
        assert_eq!(cfg.loudness_target_db, -23.0);
        assert!(!cfg.dither);
        assert_eq!(cfg.dither_seed, 42);
        // Unspecified fields keep their defaults.
        assert_eq!(cfg.preroll_ms, crate::processing::DEFAULT_PREROLL_MS);
    }

    #[test]
    fn test_processing_invalid_value_rejected_at_parse_time() {
        let text = r#"
[pack]
name = "P"

[processing]
fadeout_ms = -1.0

[[keys]]
physical_key = "A"
samples = ["a.wav"]
"#;
        assert!(matches!(
            PackManifest::parse(text),
            Err(PackError::ValidationFailed(_))
        ));
    }

    #[test]
    fn test_parse_physical_key_canonical_names() {
        assert_eq!(parse_physical_key("A").unwrap(), PhysicalKey::A);
        assert_eq!(parse_physical_key("a").unwrap(), PhysicalKey::A);
        assert_eq!(parse_physical_key("Space").unwrap(), PhysicalKey::Space);
        assert_eq!(parse_physical_key("SPACE").unwrap(), PhysicalKey::Space);
        assert_eq!(parse_physical_key("Enter").unwrap(), PhysicalKey::Enter);
        assert_eq!(
            parse_physical_key("LeftShift").unwrap(),
            PhysicalKey::LeftShift
        );
        assert_eq!(
            parse_physical_key("rightshift").unwrap(),
            PhysicalKey::RightShift
        );
        assert_eq!(parse_physical_key("Escape").unwrap(), PhysicalKey::Escape);
        assert_eq!(parse_physical_key("Esc").unwrap(), PhysicalKey::Escape);
        assert_eq!(parse_physical_key("1").unwrap(), PhysicalKey::Digit1);
        assert_eq!(parse_physical_key("Digit1").unwrap(), PhysicalKey::Digit1);
    }

    #[test]
    fn test_parse_physical_key_unknown() {
        assert!(matches!(
            parse_physical_key("FooBar"),
            Err(PackError::UnknownPhysicalKey(_))
        ));
        assert!(matches!(
            parse_physical_key(""),
            Err(PackError::UnknownPhysicalKey(_))
        ));
        assert!(matches!(
            parse_physical_key("LeftCtrl+A"),
            Err(PackError::UnknownPhysicalKey(_))
        ));
    }

    #[test]
    fn test_resolve_source_rejects_traversal() {
        let root = std::env::temp_dir();
        assert!(matches!(
            resolve_source(&root, "../outside.wav"),
            Err(PackError::PathTraversal(_))
        ));
        assert!(matches!(
            resolve_source(&root, "/etc/passwd"),
            Err(PackError::PathTraversal(_))
        ));
        assert!(matches!(
            resolve_source(&root, "a/../../b.wav"),
            Err(PackError::PathTraversal(_))
        ));
    }

    #[test]
    fn test_resolve_source_missing_file() {
        let root = std::env::temp_dir();
        assert!(matches!(
            resolve_source(&root, "definitely-not-present-12345.wav"),
            Err(PackError::SourceNotFound(_))
        ));
    }
}
