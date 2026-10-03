//! Linux evdev input backend for KeyVibes.
//!
//! This module provides keyboard input capture using the Linux kernel's evdev
//! interface. It discovers keyboards, handles hotplug, and converts Linux
//! keycodes into portable PhysicalKey representations.

pub mod backend;
pub mod device;
pub mod discovery;
pub mod events;
pub mod hotplug;
pub mod mapping;
pub mod error;
pub mod diagnostics;

pub use backend::LinuxInputBackend;
pub use error::InputError;
pub use diagnostics::InputStats;
