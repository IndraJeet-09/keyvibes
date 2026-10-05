//! Criterion benchmarks for the Phase 5 processing pipeline.
//!
//! Everything here runs offline in the builder (never at runtime), so the
//! numbers describe build-time cost per clip. Typical keyboard clips are
//! 20–300 ms, which is the `typical` size class; `long` (1 s) approximates
//! a worst-case sustained sample.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kv_pack::dither::DitherSeed;
use kv_pack::processing::{encode_i16, AudioProcessor};
use kv_pack::wav::MonoAudio;

fn sine(amp: f32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin())
        .collect()
}

/// A clip shaped like a real key press: attack transient, sustained tone,
/// decaying tail, and silent padding on both sides.
fn keypress(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            let env = (-t * 25.0).exp();
            let attack = if i < 96 { 0.3 } else { 0.0 };
            let signal = (2.0 * std::f32::consts::PI * 1000.0 * t).sin();
            0.7 * signal * (env + attack)
        })
        .collect()
}

fn mono(samples: Vec<f32>) -> MonoAudio {
    MonoAudio {
        samples,
        sample_rate: 48_000,
        channels: 1,
    }
}

fn bench_process(c: &mut Criterion) {
    let processor = AudioProcessor::with_defaults();

    let mut group = c.benchmark_group("process");
    group.bench_function("typical_keypress_10ms", |b| {
        let samples = keypress(480);
        b.iter(|| processor.process(black_box(mono(samples.clone()))).unwrap())
    });
    group.bench_function("typical_keypress_100ms", |b| {
        let samples = keypress(4_800);
        b.iter(|| processor.process(black_box(mono(samples.clone()))).unwrap())
    });
    group.bench_function("long_1s", |b| {
        let samples = keypress(48_000);
        b.iter(|| processor.process(black_box(mono(samples.clone()))).unwrap())
    });
    group.finish();
}

fn bench_encode(c: &mut Criterion) {
    let samples = keypress(4_800);
    let seed = Some(DitherSeed::from_u64(42));

    c.bench_function("encode_i16_dither_100ms", |b| {
        b.iter(|| encode_i16(black_box(&samples), 48_000, seed).unwrap())
    });
    c.bench_function("encode_i16_no_dither_100ms", |b| {
        b.iter(|| encode_i16(black_box(&samples), 48_000, None).unwrap())
    });
}

fn bench_analysis(c: &mut Criterion) {
    let samples = sine(0.5, 48_000);

    c.bench_function("analysis_measure_1s", |b| {
        b.iter(|| kv_pack::analysis::AudioAnalysis::measure(black_box(&samples), 48_000, 1))
    });
}

criterion_group!(benches, bench_process, bench_encode, bench_analysis);
criterion_main!(benches);
