//! Physical key abstraction.
//!
//! KeyVibes uses a hardware-independent physical key enum instead of
//! exposing platform-specific keycodes throughout the engine.

use std::fmt;

/// Represents a physical key on a keyboard.
///
/// This enum abstracts away platform-specific key codes (evdev on Linux,
/// virtual key codes on macOS/Windows). The input backend translates
/// native codes to `PhysicalKey`.
///
/// Variants are ordered to match typical keyboard layouts for array indexing.
#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PhysicalKey {
    // Function row
    Escape = 0,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,

    // Number row
    Grave,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Digit0,
    Minus,
    Equal,
    Backspace,

    // Top row
    Tab,
    Q,
    W,
    E,
    R,
    T,
    Y,
    U,
    I,
    O,
    P,
    LeftBracket,
    RightBracket,
    Backslash,

    // Home row
    CapsLock,
    A,
    S,
    D,
    F,
    G,
    H,
    J,
    K,
    L,
    Semicolon,
    Apostrophe,
    Enter,

    // Bottom row
    LeftShift,
    Z,
    X,
    C,
    V,
    B,
    N,
    M,
    Comma,
    Period,
    Slash,
    RightShift,

    // Modifiers
    LeftCtrl,
    LeftSuper,
    LeftAlt,
    Space,
    RightAlt,
    RightSuper,
    Menu,
    RightCtrl,

    // Navigation cluster
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,

    // Arrow keys
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,

    // Numpad
    NumLock,
    NumpadDivide,
    NumpadMultiply,
    NumpadSubtract,
    NumpadAdd,
    NumpadEnter,
    NumpadDecimal,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,

    // Additional keys
    PrintScreen,
    ScrollLock,
    Pause,
}

impl PhysicalKey {
    /// Total number of physical keys.
    pub const COUNT: usize = 104;

    /// Converts a u16 discriminant to a PhysicalKey.
    ///
    /// Returns None if the value is out of range.
    #[inline]
    pub fn from_u16(value: u16) -> Option<Self> {
        if (value as usize) < Self::COUNT {
            // SAFETY: We've verified the value is within the valid range
            Some(unsafe { std::mem::transmute::<u16, PhysicalKey>(value) })
        } else {
            None
        }
    }

    /// Returns the discriminant as a u16.
    #[inline]
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Returns the discriminant as a usize for array indexing.
    #[inline]
    pub fn as_index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for PhysicalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_physical_key_roundtrip() {
        for i in 0..PhysicalKey::COUNT {
            let key = PhysicalKey::from_u16(i as u16).unwrap();
            assert_eq!(key.as_u16() as usize, i);
            assert_eq!(key.as_index(), i);
        }
    }

    #[test]
    fn test_physical_key_out_of_range() {
        assert!(PhysicalKey::from_u16(PhysicalKey::COUNT as u16).is_none());
        assert!(PhysicalKey::from_u16(u16::MAX).is_none());
    }

    #[test]
    fn test_modifiers_distinct() {
        // Ensure left/right modifiers are distinct
        assert_ne!(PhysicalKey::LeftShift, PhysicalKey::RightShift);
        assert_ne!(PhysicalKey::LeftCtrl, PhysicalKey::RightCtrl);
        assert_ne!(PhysicalKey::LeftAlt, PhysicalKey::RightAlt);
        assert_ne!(PhysicalKey::LeftSuper, PhysicalKey::RightSuper);
    }
}
