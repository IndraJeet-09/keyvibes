//! Phase 11 acceptance: `keyvibes idle-test`.
//!
//! An idle system must cost almost nothing, and a key press must bring the
//! stream back immediately - not on the monitor's next poll tick.
//!
//! The command runs one full idle cycle against a real PipeWire stream:
//!
//! 1. stay quiet until the engine pauses the stream,
//! 2. show that a paused stream is charged no audio callbacks at all,
//! 3. press a key and measure how long until a voice is scheduled,
//! 4. press a key again after re-idling, and require the same latency,
//! 5. confirm the real-time budget was never disturbed.
//!
//! The wake path exercised in step 3 is the production one: the input
//! pipeline calls [`kv_core::StreamWake::wake`] *before* it queues the
//! command, exactly as `Runtime::trigger_key` does here.
//!
//! Anything that cannot run on this host (no PipeWire session, no pack
//! loaded) is reported as `NOT RUN` - never as a pass.

use crate::accept::{Report, Status};
use crate::pack_locate;
use anyhow::Result;
use kv_audio_pipewire::idle::{IdleConfig, IdlePhase};
use kv_core::PhysicalKey;
use kv_runtime::Runtime;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Options for `keyvibes idle-test`.
pub struct IdleTestOptions {
    /// Quiet time required before the stream is paused (seconds).
    pub idle_after: u64,
    /// Pack used to produce sound (defaults to a discovered pack).
    pub pack: Option<PathBuf>,
}

/// Runs the acceptance test.
pub fn run(options: IdleTestOptions) -> Result<()> {
    let mut report = Report::new("idle-test");

    let pack = match pack_locate::resolve(options.pack.as_deref()) {
        Ok(path) => path,
        Err(error) => {
            report.not_run(
                "idle pause and wake",
                format!("no pack available: {error:#}"),
            );
            return report.finish();
        }
    };

    let idle_after = Duration::from_secs(options.idle_after.max(1));
    let poll = Duration::from_millis(25);

    let mut runtime = Runtime::new(48_000);
    runtime.set_idle_config(IdleConfig {
        enabled: true,
        idle_after,
        poll,
    });

    if let Err(error) = runtime.load_pack(&pack) {
        report.not_run("idle pause and wake", format!("pack did not load: {error}"));
        return report.finish();
    }
    let pack_name = runtime
        .pack()
        .map(|loaded| loaded.stats().name.clone())
        .unwrap_or_default();

    if let Err(error) = runtime.start_audio() {
        report.not_run(
            "idle pause and wake",
            format!("cannot open an output stream here ({error}); is PipeWire running?"),
        );
        return report.finish();
    }

    let engine = match runtime.audio() {
        Some(engine) => engine,
        None => {
            report.not_run("idle pause and wake", "engine not started");
            return report.finish();
        }
    };
    if !engine.wait_until_active(Duration::from_secs(10)) {
        report.fail(
            "stream reaches a streaming state",
            "output never became active within 10s",
        );
        return report.finish();
    }

    // Budget for "immediately": three quanta of slack plus 50 ms. Any wake
    // that needs longer than this is indistinguishable from a stall to a
    // person typing.
    let quantum = runtime.audio_stats().quantum_ns;
    let budget = Duration::from_nanos(quantum.saturating_mul(3)) + Duration::from_millis(50);
    report.pass(
        "wake budget computed from the negotiated quantum",
        format!(
            "quantum {:?}, budget {:?}, idle-after {idle_after:?}",
            Duration::from_nanos(quantum),
            budget
        ),
    );

    // --- 1. quiet long enough, the stream pauses ----------------------------
    let paused = wait_for(idle_after + Duration::from_secs(5), || {
        runtime.idle_phase() == Some(IdlePhase::Paused)
    });
    let idle = runtime
        .idle_state()
        .map(|state| state.snapshot())
        .expect("audio started, so idle state exists");
    report.add(
        if paused { Status::Pass } else { Status::Fail },
        "stream pauses after the quiet threshold",
        format!(
            "phase {:?}, {} pause(s) after {:?}",
            runtime.idle_phase(),
            idle.pauses,
            idle_after
        ),
    );
    if !paused {
        return report.finish();
    }

    // --- 2. a paused stream is charged nothing ------------------------------
    let before = runtime.audio_stats();
    std::thread::sleep(Duration::from_millis(300));
    let after = runtime.audio_stats();
    let frozen = after.callbacks == before.callbacks;
    report.add(
        if frozen && after.active_voices == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "paused stream costs zero audio callbacks",
        format!(
            "callbacks {} -> {} over 300ms, active voices {}",
            before.callbacks, after.callbacks, after.active_voices
        ),
    );

    // --- 3 + 4. two independent wake cycles ---------------------------------
    let mut wake_latencies = Vec::new();
    for cycle in 1..=2usize {
        if cycle > 1 {
            let re_idled = wait_for(idle_after + Duration::from_secs(5), || {
                runtime.idle_phase() == Some(IdlePhase::Paused)
            });
            if !re_idled {
                report.fail(
                    "stream re-pauses between wake cycles",
                    format!("cycle {cycle}: phase {:?}", runtime.idle_phase()),
                );
                break;
            }
        }

        let queued = runtime.trigger_key(PhysicalKey::A);
        if !queued {
            report.fail(
                "key press queues a sound",
                format!("pack '{pack_name}' has no clip bound to 'A'"),
            );
            continue;
        }

        let started = Instant::now();
        let mut latency = None;
        while started.elapsed() < budget * 4 {
            if runtime.audio_stats().active_voices > 0 {
                latency = Some(started.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }

        match latency {
            Some(elapsed) => {
                wake_latencies.push(elapsed);
                report.add(
                    if elapsed <= budget {
                        Status::Pass
                    } else {
                        Status::Fail
                    },
                    if cycle == 1 {
                        "key press wakes the paused stream immediately".to_string()
                    } else {
                        format!("key press on cycle {cycle} wakes the stream immediately")
                    },
                    format!("{elapsed:?} to first voice, budget {budget:?}"),
                );
            }
            None => report.fail(
                "key press wakes the paused stream immediately",
                format!(
                    "cycle {cycle}: no voice within {:?} - the wake did not take",
                    budget * 4
                ),
            ),
        }
    }

    // --- 5. audio actually resumed, not just a counter bump -----------------
    let resume_started = Instant::now();
    let mut frames_grew = false;
    let frames_at_trigger = runtime.audio_stats().frames_rendered;
    while resume_started.elapsed() < Duration::from_secs(2) {
        if runtime.audio_stats().frames_rendered > frames_at_trigger {
            frames_grew = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let resumed = runtime
        .idle_phase()
        .is_some_and(|phase| phase != IdlePhase::Paused);
    let idle_after_wake = runtime
        .idle_state()
        .map(|state| state.snapshot())
        .unwrap_or_default();
    report.add(
        if frames_grew && resumed && idle_after_wake.resumes >= 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "stream resumes and renders after the wake",
        format!(
            "frames {} -> {}, phase {:?}, {} resume(s)",
            frames_at_trigger,
            runtime.audio_stats().frames_rendered,
            runtime.idle_phase(),
            idle_after_wake.resumes
        ),
    );

    // --- 6. the real-time path stayed undisturbed ---------------------------
    let final_stats = runtime.audio_stats();
    report.add(
        if final_stats.deadline_misses == 0
            && final_stats.budget_overruns == 0
            && engine.stats().within_safety_budget()
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "real-time budget intact across idle and wake cycles",
        format!(
            "deadline misses {}, budget overruns {}, {} callback(s) total",
            final_stats.deadline_misses, final_stats.budget_overruns, final_stats.callbacks
        ),
    );

    // --- 7. the monitor kept running without spinning ----------------------
    let monitor = runtime
        .idle_state()
        .map(|state| state.snapshot())
        .unwrap_or_default();
    report.add(
        if monitor.evaluations > 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "idle monitor is evaluating on its poll interval",
        format!(
            "{} evaluation(s), longest quiet {:?}",
            monitor.evaluations, monitor.longest_quiet
        ),
    );

    if !wake_latencies.is_empty() {
        let worst = wake_latencies.iter().copied().max().expect("non-empty");
        println!();
        println!(
            "  wake latency: {} cycle(s), worst {worst:?} (budget {budget:?})",
            wake_latencies.len()
        );
    }

    report.finish()
}

/// Polls `condition` until it holds or `timeout` elapses.
fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}
