//! Real-time audio mixer.
//!
//! Provides polyphonic mixing with voice management, interpolation,
//! and limiting for the KeyVibes audio engine.

pub mod interpolation;
pub mod limiter;
pub mod mixer;
pub mod voice;

pub use interpolation::{cubic_interpolate, cubic_interpolate_i16};
pub use limiter::SoftLimiter;
pub use mixer::{Mixer, MAX_VOICES};
pub use voice::Voice;
