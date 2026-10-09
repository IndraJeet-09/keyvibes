//! PipeWire audio backend.
//!
//! Three layers:
//!
//! * [`stream`] — the PipeWire connection and the real-time process callback
//! * [`engine`] — a control thread that supervises the connection and
//!   reconnects with bounded backoff
//! * [`lifecycle`] — reconnect policy and observable lifecycle state
//!
//! Nothing in this crate performs blocking work from the real-time callback.

pub mod engine;
pub mod idle;
pub mod lifecycle;
pub mod realtime;
pub mod stream;
pub mod xrun;

pub use engine::{AudioControl, AudioEngine, AudioOptions};
pub use idle::{
    IdleConfig, IdlePhase, IdleState, IdleStats, DEFAULT_IDLE_AFTER, DEFAULT_IDLE_POLL,
};
pub use lifecycle::{AudioPhase, LifecycleState, ReconnectPolicy, ReconnectStateMachine};
pub use stream::{AudioError, Connection, PipeWireStream, RtStats, StreamStateCode, StreamStats};
pub use xrun::{NodeXruns, XrunBaseline, NODE_NAME};
