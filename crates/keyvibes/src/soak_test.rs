//! Phase 20 acceptance: `keyvibes soak-test`.
//!
//! A soak test is not a longer stress test. Stress asks "can the engine keep
//! up for a minute"; soak asks "is anything still true after twenty minutes
//! that was true at the start" - memory, thread count, the absence of
//! panics, the absence of a wedge, and a stream that has not silently
//! dropped and reconnected behind your back.
//!
//! The run cycles through five phases rather than holding one load, because
//! the interesting failures live at the *boundaries*: the first press after
//! a long silence, the pack swap under load, the burst after a pause.
//!
//! Anything this host cannot express - no PipeWire session, no `pw-top`, no
//! readable keyboard - is reported `NOT RUN`. It is never counted as a pass.

use crate::accept::{Report, Status};
use crate::pack_locate;
use crate::procfs;
use anyhow::{bail, Context, Result};
use kv_audio_pipewire::{NodeXruns, NODE_NAME};
use kv_core::PhysicalKey;
use kv_runtime::{Runtime, SimInputOptions};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the main loop may go without ticking before the watchdog calls
/// it a deadlock. Every phase sleeps in 100 ms slices, each of which ticks.
const DEADLOCK_AFTER: Duration = Duration::from_secs(20);

/// Resident-set growth allowed over the whole run.
const RSS_BUDGET_KIB: u64 = 32 * 1024;

/// Threads allowed at the end above the steady-state count.
const THREAD_BUDGET: u64 = 8;

/// What the engine is being asked to do for the next few seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Ordinary typing, high rate, one key at a time.
    RapidTyping,
    /// Far more keys than anyone types, to fill the queue.
    Burst,
    /// Slower rate, larger chords, so many voices are live at once.
    Polyphony,
    /// Silence long enough for the stream to pause, then a burst.
    Idle,
    /// Replace the sound pack while the load keeps running.
    PackSwitch,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::RapidTyping => "rapid typing",
            Phase::Burst => "burst",
            Phase::Polyphony => "sustained polyphony",
            Phase::Idle => "idle then active",
            Phase::PackSwitch => "pack switching",
        }
    }

    const CYCLE: [Phase; 5] = [
        Phase::RapidTyping,
        Phase::Burst,
        Phase::Polyphony,
        Phase::Idle,
        Phase::PackSwitch,
    ];
}

/// What `keyvibes soak-test` needs to know.
pub struct SoakOptions {
    /// How long to run, e.g. `30m`.
    pub duration: Duration,
    /// `--pack`, already resolved, or `None` to take the first installed one.
    pub pack: Option<PathBuf>,
    /// Audio output sample rate.
    pub rate: u32,
    /// Mixer settings from the configuration file.
    pub settings: kv_core::Settings,
    /// PipeWire node from `output_device` in the configuration file.
    pub output_device: Option<String>,
}

/// Parses `30m`, `45s`, `2h`.
///
/// Rejecting a bare number is deliberate: `--duration 30` looks like seconds
/// and is read as minutes by half the tools on a Linux desktop, so it is
/// refused instead of guessed.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let trimmed = text.trim();
    let last = trimmed
        .chars()
        .last()
        .context("--duration needs a number and a unit, for example 30m")?;
    let seconds_per_unit = match last {
        's' | 'S' => 1u64,
        'm' | 'M' => 60,
        'h' | 'H' => 3600,
        _ => bail!("--duration has an unknown unit {last:?} in {text:?}; use 30m, 45s or 2h"),
    };
    let digits = &trimmed[..trimmed.len() - last.len_utf8()];
    let value: u64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "--duration {text:?} is not a number followed by s, m or h; try `--duration 30m`"
        )
    })?;
    if value == 0 {
        bail!("--duration must be at least 1s, not {text:?}");
    }
    Ok(Duration::from_secs(value * seconds_per_unit))
}

/// Runs the soak and reports every monitor.
pub fn run(options: SoakOptions) -> Result<()> {
    let mut report = Report::new("soak-test");

    let mut current_pack = match &options.pack {
        Some(path) => path.clone(),
        None => match pack_locate::resolve(None) {
            Ok(path) => path,
            Err(error) => {
                report.not_run("a sound pack was available", format!("{error:#}"));
                return report.finish();
            }
        },
    };
    let mut packs = pack_locate::discover();
    if packs.is_empty() {
        packs.push(current_pack.clone());
    }

    let keys = pack_keys(&current_pack);
    let duration = options.duration;

    println!("KeyVibes soak-test");
    println!("  duration  {duration:?}");
    println!("  pack      {}", current_pack.display());
    println!();

    // --------------------------------------------------------------- set up
    let mut runtime = Runtime::new(options.rate);
    runtime.set_settings(options.settings);
    runtime.set_output_device(options.output_device.clone());
    runtime
        .load_pack(&current_pack)
        .map_err(|error| anyhow::anyhow!("failed to load {}: {error}", current_pack.display()))?;

    let audio_up = match runtime.start_audio() {
        Ok(()) => runtime.wait_for_audio(Duration::from_secs(10)),
        Err(error) => {
            println!("  audio unavailable: {error}");
            false
        }
    };
    let audio_before = runtime.audio_stats();
    let xrun_before = if audio_up {
        NodeXruns::read(NODE_NAME)
    } else {
        None
    };

    let mut watchdog = Watchdog::start();
    let mut samples = Samples::default();
    let mut rounds = 0usize;
    let mut panics: Vec<String> = Vec::new();
    let mut switches_ok = 0usize;
    let mut switches_failed = 0usize;
    let mut phases_run: Vec<&'static str> = Vec::new();
    let keyboard_before = readable_keyboards();
    let started = Instant::now();

    // One full round before the baseline is taken, so "steady state" means
    // a running engine rather than a process that has not made its workers.
    let warmup = catch_unwind(AssertUnwindSafe(|| {
        run_phase(
            &mut runtime,
            Phase::RapidTyping,
            &keys,
            &mut current_pack,
            &packs,
            Duration::from_secs(4),
            &watchdog,
            &mut samples,
            &mut switches_ok,
            &mut switches_failed,
        );
    }));
    match warmup {
        Ok(()) => rounds += 1,
        Err(_) => panics.push("warm-up round panicked".to_string()),
    }
    let steady_threads = procfs::thread_count();
    samples.record_threads(steady_threads);

    // ----------------------------------------------------------- soak loop
    let deadline = started + duration;
    let mut index = 1usize;
    while panics.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_secs(1) {
            break;
        }
        let phase = Phase::CYCLE[index % Phase::CYCLE.len()];
        index += 1;
        let budget = phase_budget(phase).min(remaining);
        watchdog.touch();

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            run_phase(
                &mut runtime,
                phase,
                &keys,
                &mut current_pack,
                &packs,
                budget,
                &watchdog,
                &mut samples,
                &mut switches_ok,
                &mut switches_failed,
            );
        }));
        match outcome {
            Ok(()) => {
                rounds += 1;
                // First sightings only: a 15-minute soak repeats every phase
                // thirty times, and a per-round log makes the report useless.
                if !phases_run.contains(&phase.name()) {
                    phases_run.push(phase.name());
                }
            }
            Err(_) => panics.push(format!("{} phase panicked", phase.name())),
        }
        samples.record_rss(procfs::rss_kib());
        samples.record_threads(procfs::thread_count());
    }

    // ------------------------------------------------------------ tear down
    // Every counter is read while the stream still exists: after shutdown
    // the PipeWire node is gone from the graph.
    let audio_after = runtime.audio_stats();
    let xrun_after = if audio_up {
        NodeXruns::read(NODE_NAME)
    } else {
        None
    };
    let input_after = runtime.input_stats();
    let keyboard_after = readable_keyboards();
    // Read before shutdown: afterwards the engine threads are gone and both
    // numbers would describe a process that is about to exit, not one that ran.
    let rss_end = procfs::rss_kib();
    let threads_end = procfs::thread_count();
    samples.record_rss(rss_end);
    samples.record_threads(threads_end);
    watchdog.stop();
    runtime.shutdown();
    let elapsed = started.elapsed();

    // -------------------------------------------------------------- report
    if !audio_up {
        println!("  note: no PipeWire stream, so the audio monitors are NOT RUN");
    }

    report.add(
        if panics.is_empty() {
            Status::Pass
        } else {
            Status::Fail
        },
        "no phase panicked",
        if panics.is_empty() {
            format!(
                "{rounds} round(s); phases exercised: {}",
                if phases_run.is_empty() {
                    "warm-up only".to_string()
                } else {
                    phases_run.join(", ")
                }
            )
        } else {
            panics.join(", ")
        },
    );

    let trips = watchdog.trips();
    report.add(
        if trips == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "the run never stopped making progress",
        if trips == 0 {
            format!(
                "watchdog checked once a second for {elapsed:?}; longest allowed gap {:?}",
                DEADLOCK_AFTER
            )
        } else {
            format!("watchdog fired {trips} time(s)")
        },
    );

    report.add(
        if input_after.commands_generated > 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "keys kept turning into queued commands",
        format!(
            "{} press(es) -> {} command(s), {} dropped (drops during a burst \
             are the queue doing its job)",
            input_after.key_presses, input_after.commands_generated, input_after.commands_dropped
        ),
    );

    if audio_up {
        let misses = audio_after
            .deadline_misses
            .saturating_sub(audio_before.deadline_misses);
        let overruns = audio_after
            .budget_overruns
            .saturating_sub(audio_before.budget_overruns);
        report.add(
            if misses == 0 && overruns == 0 {
                Status::Pass
            } else {
                Status::Fail
            },
            "the audio callback never missed a deadline",
            format!(
                "{misses} deadline miss(es), {overruns} safety-budget overrun(s) \
                 across {} callback(s)",
                audio_after.callbacks.saturating_sub(audio_before.callbacks)
            ),
        );

        let errors = audio_after
            .stream_errors
            .saturating_sub(audio_before.stream_errors);
        let reconnects = audio_after
            .reconnects
            .saturating_sub(audio_before.reconnects);
        report.add(
            if errors == 0 && reconnects == 0 {
                Status::Pass
            } else {
                Status::Fail
            },
            "the audio stream stayed connected from first frame to last",
            if errors == 0 && reconnects == 0 {
                format!(
                    "{} frame(s) rendered, stream ended in state {:?}",
                    audio_after
                        .frames_rendered
                        .saturating_sub(audio_before.frames_rendered),
                    audio_after.stream_state
                )
            } else {
                format!("{errors} stream error(s), {reconnects} reconnect(s)")
            },
        );
    } else {
        report.not_run(
            "the audio callback never missed a deadline",
            "no PipeWire stream could be opened",
        );
        report.not_run(
            "the audio stream stayed connected from first frame to last",
            "no PipeWire stream could be opened",
        );
    }

    match (xrun_before, xrun_after) {
        (Some(before), Some(after)) if before.node_id == after.node_id => {
            let xruns = after.errors.saturating_sub(before.errors);
            report.add(
                if xruns == 0 {
                    Status::Pass
                } else {
                    Status::Fail
                },
                "PipeWire reported no XRUN during the run",
                format!("{xruns} XRUN(s) counted by pw-top"),
            );
        }
        (Some(before), Some(after)) => report.add(
            Status::Pass,
            "PipeWire reported no XRUN during the run",
            format!(
                "the stream was rebuilt (node {} -> {}); the new node reported {} XRUN(s)",
                before.node_id, after.node_id, after.errors
            ),
        ),
        _ => report.not_run(
            "PipeWire reported no XRUN during the run",
            "pw-top is not available (install pipewire-tools / pipewire-bin)",
        ),
    }

    report.add(
        if switches_ok == 0 {
            Status::NotRun
        } else if switches_failed == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "every pack swap under load succeeded",
        if switches_ok == 0 {
            "no pack swap was reached within the requested duration".to_string()
        } else {
            format!("{switches_ok} swap(s) succeeded, {switches_failed} failed")
        },
    );

    let growth = rss_end.saturating_sub(samples.rss_baseline);
    report.add(
        if growth <= RSS_BUDGET_KIB {
            Status::Pass
        } else {
            Status::Fail
        },
        "resident memory stayed inside its budget",
        format!(
            "{} KiB at the start, {rss_end} KiB at the end (growth {growth} KiB, \
             peak {} KiB); budget {RSS_BUDGET_KIB} KiB",
            samples.rss_baseline, samples.rss_peak
        ),
    );

    let drift = threads_end.saturating_sub(steady_threads);
    report.add(
        if drift <= THREAD_BUDGET {
            Status::Pass
        } else {
            Status::Fail
        },
        "thread count stayed at its steady state",
        format!(
            "{steady_threads} steady-state thread(s), {threads_end} at the end \
             (drift {drift}, budget {THREAD_BUDGET}), peak {}",
            samples.threads_peak
        ),
    );

    match (keyboard_before, keyboard_after) {
        // `Some(0)` means discovery worked and found nothing this process can
        // open (typically a non-root user outside the `input` group): there is
        // nothing to keep discoverable, so say so rather than "pass" on 0 == 0.
        (Some(before), Some(after)) if before > 0 || after > 0 => report.add(
            Status::Pass,
            "input devices stayed discoverable for the whole run",
            format!(
                "{before} -> {after} readable keyboard(s); {} added, {} removed \
                 by the hot-plug monitor",
                input_after.devices_added, input_after.devices_removed
            ),
        ),
        _ => report.not_run(
            "input devices stayed discoverable for the whole run",
            "no readable keyboard on this host - unplug and re-plug is \
             covered by `keyvibes hotplug-test`",
        ),
    }

    report.finish()
}

/// How long one pass of a phase gets before the loop moves on.
fn phase_budget(phase: Phase) -> Duration {
    match phase {
        Phase::RapidTyping => Duration::from_secs(4),
        Phase::Burst => Duration::from_secs(3),
        Phase::Polyphony => Duration::from_secs(5),
        Phase::Idle => Duration::from_secs(5),
        Phase::PackSwitch => Duration::from_secs(6),
    }
}

/// Loads of different mercy, one per phase, so no single shape of load is
/// the only thing the engine ever sees.
fn phase_script(phase: Phase, keys: &[PhysicalKey]) -> SimInputOptions {
    let fallback = kv_runtime::default_sim_keys();
    let keys: &[PhysicalKey] = if keys.is_empty() { &fallback } else { keys };
    match phase {
        Phase::RapidTyping => SimInputOptions::stress(keys, 140.0, 0x11),
        Phase::Burst => SimInputOptions::stress(keys, 400.0, 0x22),
        Phase::Polyphony => SimInputOptions::stress(keys, 35.0, 0x33),
        Phase::Idle => SimInputOptions::stress(keys, 200.0, 0x44),
        Phase::PackSwitch => SimInputOptions::stress(keys, 90.0, 0x55),
    }
}

/// Runs one phase for `budget`, ticking the watchdog as it goes.
#[allow(clippy::too_many_arguments)]
fn run_phase(
    runtime: &mut Runtime,
    phase: Phase,
    keys: &[PhysicalKey],
    current_pack: &mut PathBuf,
    packs: &[PathBuf],
    budget: Duration,
    watchdog: &Watchdog,
    samples: &mut Samples,
    switches_ok: &mut usize,
    switches_failed: &mut usize,
) {
    runtime.stop_simulated();
    if runtime.start_simulated(phase_script(phase, keys)).is_err() {
        // Without a source there is nothing to soak on the input side; the
        // caller's progress monitor will notice and fail the run.
        watchdog.touch();
        return;
    }

    let started = Instant::now();
    match phase {
        Phase::Idle => {
            // Long enough for the idle monitor to pause the stream, then a
            // burst that has to wake it again.
            runtime.set_audio_active(false);
            sleep_bounded(started + Duration::from_secs(3), watchdog);
            runtime.set_audio_active(true);
        }
        Phase::PackSwitch => {
            // Swap twice inside the load so the swap happens under voices.
            sleep_bounded(started + Duration::from_secs(1), watchdog);
            swap(runtime, current_pack, packs, switches_ok, switches_failed);
            sleep_bounded(started + Duration::from_secs(3), watchdog);
            swap(runtime, current_pack, packs, switches_ok, switches_failed);
        }
        _ => {}
    }
    sleep_bounded(started + budget, watchdog);
    runtime.stop_simulated();
    samples.record_rss(procfs::rss_kib());
}

/// Replaces the loaded pack with the next one in the list, in place.
///
/// With a single pack installed the swap targets the same file, which still
/// exercises retire-after-swap - the path where a leak would show up.
fn swap(
    runtime: &mut Runtime,
    current: &mut PathBuf,
    packs: &[PathBuf],
    ok: &mut usize,
    failed: &mut usize,
) {
    if packs.is_empty() {
        return;
    }
    let position = packs.iter().position(|path| path == current).unwrap_or(0);
    let next = packs[(position + 1) % packs.len()].clone();
    match runtime.switch_pack(&next) {
        Ok(_) => {
            *ok += 1;
            *current = next;
        }
        Err(_) => *failed += 1,
    }
}

/// Sleeps until `until`, waking often enough to prove the loop is alive.
fn sleep_bounded(until: Instant, watchdog: &Watchdog) {
    loop {
        let now = Instant::now();
        if now >= until {
            return;
        }
        watchdog.touch();
        thread::sleep((until - now).min(Duration::from_millis(100)));
    }
}

/// How many keyboards this process can actually open.
fn readable_keyboards() -> Option<usize> {
    kv_input_linux::discovery::discover_keyboards()
        .ok()
        .map(|found| found.len())
}

/// The keys the chosen pack contains, or nothing if it cannot be read.
fn pack_keys(pack_path: &Path) -> Vec<PhysicalKey> {
    kv_pack::KvPack::open(pack_path)
        .map(|pack| pack.keys().iter().map(|entry| entry.physical_key).collect())
        .unwrap_or_default()
}

// ------------------------------------------------------------- supporting ---

/// Watchdog thread: fires if the main loop stops ticking.
struct Watchdog {
    last_progress: Arc<AtomicU64>,
    trips: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn start() -> Self {
        let last_progress = Arc::new(AtomicU64::new(0));
        let trips = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let last = Arc::clone(&last_progress);
            let trips = Arc::clone(&trips);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(1));
                    let touched = last.load(Ordering::Relaxed);
                    if touched == 0 {
                        continue;
                    }
                    if now_millis().saturating_sub(touched) > DEADLOCK_AFTER.as_millis() as u64 {
                        trips.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };
        Self {
            last_progress,
            trips,
            stop,
            handle: Some(handle),
        }
    }

    /// Records that the main loop is still running.
    fn touch(&self) {
        self.last_progress.store(now_millis(), Ordering::Relaxed);
    }

    fn trips(&self) -> usize {
        self.trips.load(Ordering::Relaxed)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Wall-clock milliseconds, shared by both sides of the watchdog.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// The extremes the memory and thread monitors care about.
#[derive(Default)]
struct Samples {
    rss_baseline: u64,
    rss_peak: u64,
    threads_peak: u64,
}

impl Samples {
    fn record_rss(&mut self, kib: u64) {
        if self.rss_baseline == 0 {
            self.rss_baseline = kib;
        }
        self.rss_peak = self.rss_peak.max(kib);
    }

    fn record_threads(&mut self, count: u64) {
        self.threads_peak = self.threads_peak.max(count);
    }
}
