//! Event processing and conversion.
//!
//! This module handles evdev events and converts them into PlayCommands.

use crate::mapping::map_keycode;
use evdev::{EventType, InputEvent, KeyCode as Key};
use kv_core::PhysicalKey;

/// Result of processing an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// Key press - should generate sound
    Press(PhysicalKey),
    /// Key release - recorded but doesn't generate sound in current impl
    Release(PhysicalKey),
    /// Key repeat - explicitly ignored
    Repeat,
    /// Unknown or unsupported key
    Unknown,
    /// Synchronization event
    Sync,
    /// Event dropped by kernel
    Dropped,
    /// Other event type (not a key event)
    Other,
}

/// Processes an evdev input event.
///
/// # Returns
///
/// - `Press(key)` for key press (value == 1)
/// - `Release(key)` for key release (value == 0)
/// - `Repeat` for key repeat (value == 2)
/// - `Unknown` for unmapped keys
/// - `Sync` for SYN_REPORT
/// - `Dropped` for SYN_DROPPED
/// - `Other` for non-key events
pub fn process_event(event: &InputEvent) -> EventResult {
    match event.event_type() {
        EventType::KEY => {
            let key = Key::new(event.code());
            let value = event.value();

            match value {
                // Key press
                1 => {
                    if let Some(physical_key) = map_keycode(key) {
                        EventResult::Press(physical_key)
                    } else {
                        EventResult::Unknown
                    }
                }
                // Key release
                0 => {
                    if let Some(physical_key) = map_keycode(key) {
                        EventResult::Release(physical_key)
                    } else {
                        EventResult::Unknown
                    }
                }
                // Key repeat - explicitly ignore
                2 => EventResult::Repeat,
                // Other values - treat as unknown
                _ => EventResult::Unknown,
            }
        }
        EventType::SYNCHRONIZATION => {
            match event.code() {
                0 => EventResult::Sync,    // SYN_REPORT
                1 => EventResult::Dropped, // SYN_DROPPED
                _ => EventResult::Other,
            }
        }
        _ => EventResult::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a synthetic InputEvent
    fn make_event(event_type: EventType, code: u16, value: i32) -> InputEvent {
        InputEvent::new(event_type.0, code, value)
    }

    #[test]
    fn test_key_press() {
        let event = make_event(EventType::KEY, Key::KEY_A.code(), 1);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Press(PhysicalKey::A));
    }

    #[test]
    fn test_key_release() {
        let event = make_event(EventType::KEY, Key::KEY_A.code(), 0);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Release(PhysicalKey::A));
    }

    #[test]
    fn test_key_repeat() {
        let event = make_event(EventType::KEY, Key::KEY_A.code(), 2);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Repeat);
    }

    #[test]
    fn test_unknown_key() {
        // BTN_LEFT is a mouse button, not mapped
        let event = make_event(EventType::KEY, Key::BTN_LEFT.code(), 1);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Unknown);
    }

    #[test]
    fn test_sync_report() {
        let event = make_event(EventType::SYNCHRONIZATION, 0, 0);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Sync);
    }

    #[test]
    fn test_sync_dropped() {
        let event = make_event(EventType::SYNCHRONIZATION, 1, 0);
        let result = process_event(&event);
        assert_eq!(result, EventResult::Dropped);
    }

    #[test]
    fn test_multiple_keys() {
        let keys = [
            (Key::KEY_SPACE, PhysicalKey::Space),
            (Key::KEY_ENTER, PhysicalKey::Enter),
            (Key::KEY_ESC, PhysicalKey::Escape),
            (Key::KEY_LEFTSHIFT, PhysicalKey::LeftShift),
        ];

        for (linux_key, physical_key) in &keys {
            let event = make_event(EventType::KEY, linux_key.code(), 1);
            let result = process_event(&event);
            assert_eq!(result, EventResult::Press(*physical_key));
        }
    }

    #[test]
    fn an_unmapped_press_is_unknown_never_a_press_of_some_other_key() {
        // F13 exists on real keyboards and is deliberately not modelled by
        // `PhysicalKey`. The event must be dropped as Unknown: turning it
        // into `Press(A)` (or any other key) would play the wrong sound and
        // hide a coverage gap.
        for code in [
            Key::KEY_F13.code(),
            Key::KEY_F24.code(),
            Key::KEY_MUTE.code(),
        ] {
            let press = make_event(EventType::KEY, code, 1);
            assert_eq!(process_event(&press), EventResult::Unknown);
            assert_ne!(process_event(&press), EventResult::Press(PhysicalKey::A));
            // A release of the same keycode is Unknown too, never Release(A).
            let release = make_event(EventType::KEY, code, 0);
            assert_eq!(process_event(&release), EventResult::Unknown);
        }
    }

    #[test]
    fn repeats_are_not_presses_and_releases_are_not_presses() {
        let press = make_event(EventType::KEY, Key::KEY_Q.code(), 1);
        let repeat = make_event(EventType::KEY, Key::KEY_Q.code(), 2);
        let release = make_event(EventType::KEY, Key::KEY_Q.code(), 0);

        assert_eq!(process_event(&press), EventResult::Press(PhysicalKey::Q));
        assert_eq!(process_event(&repeat), EventResult::Repeat);
        assert_eq!(
            process_event(&release),
            EventResult::Release(PhysicalKey::Q)
        );
        // Exactly one of the three is a press: holding a key must not
        // retrigger, and letting go must not trigger at all.
        assert!(
            [
                matches!(process_event(&press), EventResult::Press(_)),
                matches!(process_event(&repeat), EventResult::Press(_)),
                matches!(process_event(&release), EventResult::Press(_)),
            ]
            .iter()
            .filter(|&&is_press| is_press)
            .count()
                == 1
        );
    }

    #[test]
    fn test_ignore_repeats() {
        // Holding a key should only generate one press event
        let events = [
            make_event(EventType::KEY, Key::KEY_A.code(), 1), // press
            make_event(EventType::KEY, Key::KEY_A.code(), 2), // repeat
            make_event(EventType::KEY, Key::KEY_A.code(), 2), // repeat
            make_event(EventType::KEY, Key::KEY_A.code(), 2), // repeat
            make_event(EventType::KEY, Key::KEY_A.code(), 0), // release
        ];

        let results: Vec<EventResult> = events.iter().map(process_event).collect();

        assert_eq!(results[0], EventResult::Press(PhysicalKey::A));
        assert_eq!(results[1], EventResult::Repeat);
        assert_eq!(results[2], EventResult::Repeat);
        assert_eq!(results[3], EventResult::Repeat);
        assert_eq!(results[4], EventResult::Release(PhysicalKey::A));
    }
}
