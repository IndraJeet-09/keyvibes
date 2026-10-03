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
/// This is a compact, fixed-size state for tracking recent selections
/// without allocating in the RT path.
#[derive(Debug, Default, Clone, Copy)]
pub struct VariantState {
    /// Last selected variant (used for repeat avoidance).
    pub last_variant: u16,
    /// Counter for deterministic rotation.
    pub rotation: u8,
}

impl VariantState {
    /// Selects a variant for the given number of available variants.
    ///
    /// Uses deterministic rotation to avoid immediate repeats.
    /// This is lock-free and allocation-free.
    pub fn select(&mut self, count: u16) -> u16 {
        let count = count.max(1);
        let selected = ((self.rotation as u16) % count.max(1)) as u16;
        // Update state
        self.rotation = self.rotation.wrapping_add(1);
        // Avoid same variant twice in sequence for multi-variant keys
        if count > 1 && selected == self.last_variant {
            let next_variant = (self.last_variant as u16 + 1) % count as u16;
            self.last_variant = next_variant;
            next_variant
        } else {
            self.last_variant = selected;
            selected
        }
    }
}
