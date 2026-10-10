//! Counts heap allocations made by this process.
//!
//! The engine's contract is that nothing allocates on the real-time path: a
//! `Vec` growing inside the PipeWire callback would be a latency spike the
//! scheduler cannot hide. That contract is enforced by audits and tests
//! inside the audio crates, but those run inside `cargo test` - they never
//! see the binary a user actually runs.
//!
//! Installing [`Counting`] as the binary's global allocator means
//! [`snapshot`] reports exactly what this process did, on every thread, with
//! no cooperation required from the code under measurement. The cost is two
//! relaxed atomic increments per allocation, which is far below the cost of
//! the allocation itself.
//!
//! Counting is deliberately *not* attributed per thread: a window in which
//! this thread does nothing but a real-time callback still runs is a window
//! in which the only unexpected allocator is the callback. Commands that need
//! that window build everything they touch before taking a snapshot.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The allocator the `keyvibes` binary runs with.
pub struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);

/// A point-in-time reading of the allocation counters.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// `alloc` / `alloc_zeroed` / `realloc` calls since the process started.
    pub allocations: usize,
    /// `dealloc` calls since the process started.
    pub frees: usize,
}

impl Snapshot {
    /// Reads the counters now.
    pub fn take() -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            frees: FREES.load(Ordering::Relaxed),
        }
    }

    /// Allocations made between `self` (taken first) and `later`.
    pub fn allocations_since(&self, later: Snapshot) -> usize {
        later.allocations.saturating_sub(self.allocations)
    }

    /// Frees made between `self` and `later`.
    pub fn frees_since(&self, later: Snapshot) -> usize {
        later.frees.saturating_sub(self.frees)
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREES.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A realloc is a fresh allocation as far as a latency budget is
        // concerned: it may move and it may touch the whole block.
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heap_allocation_is_counted() {
        let before = Snapshot::take();
        let held = vec![0u8; 4096];
        let after = Snapshot::take();
        assert!(before.allocations_since(after) >= 1);
        drop(held);
    }

    #[test]
    fn a_free_is_counted_too() {
        let held = vec![0u8; 4096];
        let before = Snapshot::take();
        drop(held);
        let after = Snapshot::take();
        assert!(before.frees_since(after) >= 1);
    }
}
