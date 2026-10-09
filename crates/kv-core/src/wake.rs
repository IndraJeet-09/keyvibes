//! Control-plane wake-up hook for idle power management.
//!
//! When KeyVibes has been idle for a while the output stream is paused so the
//! session manager can stop scheduling us - that is the Linux/PipeWire way to
//! stop burning CPU while no key is being pressed. Pausing is only safe if
//! the *next* key can undo it immediately, which is what this trait provides.
//!
//! The hook is called from an input path (an evdev reader thread or the
//! simulated source). It is **never** called from the real-time callback:
//! waking a stream is a lifecycle operation and belongs on a control thread.

/// Something that can bring a paused audio stream back to life.
///
/// Implementations must be cheap when the stream is already running: the hot
/// path is a single relaxed atomic load, with the actual control-plane
/// message sent only on the idle → active edge.
pub trait StreamWake: Send + Sync {
    /// Called just before a command is queued for the audio callback.
    ///
    /// Must not block for a meaningful amount of time, must not allocate on
    /// the real-time path (it is never called from one), and must tolerate
    /// being called when nothing is listening.
    fn wake(&self);
}

/// A wake hook that does nothing.
///
/// Used when no audio engine is running yet: input must still work, and
/// there is simply no stream to resume.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoWake;

impl StreamWake for NoWake {
    fn wake(&self) {}
}
