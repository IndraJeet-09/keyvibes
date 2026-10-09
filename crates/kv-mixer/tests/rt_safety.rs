//! Phase 8: real-time safety proof by measurement.
//!
//! `keyvibes stress` measures wall-clock latency under load; this test proves
//! the stronger structural claim that the code the PipeWire callback actually
//! runs performs **zero heap allocations**.
//!
//! The proof is a counting `#[global_allocator]` wrapped around the real
//! `Mixer::trigger` / `Mixer::render_block` hot path - the same two calls the
//! callback makes after dequeuing a command. A single test lives in this file
//! on purpose: the test harness runs tests in parallel threads, and any other
//! test in this binary would pollute the counter.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: CountingAllocator = CountingAllocator;

fn allocations() -> usize {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// A clip long enough to span several render blocks, with guard samples at
/// both ends like a real packed clip.
fn clip() -> Vec<i16> {
    (0..4096u32)
        .map(|i| ((i.wrapping_mul(668265263) >> 16) as i16) / 2)
        .collect()
}

#[test]
fn trigger_and_render_block_allocate_nothing() {
    let samples = clip();
    let len = samples.len() as u32;
    let ptr = samples.as_ptr();

    let mut mixer = kv_mixer::Mixer::new(48000);
    let mut output = vec![0.0f32; 1024 * 2];
    let queue = kv_ring::SpscRing::<kv_core::PlayCommand>::with_capacity(1024);

    // Warm-up: exercise every branch once so lazily initialised state (voice
    // bookkeeping, gain ramp, first-time fills) cannot hide a later
    // allocation behind the "before" sample.
    for _ in 0..8 {
        let cmd =
            unsafe { kv_core::PlayCommand::new(ptr, len, 48000, 1u64 << 32, 1.0, 1.0, false) };
        assert!(queue.push(cmd).is_ok());
        if let Some(cmd) = queue.pop() {
            mixer.trigger(cmd);
            unsafe { mixer.render_block(&mut output) };
        }
    }

    let before = allocations();

    for _ in 0..10_000 {
        let cmd =
            unsafe { kv_core::PlayCommand::new(ptr, len, 48000, 1u64 << 32, 0.7, 1.3, false) };
        queue.push(cmd).expect("ring sized for one command");
        let cmd = queue.pop().expect("ring just filled");
        mixer.trigger(cmd);
        unsafe { mixer.render_block(&mut output) };
    }

    let after = allocations();

    assert_eq!(
        before,
        after,
        "the real-time path allocated {} time(s): trigger() and \
         render_block() must never touch the heap",
        after.saturating_sub(before)
    );
    assert!(
        mixer.active_voice_count() > 0,
        "the mixer should have been busy"
    );

    // Control measurement: prove the counter actually counts, so a false pass
    // (counter stuck at zero) is impossible.
    let control_before = allocations();
    let held = format!("control {control_before}");
    let control_after = allocations();
    assert!(
        control_after > control_before,
        "counting allocator did not observe an obvious allocation"
    );
    drop(held);
}
