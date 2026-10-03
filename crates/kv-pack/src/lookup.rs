//! Key lookup interface.

use kv_core::PhysicalKey;

/// Lookup result for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySounds {
    /// Physical key.
    pub key: PhysicalKey,
    /// Index of first clip in clip table.
    pub first_clip: u32,
    /// Number of variant clips.
    pub variant_count: u16,
}

/// Clip information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipInfo {
    /// Index in clip table.
    pub index: u32,
    /// Logical frame count (without guards).
    pub frames: u32,
    /// Sample offset within sample region.
    pub sample_offset: u64,
}

/// Variant selection state per key.
///
/// Re-exported from [`kv_core::VariantState`] so that the pack, the input
/// threads, and the runtime all share one type. Input threads own an array of
/// `[VariantState; PhysicalKey::COUNT]`.
pub use kv_core::VariantState;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_sounds_is_copy() {
        let sounds = KeySounds {
            key: PhysicalKey::A,
            first_clip: 0,
            variant_count: 2,
        };
        let copy = sounds;
        assert_eq!(copy.key, PhysicalKey::A);
    }

    #[test]
    fn test_variant_state_is_reexported() {
        let mut state = VariantState::default();
        assert_eq!(state.select(2), 1);
        assert_eq!(state.select(2), 0);
    }
}
