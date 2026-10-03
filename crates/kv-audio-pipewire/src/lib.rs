//! PipeWire audio backend.

pub mod lifecycle;
pub mod realtime;
pub mod stream;

pub use stream::PipeWireStream;
