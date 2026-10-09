//! End-to-end smoke tests against a live PipeWire server.
//!
//! These tests are ignored by default so CI machines without a sound server
//! stay green; run them locally with:
//!
//! ```bash
//! cargo test -p kv-audio-pipewire --test smoke -- --ignored --nocapture
//! ```

use kv_audio_pipewire::{AudioEngine, ReconnectPolicy};
use kv_core::PlayCommand;
use kv_ring::SpscRing;
use std::sync::Arc;
use std::time::Duration;

/// One second of 440 Hz at 48 kHz, quiet enough to be harmless on real speakers.
fn tone() -> Vec<i16> {
    (0..48_000)
        .map(|i| {
            let phase = (i as f32) * std::f32::consts::TAU * 440.0 / 48_000.0;
            (phase.sin() * 4000.0) as i16
        })
        .collect()
}

#[test]
#[ignore = "requires a running PipeWire server"]
fn smoke_engine_connects_and_renders() {
    let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(64));
    let engine = AudioEngine::start(
        48_000,
        queue.clone(),
        ReconnectPolicy::default(),
        Default::default(),
    )
    .expect("PipeWire must be reachable");

    let active = engine.wait_until_active(Duration::from_secs(5));
    let before = engine.stats();
    println!(
        "state after connect: {:?} active={active}",
        before.stream_state
    );

    let tone = tone();
    let cmd =
        unsafe { PlayCommand::new(tone.as_ptr(), 48_000, 48_000, 1u64 << 32, 0.5, 0.5, false) };
    queue.push(cmd).expect("queue has room");

    // Give the graph several quanta to schedule us.
    std::thread::sleep(Duration::from_millis(700));

    let stats = engine.stats();
    println!("{stats:#?}");

    engine.stop_and_join().expect("clean shutdown");

    assert!(
        stats.frames_rendered > 0,
        "PipeWire never invoked the process callback"
    );
    assert!(
        stats.peak > 0.0,
        "the mixer rendered only silence after a non-silent command"
    );
    assert_eq!(stats.deadline_misses, 0, "callback overran its quantum");
}
