//! Simple benchmarks for the mixer.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kv_core::PlayCommand;
use kv_mixer::Mixer;
use kv_ring::SpscRing;

/// Generate a simple click for testing.
fn generate_click() -> Vec<i16> {
    vec![
        0, 0, // guard samples
        16000, 8000, -8000, -4000, 2000, -1000, 500, -250, 0, 0, 0, 0, // guard samples
    ]
}

fn bench_mixer_render(c: &mut Criterion) {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    // Pre-trigger some voices
    for _ in 0..8 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.5,
                0.5,
                false,
            )
        };
        mixer.trigger(cmd);
    }

    c.bench_function("render_8_voices_128_frames", |b| {
        b.iter(|| {
            let mut output = [0.0f32; 256];
            unsafe {
                mixer.render_block(black_box(&mut output));
            }
            black_box(output);
        })
    });

    // Test with full polyphony
    let mut mixer_full = Mixer::new(48000);
    for _ in 0..32 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.25,
                0.25,
                false,
            )
        };
        mixer_full.trigger(cmd);
    }

    c.bench_function("render_32_voices_128_frames", |b| {
        b.iter(|| {
            let mut output = [0.0f32; 256];
            unsafe {
                mixer_full.render_block(black_box(&mut output));
            }
            black_box(output);
        })
    });
}

fn bench_queue_operations(c: &mut Criterion) {
    let samples = generate_click();
    let queue: SpscRing<PlayCommand> = SpscRing::with_capacity(256);

    // Fill queue halfway
    for _ in 0..128 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.5,
                0.5,
                false,
            )
        };
        queue.push(cmd).expect("Should have space");
    }

    c.bench_function("queue_push_pop_128", |b| {
        b.iter(|| {
            let cmd = unsafe {
                PlayCommand::new(
                    samples[2..].as_ptr(),
                    (samples.len() - 5) as u32,
                    48000,
                    1u64 << 32,
                    0.5,
                    0.5,
                    false,
                )
            };
            let _ = black_box(queue.push(cmd));
            let _ = black_box(queue.pop());
        })
    });
}

criterion_group!(benches, bench_mixer_render, bench_queue_operations);
criterion_main!(benches);
