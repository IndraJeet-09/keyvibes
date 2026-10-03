//! Real-time safety utilities.
//!
//! These utilities ensure the PipeWire process callback maintains real-time
//! safety: zero allocations, no mutex locks (where avoidable), and no I/O.

/// A lock-free command processor for the real-time thread.
///
/// This is a thin wrapper over the SPSC ring that provides safe access
/// from the real-time thread without blocking.
pub struct RtCommandProcessor<T> {
    queue: std::sync::Arc<kv_ring::SpscRing<T>>,
}

impl<T: Copy> RtCommandProcessor<T> {
    /// Creates a new real-time command processor.
    pub fn new(queue: std::sync::Arc<kv_ring::SpscRing<T>>) -> Self {
        Self { queue }
    }

    /// Drains all available commands from the queue.
    /// Returns the number of commands processed.
    pub fn drain<F>(&self, mut f: F) -> usize
    where
        F: FnMut(T),
    {
        let mut count = 0;
        while let Some(cmd) = self.queue.pop() {
            f(cmd);
            count += 1;
        }
        count
    }
}
