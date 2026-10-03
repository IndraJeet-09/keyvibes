//! PipeWire audio backend.

pub mod stream;
pub mod realtime;
pub mod lifecycle;

pub use stream::PipeWireStream;
