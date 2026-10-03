//! Lock-free data structures for real-time audio.

pub mod spsc;

pub use spsc::SpscRing;
