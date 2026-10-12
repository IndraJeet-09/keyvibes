//! Linux KeyCode to PhysicalKey mapping.
//!
//! This module provides the canonical mapping from Linux input keycodes
//! to our portable PhysicalKey representation.

use evdev::KeyCode as Key;
use kv_core::PhysicalKey;

/// Maps a Linux KeyCode to a PhysicalKey.
///
/// Returns None for unsupported or unknown keys.
pub fn map_keycode(key: Key) -> Option<PhysicalKey> {
    match key {
        // Function keys
        Key::KEY_ESC => Some(PhysicalKey::Escape),
        Key::KEY_F1 => Some(PhysicalKey::F1),
        Key::KEY_F2 => Some(PhysicalKey::F2),
        Key::KEY_F3 => Some(PhysicalKey::F3),
        Key::KEY_F4 => Some(PhysicalKey::F4),
        Key::KEY_F5 => Some(PhysicalKey::F5),
        Key::KEY_F6 => Some(PhysicalKey::F6),
        Key::KEY_F7 => Some(PhysicalKey::F7),
        Key::KEY_F8 => Some(PhysicalKey::F8),
        Key::KEY_F9 => Some(PhysicalKey::F9),
        Key::KEY_F10 => Some(PhysicalKey::F10),
        Key::KEY_F11 => Some(PhysicalKey::F11),
        Key::KEY_F12 => Some(PhysicalKey::F12),

        // Number row
        Key::KEY_GRAVE => Some(PhysicalKey::Grave),
        Key::KEY_1 => Some(PhysicalKey::Digit1),
        Key::KEY_2 => Some(PhysicalKey::Digit2),
        Key::KEY_3 => Some(PhysicalKey::Digit3),
        Key::KEY_4 => Some(PhysicalKey::Digit4),
        Key::KEY_5 => Some(PhysicalKey::Digit5),
        Key::KEY_6 => Some(PhysicalKey::Digit6),
        Key::KEY_7 => Some(PhysicalKey::Digit7),
        Key::KEY_8 => Some(PhysicalKey::Digit8),
        Key::KEY_9 => Some(PhysicalKey::Digit9),
        Key::KEY_0 => Some(PhysicalKey::Digit0),
        Key::KEY_MINUS => Some(PhysicalKey::Minus),
        Key::KEY_EQUAL => Some(PhysicalKey::Equal),
        Key::KEY_BACKSPACE => Some(PhysicalKey::Backspace),

        // Top row
        Key::KEY_TAB => Some(PhysicalKey::Tab),
        Key::KEY_Q => Some(PhysicalKey::Q),
        Key::KEY_W => Some(PhysicalKey::W),
        Key::KEY_E => Some(PhysicalKey::E),
        Key::KEY_R => Some(PhysicalKey::R),
        Key::KEY_T => Some(PhysicalKey::T),
        Key::KEY_Y => Some(PhysicalKey::Y),
        Key::KEY_U => Some(PhysicalKey::U),
        Key::KEY_I => Some(PhysicalKey::I),
        Key::KEY_O => Some(PhysicalKey::O),
        Key::KEY_P => Some(PhysicalKey::P),
        Key::KEY_LEFTBRACE => Some(PhysicalKey::LeftBracket),
        Key::KEY_RIGHTBRACE => Some(PhysicalKey::RightBracket),
        Key::KEY_BACKSLASH => Some(PhysicalKey::Backslash),

        // Home row
        Key::KEY_CAPSLOCK => Some(PhysicalKey::CapsLock),
        Key::KEY_A => Some(PhysicalKey::A),
        Key::KEY_S => Some(PhysicalKey::S),
        Key::KEY_D => Some(PhysicalKey::D),
        Key::KEY_F => Some(PhysicalKey::F),
        Key::KEY_G => Some(PhysicalKey::G),
        Key::KEY_H => Some(PhysicalKey::H),
        Key::KEY_J => Some(PhysicalKey::J),
        Key::KEY_K => Some(PhysicalKey::K),
        Key::KEY_L => Some(PhysicalKey::L),
        Key::KEY_SEMICOLON => Some(PhysicalKey::Semicolon),
        Key::KEY_APOSTROPHE => Some(PhysicalKey::Apostrophe),
        Key::KEY_ENTER => Some(PhysicalKey::Enter),

        // Bottom row
        Key::KEY_LEFTSHIFT => Some(PhysicalKey::LeftShift),
        Key::KEY_Z => Some(PhysicalKey::Z),
        Key::KEY_X => Some(PhysicalKey::X),
        Key::KEY_C => Some(PhysicalKey::C),
        Key::KEY_V => Some(PhysicalKey::V),
        Key::KEY_B => Some(PhysicalKey::B),
        Key::KEY_N => Some(PhysicalKey::N),
        Key::KEY_M => Some(PhysicalKey::M),
        Key::KEY_COMMA => Some(PhysicalKey::Comma),
        Key::KEY_DOT => Some(PhysicalKey::Period),
        Key::KEY_SLASH => Some(PhysicalKey::Slash),
        Key::KEY_RIGHTSHIFT => Some(PhysicalKey::RightShift),

        // Space row
        Key::KEY_LEFTCTRL => Some(PhysicalKey::LeftCtrl),
        Key::KEY_LEFTMETA => Some(PhysicalKey::LeftSuper),
        Key::KEY_LEFTALT => Some(PhysicalKey::LeftAlt),
        Key::KEY_SPACE => Some(PhysicalKey::Space),
        Key::KEY_RIGHTALT => Some(PhysicalKey::RightAlt),
        Key::KEY_RIGHTMETA => Some(PhysicalKey::RightSuper),
        Key::KEY_COMPOSE => Some(PhysicalKey::Menu),
        Key::KEY_RIGHTCTRL => Some(PhysicalKey::RightCtrl),

        // Navigation cluster
        Key::KEY_INSERT => Some(PhysicalKey::Insert),
        Key::KEY_DELETE => Some(PhysicalKey::Delete),
        Key::KEY_HOME => Some(PhysicalKey::Home),
        Key::KEY_END => Some(PhysicalKey::End),
        Key::KEY_PAGEUP => Some(PhysicalKey::PageUp),
        Key::KEY_PAGEDOWN => Some(PhysicalKey::PageDown),

        // Arrow keys
        Key::KEY_UP => Some(PhysicalKey::ArrowUp),
        Key::KEY_DOWN => Some(PhysicalKey::ArrowDown),
        Key::KEY_LEFT => Some(PhysicalKey::ArrowLeft),
        Key::KEY_RIGHT => Some(PhysicalKey::ArrowRight),

        // Numpad
        Key::KEY_NUMLOCK => Some(PhysicalKey::NumLock),
        Key::KEY_KPSLASH => Some(PhysicalKey::NumpadDivide),
        Key::KEY_KPASTERISK => Some(PhysicalKey::NumpadMultiply),
        Key::KEY_KPMINUS => Some(PhysicalKey::NumpadSubtract),
        Key::KEY_KP7 => Some(PhysicalKey::Numpad7),
        Key::KEY_KP8 => Some(PhysicalKey::Numpad8),
        Key::KEY_KP9 => Some(PhysicalKey::Numpad9),
        Key::KEY_KPPLUS => Some(PhysicalKey::NumpadAdd),
        Key::KEY_KP4 => Some(PhysicalKey::Numpad4),
        Key::KEY_KP5 => Some(PhysicalKey::Numpad5),
        Key::KEY_KP6 => Some(PhysicalKey::Numpad6),
        Key::KEY_KP1 => Some(PhysicalKey::Numpad1),
        Key::KEY_KP2 => Some(PhysicalKey::Numpad2),
        Key::KEY_KP3 => Some(PhysicalKey::Numpad3),
        Key::KEY_KPENTER => Some(PhysicalKey::NumpadEnter),
        Key::KEY_KP0 => Some(PhysicalKey::Numpad0),
        Key::KEY_KPDOT => Some(PhysicalKey::NumpadDecimal),

        // Print Screen, Scroll Lock, Pause
        Key::KEY_SYSRQ => Some(PhysicalKey::PrintScreen),
        Key::KEY_SCROLLLOCK => Some(PhysicalKey::ScrollLock),
        Key::KEY_PAUSE => Some(PhysicalKey::Pause),

        // Unsupported or unknown keys
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alpha_keys() {
        assert_eq!(map_keycode(Key::KEY_A), Some(PhysicalKey::A));
        assert_eq!(map_keycode(Key::KEY_Z), Some(PhysicalKey::Z));
        assert_eq!(map_keycode(Key::KEY_M), Some(PhysicalKey::M));
    }

    #[test]
    fn test_number_keys() {
        assert_eq!(map_keycode(Key::KEY_1), Some(PhysicalKey::Digit1));
        assert_eq!(map_keycode(Key::KEY_0), Some(PhysicalKey::Digit0));
    }

    #[test]
    fn test_function_keys() {
        assert_eq!(map_keycode(Key::KEY_F1), Some(PhysicalKey::F1));
        assert_eq!(map_keycode(Key::KEY_F12), Some(PhysicalKey::F12));
    }

    #[test]
    fn test_modifier_keys() {
        assert_eq!(
            map_keycode(Key::KEY_LEFTSHIFT),
            Some(PhysicalKey::LeftShift)
        );
        assert_eq!(
            map_keycode(Key::KEY_RIGHTSHIFT),
            Some(PhysicalKey::RightShift)
        );
        assert_eq!(map_keycode(Key::KEY_LEFTCTRL), Some(PhysicalKey::LeftCtrl));
        assert_eq!(map_keycode(Key::KEY_LEFTALT), Some(PhysicalKey::LeftAlt));
    }

    #[test]
    fn test_special_keys() {
        assert_eq!(map_keycode(Key::KEY_SPACE), Some(PhysicalKey::Space));
        assert_eq!(map_keycode(Key::KEY_ENTER), Some(PhysicalKey::Enter));
        assert_eq!(
            map_keycode(Key::KEY_BACKSPACE),
            Some(PhysicalKey::Backspace)
        );
        assert_eq!(map_keycode(Key::KEY_TAB), Some(PhysicalKey::Tab));
        assert_eq!(map_keycode(Key::KEY_ESC), Some(PhysicalKey::Escape));
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(map_keycode(Key::KEY_UP), Some(PhysicalKey::ArrowUp));
        assert_eq!(map_keycode(Key::KEY_DOWN), Some(PhysicalKey::ArrowDown));
        assert_eq!(map_keycode(Key::KEY_LEFT), Some(PhysicalKey::ArrowLeft));
        assert_eq!(map_keycode(Key::KEY_RIGHT), Some(PhysicalKey::ArrowRight));
    }

    #[test]
    fn test_numpad() {
        assert_eq!(map_keycode(Key::KEY_KP0), Some(PhysicalKey::Numpad0));
        assert_eq!(map_keycode(Key::KEY_KP9), Some(PhysicalKey::Numpad9));
        assert_eq!(
            map_keycode(Key::KEY_KPENTER),
            Some(PhysicalKey::NumpadEnter)
        );
    }

    #[test]
    fn test_unknown_key() {
        // BTN_LEFT is a mouse button, should not map to a keyboard key
        assert_eq!(map_keycode(Key::BTN_LEFT), None);
    }

    /// The complete evdev -> `PhysicalKey` table, asserted in both directions.
    ///
    /// This is the regression that says "every key the enum can name has a
    /// keycode and every keycode in the table resolves": a key added to
    /// `PhysicalKey` without a mapping, or a mapping silently dropped, fails
    /// here instead of making no sound at runtime.
    #[test]
    fn every_physical_key_maps_from_exactly_one_evdev_keycode() {
        const TABLE: &[(Key, PhysicalKey)] = &[
            (Key::KEY_ESC, PhysicalKey::Escape),
            (Key::KEY_F1, PhysicalKey::F1),
            (Key::KEY_F2, PhysicalKey::F2),
            (Key::KEY_F3, PhysicalKey::F3),
            (Key::KEY_F4, PhysicalKey::F4),
            (Key::KEY_F5, PhysicalKey::F5),
            (Key::KEY_F6, PhysicalKey::F6),
            (Key::KEY_F7, PhysicalKey::F7),
            (Key::KEY_F8, PhysicalKey::F8),
            (Key::KEY_F9, PhysicalKey::F9),
            (Key::KEY_F10, PhysicalKey::F10),
            (Key::KEY_F11, PhysicalKey::F11),
            (Key::KEY_F12, PhysicalKey::F12),
            (Key::KEY_GRAVE, PhysicalKey::Grave),
            (Key::KEY_1, PhysicalKey::Digit1),
            (Key::KEY_2, PhysicalKey::Digit2),
            (Key::KEY_3, PhysicalKey::Digit3),
            (Key::KEY_4, PhysicalKey::Digit4),
            (Key::KEY_5, PhysicalKey::Digit5),
            (Key::KEY_6, PhysicalKey::Digit6),
            (Key::KEY_7, PhysicalKey::Digit7),
            (Key::KEY_8, PhysicalKey::Digit8),
            (Key::KEY_9, PhysicalKey::Digit9),
            (Key::KEY_0, PhysicalKey::Digit0),
            (Key::KEY_MINUS, PhysicalKey::Minus),
            (Key::KEY_EQUAL, PhysicalKey::Equal),
            (Key::KEY_BACKSPACE, PhysicalKey::Backspace),
            (Key::KEY_TAB, PhysicalKey::Tab),
            (Key::KEY_Q, PhysicalKey::Q),
            (Key::KEY_W, PhysicalKey::W),
            (Key::KEY_E, PhysicalKey::E),
            (Key::KEY_R, PhysicalKey::R),
            (Key::KEY_T, PhysicalKey::T),
            (Key::KEY_Y, PhysicalKey::Y),
            (Key::KEY_U, PhysicalKey::U),
            (Key::KEY_I, PhysicalKey::I),
            (Key::KEY_O, PhysicalKey::O),
            (Key::KEY_P, PhysicalKey::P),
            (Key::KEY_LEFTBRACE, PhysicalKey::LeftBracket),
            (Key::KEY_RIGHTBRACE, PhysicalKey::RightBracket),
            (Key::KEY_BACKSLASH, PhysicalKey::Backslash),
            (Key::KEY_CAPSLOCK, PhysicalKey::CapsLock),
            (Key::KEY_A, PhysicalKey::A),
            (Key::KEY_S, PhysicalKey::S),
            (Key::KEY_D, PhysicalKey::D),
            (Key::KEY_F, PhysicalKey::F),
            (Key::KEY_G, PhysicalKey::G),
            (Key::KEY_H, PhysicalKey::H),
            (Key::KEY_J, PhysicalKey::J),
            (Key::KEY_K, PhysicalKey::K),
            (Key::KEY_L, PhysicalKey::L),
            (Key::KEY_SEMICOLON, PhysicalKey::Semicolon),
            (Key::KEY_APOSTROPHE, PhysicalKey::Apostrophe),
            (Key::KEY_ENTER, PhysicalKey::Enter),
            (Key::KEY_LEFTSHIFT, PhysicalKey::LeftShift),
            (Key::KEY_Z, PhysicalKey::Z),
            (Key::KEY_X, PhysicalKey::X),
            (Key::KEY_C, PhysicalKey::C),
            (Key::KEY_V, PhysicalKey::V),
            (Key::KEY_B, PhysicalKey::B),
            (Key::KEY_N, PhysicalKey::N),
            (Key::KEY_M, PhysicalKey::M),
            (Key::KEY_COMMA, PhysicalKey::Comma),
            (Key::KEY_DOT, PhysicalKey::Period),
            (Key::KEY_SLASH, PhysicalKey::Slash),
            (Key::KEY_RIGHTSHIFT, PhysicalKey::RightShift),
            (Key::KEY_LEFTCTRL, PhysicalKey::LeftCtrl),
            (Key::KEY_LEFTMETA, PhysicalKey::LeftSuper),
            (Key::KEY_LEFTALT, PhysicalKey::LeftAlt),
            (Key::KEY_SPACE, PhysicalKey::Space),
            (Key::KEY_RIGHTALT, PhysicalKey::RightAlt),
            (Key::KEY_RIGHTMETA, PhysicalKey::RightSuper),
            (Key::KEY_COMPOSE, PhysicalKey::Menu),
            (Key::KEY_RIGHTCTRL, PhysicalKey::RightCtrl),
            (Key::KEY_INSERT, PhysicalKey::Insert),
            (Key::KEY_DELETE, PhysicalKey::Delete),
            (Key::KEY_HOME, PhysicalKey::Home),
            (Key::KEY_END, PhysicalKey::End),
            (Key::KEY_PAGEUP, PhysicalKey::PageUp),
            (Key::KEY_PAGEDOWN, PhysicalKey::PageDown),
            (Key::KEY_UP, PhysicalKey::ArrowUp),
            (Key::KEY_DOWN, PhysicalKey::ArrowDown),
            (Key::KEY_LEFT, PhysicalKey::ArrowLeft),
            (Key::KEY_RIGHT, PhysicalKey::ArrowRight),
            (Key::KEY_NUMLOCK, PhysicalKey::NumLock),
            (Key::KEY_KPSLASH, PhysicalKey::NumpadDivide),
            (Key::KEY_KPASTERISK, PhysicalKey::NumpadMultiply),
            (Key::KEY_KPMINUS, PhysicalKey::NumpadSubtract),
            (Key::KEY_KP7, PhysicalKey::Numpad7),
            (Key::KEY_KP8, PhysicalKey::Numpad8),
            (Key::KEY_KP9, PhysicalKey::Numpad9),
            (Key::KEY_KPPLUS, PhysicalKey::NumpadAdd),
            (Key::KEY_KP4, PhysicalKey::Numpad4),
            (Key::KEY_KP5, PhysicalKey::Numpad5),
            (Key::KEY_KP6, PhysicalKey::Numpad6),
            (Key::KEY_KP1, PhysicalKey::Numpad1),
            (Key::KEY_KP2, PhysicalKey::Numpad2),
            (Key::KEY_KP3, PhysicalKey::Numpad3),
            (Key::KEY_KPENTER, PhysicalKey::NumpadEnter),
            (Key::KEY_KP0, PhysicalKey::Numpad0),
            (Key::KEY_KPDOT, PhysicalKey::NumpadDecimal),
            (Key::KEY_SYSRQ, PhysicalKey::PrintScreen),
            (Key::KEY_SCROLLLOCK, PhysicalKey::ScrollLock),
            (Key::KEY_PAUSE, PhysicalKey::Pause),
        ];

        assert_eq!(
            TABLE.len(),
            PhysicalKey::COUNT,
            "the table has {} entries, PhysicalKey::COUNT is {}",
            TABLE.len(),
            PhysicalKey::COUNT
        );

        let mut seen = std::collections::HashSet::new();
        for (code, key) in TABLE {
            assert_eq!(
                map_keycode(*code),
                Some(*key),
                "{code:?} must map to {key:?}"
            );
            assert!(seen.insert(*key), "duplicate entry for {key:?}");
        }

        for id in 0..PhysicalKey::COUNT as u16 {
            let key = PhysicalKey::from_u16(id).expect("id < COUNT");
            assert!(
                seen.contains(&key),
                "{key:?} has no evdev keycode - pressing it would be silently dropped"
            );
        }
    }

    /// One key per category, named the way the product describes them.
    #[test]
    fn representative_keys_from_every_category_map_correctly() {
        let representative = [
            (Key::KEY_A, PhysicalKey::A),
            (Key::KEY_Z, PhysicalKey::Z),
            (Key::KEY_M, PhysicalKey::M),
            (Key::KEY_1, PhysicalKey::Digit1),
            (Key::KEY_0, PhysicalKey::Digit0),
            (Key::KEY_SEMICOLON, PhysicalKey::Semicolon),
            (Key::KEY_APOSTROPHE, PhysicalKey::Apostrophe),
            (Key::KEY_COMMA, PhysicalKey::Comma),
            (Key::KEY_SLASH, PhysicalKey::Slash),
            (Key::KEY_GRAVE, PhysicalKey::Grave),
            (Key::KEY_LEFTSHIFT, PhysicalKey::LeftShift),
            (Key::KEY_RIGHTCTRL, PhysicalKey::RightCtrl),
            (Key::KEY_LEFTMETA, PhysicalKey::LeftSuper),
            (Key::KEY_SPACE, PhysicalKey::Space),
            (Key::KEY_ENTER, PhysicalKey::Enter),
            (Key::KEY_BACKSPACE, PhysicalKey::Backspace),
            (Key::KEY_TAB, PhysicalKey::Tab),
            (Key::KEY_ESC, PhysicalKey::Escape),
            (Key::KEY_UP, PhysicalKey::ArrowUp),
            (Key::KEY_LEFT, PhysicalKey::ArrowLeft),
            (Key::KEY_F1, PhysicalKey::F1),
            (Key::KEY_F12, PhysicalKey::F12),
            (Key::KEY_HOME, PhysicalKey::Home),
            (Key::KEY_KPENTER, PhysicalKey::NumpadEnter),
        ];
        for (code, key) in representative {
            assert_eq!(map_keycode(code), Some(key), "{code:?}");
        }
    }

    /// A keycode outside the table must stay unmapped - it must never resolve
    /// to some other key (A in particular) and play that key's sound.
    #[test]
    fn an_unmapped_keycode_never_resolves_to_some_other_key() {
        // F13..F24, consumer keys and mouse buttons are all real evdev
        // keycodes that `PhysicalKey` deliberately does not model.
        for code in [
            Key::KEY_F13,
            Key::KEY_F14,
            Key::KEY_F24,
            Key::KEY_MUTE,
            Key::KEY_PLAYPAUSE,
            Key::KEY_HANGEUL,
            Key::BTN_LEFT,
            Key::BTN_RIGHT,
        ] {
            assert_eq!(map_keycode(code), None, "{code:?} must stay unmapped");
            assert_ne!(
                map_keycode(code),
                Some(PhysicalKey::A),
                "{code:?} resolved to A - an unmapped key must never borrow A's sound"
            );
        }
    }
}
