//! Local control protocol for the KeyVibes engine.
//!
//! The engine (`keyvibes run`) listens on a per-user unix socket; any
//! same-uid client - the desktop UI, `keyvibes ctl`, a test - can query
//! state and change settings over newline-delimited JSON. There is no
//! network listener and no authentication beyond the socket's `0600`
//! permissions and the kernel's same-uid guarantee.
//!
//! Nothing here touches the real-time path: the server thread applies
//! lock-free updates through shared atomics and hands long-running work
//! (pack switching) to the engine's main loop.

pub mod protocol;
pub mod socket;

pub use protocol::{
    AudioInfo, EngineInfo, Envelope, Event, InputInfo, LiveSettings, PackInfo, PreviewResult,
    Request, Response, ServerMessage, State,
};
pub use socket::{default_socket_path, read_request, write_message, Client, Server, SOCKET_ENV};

/// The current process's effective uid.
///
/// Used for the `SO_PEERCRED` check and the `/tmp/keyvibes-$UID` fallback.
pub(crate) fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
