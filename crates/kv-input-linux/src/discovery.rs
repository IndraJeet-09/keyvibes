//! Device discovery and keyboard classification.
//!
//! This module scans /dev/input/event* and identifies keyboard devices
//! based on their capabilities, not their event number.

use crate::error::InputError;
use evdev::{Device, EventType, Key};
use std::path::PathBuf;

/// Information about a discovered keyboard device.
#[derive(Debug, Clone)]
pub struct KeyboardInfo {
    pub path: PathBuf,
    pub name: String,
    pub phys: Option<String>,
}

/// Discovers all keyboard devices on the system.
pub fn discover_keyboards() -> Result<Vec<KeyboardInfo>, InputError> {
    let mut keyboards = Vec::new();

    // Enumerate all event devices
    let devices = evdev::enumerate();

    for (path, device) in devices {
        if is_keyboard(&device) {
            keyboards.push(KeyboardInfo {
                path: path.clone(),
                name: device.name().unwrap_or("Unknown").to_string(),
                phys: device.physical_path().map(|s| s.to_string()),
            });
        }
    }

    Ok(keyboards)
}

/// Determines if a device is a keyboard based on its capabilities.
///
/// A device is considered a keyboard if it:
/// 1. Supports EV_KEY events
/// 2. Has a significant number of standard keyboard keys
/// 3. Is not a mouse, touchpad, or other input device
pub fn is_keyboard(device: &Device) -> bool {
    // Must support key events
    if !device.supported_events().contains(EventType::KEY) {
        return false;
    }

    // Get supported keys
    let keys = match device.supported_keys() {
        Some(keys) => keys,
        None => return false,
    };

    // Count keyboard-specific keys
    let mut keyboard_key_count = 0;

    // Check for standard keyboard keys
    let standard_keys = [
        Key::KEY_A, Key::KEY_B, Key::KEY_C, Key::KEY_D, Key::KEY_E,
        Key::KEY_F, Key::KEY_G, Key::KEY_H, Key::KEY_I, Key::KEY_J,
        Key::KEY_K, Key::KEY_L, Key::KEY_M, Key::KEY_N, Key::KEY_O,
        Key::KEY_P, Key::KEY_Q, Key::KEY_R, Key::KEY_S, Key::KEY_T,
        Key::KEY_U, Key::KEY_V, Key::KEY_W, Key::KEY_X, Key::KEY_Y,
        Key::KEY_Z,
        Key::KEY_1, Key::KEY_2, Key::KEY_3, Key::KEY_4, Key::KEY_5,
        Key::KEY_6, Key::KEY_7, Key::KEY_8, Key::KEY_9, Key::KEY_0,
        Key::KEY_ENTER, Key::KEY_SPACE, Key::KEY_BACKSPACE,
        Key::KEY_TAB, Key::KEY_ESC,
        Key::KEY_LEFTSHIFT, Key::KEY_RIGHTSHIFT,
        Key::KEY_LEFTCTRL, Key::KEY_RIGHTCTRL,
        Key::KEY_LEFTALT, Key::KEY_RIGHTALT,
    ];

    for key in &standard_keys {
        if keys.contains(*key) {
            keyboard_key_count += 1;
        }
    }

    // Heuristic: A keyboard should have at least 20 standard keys
    // This filters out mice, touchpads, and game controllers
    // but still accepts compact keyboards and laptop keyboards
    if keyboard_key_count < 20 {
        return false;
    }

    // Reject if device has mouse buttons as primary input
    // (some keyboards have mouse buttons, but they also have many letter keys)
    let mouse_button_count = [
        Key::BTN_LEFT, Key::BTN_RIGHT, Key::BTN_MIDDLE,
        Key::BTN_SIDE, Key::BTN_EXTRA,
    ]
    .iter()
    .filter(|btn| keys.contains(**btn))
    .count();

    // If it has many mouse buttons but few keyboard keys, it's probably a mouse
    if mouse_button_count >= 3 && keyboard_key_count < 30 {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests require mocking Device capabilities
    // For now, we test the discovery logic structure

    #[test]
    fn test_discover_keyboards_structure() {
        // This test verifies the function signature and error handling
        // Real testing requires actual hardware or mocked devices
        match discover_keyboards() {
            Ok(keyboards) => {
                // On a system with keyboards, this should find at least one
                println!("Found {} keyboards", keyboards.len());
            }
            Err(e) => {
                // Enumeration might fail in test environments
                println!("Discovery failed (expected in test): {}", e);
            }
        }
    }
}
