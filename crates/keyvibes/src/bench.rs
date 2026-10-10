//! Phase 19 acceptance: `keyvibes benchmark`.
//!
//! A regression in this project is not "the code looks slower", it is "the
//! mixer can no longer finish a block before the next one is due" or "the
//! real-time path started allocating". Every number printed here therefore
//! has a budget attached, and the budget is expressed in the units the audio
//! path cares about - the wall-clock length of one block.
//!
//! Measurement notes:
//!
//! * Each loop is built completely before the counters are snapshotted, so
//!   the allocation checks see the loop and nothing else.
//! * Operations are timed in batches and reported per operation: a single
//!   `Instant::now()` costs a comparable amount to the operations themselves.
//! * Percentiles come from the whole batch set, not the mean - the mean is
//!   exactly the statistic a real-time system cannot use.

use crate::accept::{Report, Status};
use crate::alloc_probe::Snapshot;
use crate::config::Config;
use crate::pack_locate;
use anyhow::Result;
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_mixer::{Mixer, MixerSettings};
use kv_pack::KvPack;
use kv_runtime::Runtime;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Frames rendered per mixer block - the unit every budget is relative to.
const BLOCK: usize = 1024;
/// Output rate the engine renders at.
const RATE: u32 = 48_000;
/// Samples per timing batch for the small operations.
const BATCH: usize = 64;
/// Batches collected for each small operation.
const BATCHES: usize = 1_024;
/// Mixer blocks timed per run.
const BLOCKS: usize = 400;
/// Pack open/close cycles in the memory-growth check.
const CYCLES: usize = 200;
/// Voices kept active while the mixer is timed - `MAX_VOICES`.
const VOICES: usize = 32;

/// One operation's latency distribution, in nanoseconds per operation.
struct Timing {
    p50: u64,
    p99: u64,
    max: u64,
}

impl Timing {
    fn from_sorted(samples: &mut [u64]) -> Self {
        samples.sort_unstable();
        let pick = |percentile: usize| {
            let index = (samples.len().saturating_sub(1)) * percentile / 100;
            samples[index]
        };
        Self {
            p50: pick(50),
            p99: pick(99),
            max: samples.last().copied().unwrap_or(0),
        }
    }

    fn describe(&self) -> String {
        format!(
            "p50 {:.1} ns, p99 {:.1} ns, max {:.1} ns",
            self.p50 as f64, self.p99 as f64, self.max as f64
        )
    }
}

/// Runs the acceptance test.
pub fn run(selector: Option<&str>) -> Result<()> {
    let mut report = Report::new("benchmark");

    let Some(pack_path) = pick_pack(selector) else {
        report.not_run(
            "a sound pack was available to benchmark",
            "no pack installed; build one with `keyvibes pack build`",
        );
        return report.finish();
    };

    let block_ns = BLOCK as f64 / f64::from(RATE) * 1_000_000_000.0;

    // ---------------------------------------------------------------- queue
    let (push, pop) = bench_queue();
    report.add(
        if push.p99 <= 1_000 && pop.p99 <= 1_000 {
            Status::Pass
        } else {
            Status::Fail
        },
        "single-producer queue stays far below its per-command budget",
        format!(
            "push: {}; pop: {}; budget 1000 ns p99 each",
            push.describe(),
            pop.describe()
        ),
    );

    // ------------------------------------------------- mixer timing and RT
    // One pass supplies both: the latency distribution and the number of
    // heap allocations made while producing it.
    let (render, render_allocations, render_frees) = bench_mixer();
    let budget_ns = block_ns / 10.0;
    report.add(
        if (render.p99 as f64) <= budget_ns {
            Status::Pass
        } else {
            Status::Fail
        },
        "mixer renders 32 voices inside one tenth of the real-time budget",
        format!(
            "{}; budget {:.0} ns (1/10 of a {BLOCK}-frame block at {RATE} Hz = {:.0} ns){}",
            render.describe(),
            budget_ns,
            block_ns,
            profile_note()
        ),
    );

    // --------------------------------------------- pack lookup and allocations
    let pack = KvPack::open(&pack_path)
        .map_err(|error| anyhow::anyhow!("cannot benchmark {}: {error}", pack_path.display()))?;
    let keys: Vec<PhysicalKey> = pack.keys().iter().map(|entry| entry.physical_key).collect();
    let player = kv_runtime::PackPlayer::new(std::sync::Arc::new(pack), RATE);

    let (lookup, lookup_allocations, lookup_frees) = bench_lookup(&player, &keys);

    report.add(
        if lookup.p99 <= 5_000 {
            Status::Pass
        } else {
            Status::Fail
        },
        "sound pack lookup stays inside its per-press budget",
        format!(
            "{}; budget 5000 ns p99 across {} key(s)",
            lookup.describe(),
            keys.len()
        ),
    );
    report.add(
        if lookup_allocations == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "sound pack lookup allocates nothing",
        if lookup_allocations == 0 {
            format!(
                "{} press(es), 0 heap allocation(s), {lookup_frees} free(s)",
                BATCHES * BATCH
            )
        } else {
            format!(
                "{} press(es) made {lookup_allocations} heap allocation(s)",
                BATCHES * BATCH
            )
        },
    );

    report.add(
        if render_allocations == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "the real-time render path allocates nothing",
        if render_allocations == 0 {
            format!(
                "{BLOCKS} block(s) of {VOICES} voices, 0 heap allocation(s), \
                 {render_frees} free(s)"
            )
        } else {
            format!(
                "{BLOCKS} block(s) made {render_allocations} heap allocation(s) \
                 - a real-time violation"
            )
        },
    );

    // ------------------------------------------------------------- key press
    let (press, dropped) = bench_presses(&pack_path);
    report.add(
        if press.p99 <= 20_000 && dropped == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "key press to queued command stays inside its budget",
        format!(
            "{}; budget 20000 ns p99; {} of {} press(es) could not be queued{}",
            press.describe(),
            dropped,
            BATCHES * BATCH,
            profile_note()
        ),
    );

    // ------------------------------------------------------------- memory
    let growth = bench_memory(&pack_path);
    report.add(
        if growth <= 16 * 1024 {
            Status::Pass
        } else {
            Status::Fail
        },
        "repeated pack open and close does not grow the process",
        format!("{CYCLES} cycle(s) grew resident memory by {growth} KiB; budget 16384 KiB"),
    );

    // --------------------------------------------- the live callback itself
    match bench_live_callback(&pack_path) {
        Ok((allocations, callbacks)) => report.add(
            if allocations == 0 {
                Status::Pass
            } else {
                Status::Fail
            },
            "the live audio callback allocates nothing while it renders",
            if allocations == 0 {
                format!("{callbacks} callback(s) in the window, 0 heap allocation(s)")
            } else {
                format!(
                    "{callbacks} callback(s) in the window made \
                     {allocations} heap allocation(s)"
                )
            },
        ),
        Err(reason) => report.not_run(
            "the live audio callback allocates nothing while it renders",
            reason,
        ),
    }

    report.finish()
}

// ------------------------------------------------------------------ queue ---

/// Times a push and a pop of the lock-free ring separately.
fn bench_queue() -> (Timing, Timing) {
    let queue: kv_ring::SpscRing<PlayCommand> = kv_ring::SpscRing::with_capacity(4096);
    let command = dummy_command();

    let mut pushes = Vec::with_capacity(BATCHES);
    let mut pops = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let start = Instant::now();
        for _ in 0..BATCH {
            if queue.push(command).is_err() {
                black_box(());
            }
        }
        pushes.push(start.elapsed().as_nanos() as u64 / BATCH as u64);

        let start = Instant::now();
        for _ in 0..BATCH {
            black_box(queue.pop());
        }
        pops.push(start.elapsed().as_nanos() as u64 / BATCH as u64);
    }
    (
        Timing::from_sorted(&mut pushes),
        Timing::from_sorted(&mut pops),
    )
}

// ----------------------------------------------------------------- mixer ---

/// Renders `BLOCKS` blocks with `VOICES` voices active, one sample per block.
///
/// Returns the latency distribution and the heap allocations made while
/// producing it. Everything the loop touches is built before the first
/// snapshot, so the allocation count is attributable to the render path.
fn bench_mixer() -> (Timing, usize, usize) {
    let sample: &'static [i16] = Box::leak(vec![1_000i16; RATE as usize * 4].into_boxed_slice());
    let mut mixer = Mixer::with_settings(RATE, MixerSettings::default());
    let mut output = vec![0.0f32; BLOCK * 2];
    let mut samples = Vec::with_capacity(BLOCKS);
    fill_voices(&mut mixer, sample);

    let before = Snapshot::take();
    for _ in 0..BLOCKS {
        if mixer.active_voice_count() < VOICES {
            fill_voices(&mut mixer, sample);
        }
        let start = Instant::now();
        // SAFETY: every voice points into `sample`, which is leaked for the
        // lifetime of the process and never resized.
        unsafe { mixer.render_block(&mut output) };
        samples.push(start.elapsed().as_nanos() as u64);
    }
    let after = Snapshot::take();
    black_box(&output);

    (
        Timing::from_sorted(&mut samples),
        before.allocations_since(after),
        before.frees_since(after),
    )
}

/// Starts `VOICES` voices reading from `sample`.
fn fill_voices(mixer: &mut Mixer, sample: &'static [i16]) {
    for index in 0..VOICES {
        mixer.trigger(PlayCommand {
            sample_ptr: sample.as_ptr(),
            sample_len: sample.len() as u32,
            source_rate: RATE,
            // One distinct pitch per voice, so interpolation is not the same
            // instruction path every time.
            pitch_step: (1u64 << 32) + index as u64,
            left_gain: 0.5,
            right_gain: 0.5,
            release: false,
            enqueued_ns: 0,
        });
    }
}

// ---------------------------------------------------------- pack lookup ---

/// `PlayCommand` construction for every key the pack actually contains.
///
/// The sample vector is allocated before the counters are read, so the
/// reported allocation count belongs to the presses alone.
fn bench_lookup(player: &kv_runtime::PackPlayer, keys: &[PhysicalKey]) -> (Timing, usize, usize) {
    if keys.is_empty() {
        return (
            Timing {
                p50: 0,
                p99: 0,
                max: 0,
            },
            0,
            0,
        );
    }
    let mut state = VariantState {
        last_variant: 0,
        rotation: 0,
    };
    let mut samples = Vec::with_capacity(BATCHES);
    let before = Snapshot::take();
    for _ in 0..BATCHES {
        let start = Instant::now();
        for _ in 0..BATCH {
            let key = keys[black_box(state.rotation as usize) % keys.len()];
            black_box(player.play(key, &mut state));
        }
        samples.push(start.elapsed().as_nanos() as u64 / BATCH as u64);
    }
    let after = Snapshot::take();
    (
        Timing::from_sorted(&mut samples),
        before.allocations_since(after),
        before.frees_since(after),
    )
}

// ------------------------------------------------------------ key press ---

/// The whole input path: lock, look up, stamp, push.
///
/// The queue is drained before every batch so a full queue is never measured
/// as if it were latency; batches that could not be drained are counted
/// separately and reported.
fn bench_presses(pack_path: &PathBuf) -> (Timing, usize) {
    let mut runtime = Runtime::new(RATE);
    if runtime.load_pack(pack_path).is_err() {
        return (
            Timing {
                p50: 0,
                p99: 0,
                max: 0,
            },
            BATCHES,
        );
    }
    let keys: Vec<PhysicalKey> = runtime
        .pack()
        .map(|pack| pack.keys().iter().map(|entry| entry.physical_key).collect())
        .unwrap_or_default();

    let mut samples = Vec::with_capacity(BATCHES);
    let mut dropped = 0usize;
    for batch in 0..BATCHES {
        runtime.command_queue().drain(|_| {});
        let start = Instant::now();
        for step in 0..BATCH {
            if keys.is_empty() {
                break;
            }
            let key = keys[(batch + step) % keys.len()];
            if !runtime.trigger_key(key) {
                dropped += 1;
            }
        }
        samples.push(start.elapsed().as_nanos() as u64 / BATCH as u64);
    }
    runtime.command_queue().drain(|_| {});
    runtime.shutdown();
    (Timing::from_sorted(&mut samples), dropped)
}

// ---------------------------------------------------------------- memory ---

/// Resident-set growth after opening and closing the same pack `CYCLES` times.
fn bench_memory(pack_path: &PathBuf) -> u64 {
    // One warm-up pass: the first open touches the allocator's cold paths and
    // the kernel's page cache, neither of which is a leak.
    if let Ok(warm) = KvPack::open(pack_path) {
        drop(warm);
    }
    let before = crate::procfs::rss_kib();
    for _ in 0..CYCLES {
        if let Ok(pack) = KvPack::open(pack_path) {
            drop(pack);
        }
    }
    let after = crate::procfs::rss_kib();
    after.saturating_sub(before)
}

// ------------------------------------------------------------ live stream ---

/// Counts allocations in a window where the stream is rendering and this
/// thread is doing nothing but sleeping.
///
/// Returns `Err` with an operator-readable reason when there is no PipeWire
/// session to open a stream on - never a pass by default.
fn bench_live_callback(pack_path: &PathBuf) -> Result<(usize, u64), String> {
    let mut runtime = Runtime::new(RATE);
    runtime
        .load_pack(pack_path)
        .map_err(|error| format!("pack could not be loaded: {error}"))?;
    runtime
        .start_audio()
        .map_err(|error| format!("PipeWire stream could not be created: {error}"))?;
    if !runtime.wait_for_audio(Duration::from_secs(3)) {
        runtime.shutdown();
        return Err("PipeWire accepted no buffer within 3s (is a sink running?)".to_string());
    }

    // Everything this thread will touch for the rest of the window is built
    // now, so any allocation inside the window comes from another thread.
    let start = Snapshot::take();
    let callbacks_start = runtime.audio_stats().callbacks;
    std::thread::sleep(Duration::from_millis(750));
    let callbacks_end = runtime.audio_stats().callbacks;
    let end = Snapshot::take();
    runtime.shutdown();

    let callbacks = callbacks_end.saturating_sub(callbacks_start);
    Ok((start.allocations_since(end), callbacks))
}

// --------------------------------------------------------------- helpers ---

/// The pack this run measures, honouring `--pack`.
fn pick_pack(selector: Option<&str>) -> Option<PathBuf> {
    crate::select_pack(selector, &Config::default())
        .ok()
        .or_else(|| pack_locate::discover().into_iter().next())
}

/// Flags a number that only makes sense once the binary is optimized.
///
/// The budgets are real-time budgets; a `debug` build of the same code runs
/// roughly an order of magnitude slower, so a failure here is a note about
/// how it was invoked, not a regression.
fn profile_note() -> String {
    if cfg!(debug_assertions) {
        " (debug build: run with --release for the number that counts)".to_string()
    } else {
        String::new()
    }
}

/// A command with a valid, non-null sample pointer.
fn dummy_command() -> PlayCommand {
    static SAMPLES: [i16; 64] = [0; 64];
    PlayCommand {
        sample_ptr: SAMPLES.as_ptr(),
        sample_len: SAMPLES.len() as u32,
        source_rate: RATE,
        pitch_step: 1 << 32,
        left_gain: 1.0,
        right_gain: 1.0,
        release: false,
        enqueued_ns: 0,
    }
}
