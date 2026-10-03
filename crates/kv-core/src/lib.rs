//! Core types and abstractions for KeyVibes.
//!
//! This crate defines the fundamental types used throughout the KeyVibes architecture:
//! - Physical key abstraction
//! - Play commands
//! - Variation parameters
//! - Geometry
//! - Settings

pub mod physical_key;
pub mod play;
pub mod variation;
pub mod geometry;
pub mod settings;

pub use physical_key::PhysicalKey;
pub use play::{PlayCommand, KeyEvent, KeyEventKind};
pub use variation::{VariationParams, PitchVariation, GainVariation};
pub use geometry::KeyGeometry;
pub use settings::Settings;
