//! Core types and abstractions for KeyVibes.
//!
//! This crate defines the fundamental types used throughout the KeyVibes architecture:
//! - Physical key abstraction
//! - Play commands
//! - Variation parameters
//! - Geometry
//! - Settings

pub mod geometry;
pub mod metrics;
pub mod physical_key;
pub mod play;
pub mod settings;
pub mod time;
pub mod variation;
pub mod wake;

pub use geometry::KeyGeometry;
pub use metrics::AtomicHistogram;
pub use physical_key::PhysicalKey;
pub use play::{KeyEvent, KeyEventKind, PlayCommand, SoundSource};
pub use settings::Settings;
pub use time::monotonic_ns;
pub use variation::{GainVariation, PitchVariation, VariantState, VariationParams};
pub use wake::{NoWake, StreamWake};
