//! Keyboard geometry and spatialization.
//!
//! Maps physical keys to 2D positions for stereo spatialization.

use crate::physical_key::PhysicalKey;
use std::f32::consts::PI;

/// 2D position of a key on a keyboard.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct KeyGeometry {
    /// Horizontal position (units).
    pub x: f32,
    /// Vertical position (units).
    pub y: f32,
}

impl KeyGeometry {
    /// Creates a new key geometry.
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Returns the default ANSI keyboard geometry for a given key.
    ///
    /// Positions are in keyboard units where each key is roughly 1 unit wide.
    /// Origin is at the top-left (Escape key).
    pub fn default_position(key: PhysicalKey) -> Self {
        // ANSI keyboard layout positions (approximate)
        // Row 0: Function keys (y = 0)
        // Row 1: Number row (y = 1.5)
        // Row 2: QWERTY row (y = 2.5)
        // Row 3: ASDF row (y = 3.5)
        // Row 4: ZXCV row (y = 4.5)
        // Row 5: Space bar row (y = 5.5)

        match key {
            // Function row
            PhysicalKey::Escape => Self::new(0.0, 0.0),
            PhysicalKey::F1 => Self::new(2.0, 0.0),
            PhysicalKey::F2 => Self::new(3.0, 0.0),
            PhysicalKey::F3 => Self::new(4.0, 0.0),
            PhysicalKey::F4 => Self::new(5.0, 0.0),
            PhysicalKey::F5 => Self::new(6.5, 0.0),
            PhysicalKey::F6 => Self::new(7.5, 0.0),
            PhysicalKey::F7 => Self::new(8.5, 0.0),
            PhysicalKey::F8 => Self::new(9.5, 0.0),
            PhysicalKey::F9 => Self::new(11.0, 0.0),
            PhysicalKey::F10 => Self::new(12.0, 0.0),
            PhysicalKey::F11 => Self::new(13.0, 0.0),
            PhysicalKey::F12 => Self::new(14.0, 0.0),

            // Number row
            PhysicalKey::Grave => Self::new(0.0, 1.5),
            PhysicalKey::Digit1 => Self::new(1.0, 1.5),
            PhysicalKey::Digit2 => Self::new(2.0, 1.5),
            PhysicalKey::Digit3 => Self::new(3.0, 1.5),
            PhysicalKey::Digit4 => Self::new(4.0, 1.5),
            PhysicalKey::Digit5 => Self::new(5.0, 1.5),
            PhysicalKey::Digit6 => Self::new(6.0, 1.5),
            PhysicalKey::Digit7 => Self::new(7.0, 1.5),
            PhysicalKey::Digit8 => Self::new(8.0, 1.5),
            PhysicalKey::Digit9 => Self::new(9.0, 1.5),
            PhysicalKey::Digit0 => Self::new(10.0, 1.5),
            PhysicalKey::Minus => Self::new(11.0, 1.5),
            PhysicalKey::Equal => Self::new(12.0, 1.5),
            PhysicalKey::Backspace => Self::new(13.5, 1.5),

            // QWERTY row
            PhysicalKey::Tab => Self::new(0.5, 2.5),
            PhysicalKey::Q => Self::new(1.5, 2.5),
            PhysicalKey::W => Self::new(2.5, 2.5),
            PhysicalKey::E => Self::new(3.5, 2.5),
            PhysicalKey::R => Self::new(4.5, 2.5),
            PhysicalKey::T => Self::new(5.5, 2.5),
            PhysicalKey::Y => Self::new(6.5, 2.5),
            PhysicalKey::U => Self::new(7.5, 2.5),
            PhysicalKey::I => Self::new(8.5, 2.5),
            PhysicalKey::O => Self::new(9.5, 2.5),
            PhysicalKey::P => Self::new(10.5, 2.5),
            PhysicalKey::LeftBracket => Self::new(11.5, 2.5),
            PhysicalKey::RightBracket => Self::new(12.5, 2.5),
            PhysicalKey::Backslash => Self::new(13.5, 2.5),

            // ASDF row
            PhysicalKey::CapsLock => Self::new(0.75, 3.5),
            PhysicalKey::A => Self::new(1.75, 3.5),
            PhysicalKey::S => Self::new(2.75, 3.5),
            PhysicalKey::D => Self::new(3.75, 3.5),
            PhysicalKey::F => Self::new(4.75, 3.5),
            PhysicalKey::G => Self::new(5.75, 3.5),
            PhysicalKey::H => Self::new(6.75, 3.5),
            PhysicalKey::J => Self::new(7.75, 3.5),
            PhysicalKey::K => Self::new(8.75, 3.5),
            PhysicalKey::L => Self::new(9.75, 3.5),
            PhysicalKey::Semicolon => Self::new(10.75, 3.5),
            PhysicalKey::Apostrophe => Self::new(11.75, 3.5),
            PhysicalKey::Enter => Self::new(13.0, 3.5),

            // ZXCV row
            PhysicalKey::LeftShift => Self::new(1.0, 4.5),
            PhysicalKey::Z => Self::new(2.25, 4.5),
            PhysicalKey::X => Self::new(3.25, 4.5),
            PhysicalKey::C => Self::new(4.25, 4.5),
            PhysicalKey::V => Self::new(5.25, 4.5),
            PhysicalKey::B => Self::new(6.25, 4.5),
            PhysicalKey::N => Self::new(7.25, 4.5),
            PhysicalKey::M => Self::new(8.25, 4.5),
            PhysicalKey::Comma => Self::new(9.25, 4.5),
            PhysicalKey::Period => Self::new(10.25, 4.5),
            PhysicalKey::Slash => Self::new(11.25, 4.5),
            PhysicalKey::RightShift => Self::new(13.0, 4.5),

            // Bottom row
            PhysicalKey::LeftCtrl => Self::new(0.5, 5.5),
            PhysicalKey::LeftSuper => Self::new(1.5, 5.5),
            PhysicalKey::LeftAlt => Self::new(2.5, 5.5),
            PhysicalKey::Space => Self::new(7.25, 5.5), // Center of space bar
            PhysicalKey::RightAlt => Self::new(10.5, 5.5),
            PhysicalKey::RightSuper => Self::new(11.5, 5.5),
            PhysicalKey::Menu => Self::new(12.5, 5.5),
            PhysicalKey::RightCtrl => Self::new(13.5, 5.5),

            // Navigation cluster (to the right of main keyboard)
            PhysicalKey::Insert => Self::new(15.5, 1.5),
            PhysicalKey::Home => Self::new(16.5, 1.5),
            PhysicalKey::PageUp => Self::new(17.5, 1.5),
            PhysicalKey::Delete => Self::new(15.5, 2.5),
            PhysicalKey::End => Self::new(16.5, 2.5),
            PhysicalKey::PageDown => Self::new(17.5, 2.5),

            // Arrow keys
            PhysicalKey::ArrowUp => Self::new(16.5, 4.5),
            PhysicalKey::ArrowDown => Self::new(16.5, 5.5),
            PhysicalKey::ArrowLeft => Self::new(15.5, 5.5),
            PhysicalKey::ArrowRight => Self::new(17.5, 5.5),

            // Numpad (far right)
            PhysicalKey::NumLock => Self::new(19.0, 1.5),
            PhysicalKey::NumpadDivide => Self::new(20.0, 1.5),
            PhysicalKey::NumpadMultiply => Self::new(21.0, 1.5),
            PhysicalKey::NumpadSubtract => Self::new(22.0, 1.5),

            PhysicalKey::Numpad7 => Self::new(19.0, 2.5),
            PhysicalKey::Numpad8 => Self::new(20.0, 2.5),
            PhysicalKey::Numpad9 => Self::new(21.0, 2.5),
            PhysicalKey::NumpadAdd => Self::new(22.0, 2.5),

            PhysicalKey::Numpad4 => Self::new(19.0, 3.5),
            PhysicalKey::Numpad5 => Self::new(20.0, 3.5),
            PhysicalKey::Numpad6 => Self::new(21.0, 3.5),

            PhysicalKey::Numpad1 => Self::new(19.0, 4.5),
            PhysicalKey::Numpad2 => Self::new(20.0, 4.5),
            PhysicalKey::Numpad3 => Self::new(21.0, 4.5),
            PhysicalKey::NumpadEnter => Self::new(22.0, 4.5),

            PhysicalKey::Numpad0 => Self::new(19.5, 5.5),
            PhysicalKey::NumpadDecimal => Self::new(21.0, 5.5),

            // Special keys
            PhysicalKey::PrintScreen => Self::new(15.5, 0.0),
            PhysicalKey::ScrollLock => Self::new(16.5, 0.0),
            PhysicalKey::Pause => Self::new(17.5, 0.0),
        }
    }

    /// Calculates stereo pan from horizontal position.
    ///
    /// Returns a pan value in the range [-1.0, 1.0] where:
    /// - -1.0 = hard left
    /// - 0.0 = center
    /// - 1.0 = hard right
    ///
    /// The pan is scaled by 0.4 to keep sounds reasonably centered.
    #[inline]
    pub fn calculate_pan(&self) -> f32 {
        // Center the keyboard horizontally around x = 7.25 (space bar)
        // Normalize by width of main keyboard (approximately 7.75 units half-width)
        let normalized = (self.x - 7.25) / 7.75;

        // Clamp and scale
        normalized.clamp(-1.0, 1.0) * 0.4
    }

    /// Converts pan to stereo gains using equal-power panning.
    ///
    /// Returns (left_gain, right_gain).
    #[inline]
    pub fn pan_to_gains(pan: f32) -> (f32, f32) {
        // Pan in [-1, 1] maps to angle in [0, PI/2]
        let angle = (pan + 1.0) * PI / 4.0;

        // Equal-power panning
        const SQRT_2: f32 = std::f32::consts::SQRT_2;
        let left = angle.cos() * SQRT_2;
        let right = angle.sin() * SQRT_2;

        (left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_center_keys_near_center() {
        let space = KeyGeometry::default_position(PhysicalKey::Space);
        let pan = space.calculate_pan();
        assert!(
            pan.abs() < 0.1,
            "Space bar should be near center, got pan={}",
            pan
        );
    }

    #[test]
    fn test_left_keys_pan_left() {
        let q = KeyGeometry::default_position(PhysicalKey::Q);
        let pan = q.calculate_pan();
        assert!(pan < 0.0, "Q should pan left, got pan={}", pan);
    }

    #[test]
    fn test_right_keys_pan_right() {
        let p = KeyGeometry::default_position(PhysicalKey::P);
        let pan = p.calculate_pan();
        assert!(pan > 0.0, "P should pan right, got pan={}", pan);
    }

    #[test]
    fn test_equal_power_panning() {
        // Center: equal power to both channels
        let (left, right) = KeyGeometry::pan_to_gains(0.0);
        assert!((left - right).abs() < 0.01);
        assert!((left.powi(2) + right.powi(2) - 2.0).abs() < 0.01);

        // Hard left: more left gain
        let (left, right) = KeyGeometry::pan_to_gains(-1.0);
        assert!(left > right);

        // Hard right: more right gain
        let (left, right) = KeyGeometry::pan_to_gains(1.0);
        assert!(right > left);
    }

    #[test]
    fn test_modifiers_distinct_positions() {
        let left_shift = KeyGeometry::default_position(PhysicalKey::LeftShift);
        let right_shift = KeyGeometry::default_position(PhysicalKey::RightShift);
        assert_ne!(left_shift.x, right_shift.x);
    }
}
