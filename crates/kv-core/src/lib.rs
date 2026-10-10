//! Core types and abstractions for KeyVibes.
//!
//! This crate defines the fundamental types used throughout the KeyVibes architecture:
//! - Physical key abstraction
//! - Play commands
//! - Variation parameters
//! - Geometry
//! - Settings
//!
//! # Platform neutrality
//!
//! `kv-core` is the one crate every other crate shares, so it must not know
//! what it is running on. It carries the vocabulary of the engine -
//! `PhysicalKey`, `PlayCommand`, `Settings` - and deliberately none of the
//! machinery that realises them:
//!
//! ```text
//! kv-core                       shared types, no platform
//!    ├── kv-input-linux         evdev          (future: kv-input-windows)
//!    └── kv-audio-pipewire      PipeWire       (future: kv-audio-wasapi)
//! ```
//!
//! An input backend translates the host's key codes into [`PhysicalKey`]; an
//! audio backend renders [`PlayCommand`]s the host understands. Nothing above
//! that line names either. This is enforced on every `cargo test -p kv-core`
//! by `tests/platform_audit.rs`, which rejects a dependency outside a short
//! portable allow list and any use of a platform API in the source.

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
