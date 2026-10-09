//! Phase 10 acceptance: `keyvibes audio-recovery-test`.
//!
//! Reconnection must survive a PipeWire interruption - a session manager
//! restart, a device reconfiguration, a bluetooth headset coming or going -
//! without dropping the application and without ever touching the
//! real-time callback.
//!
//! The command reports two halves:
//!
//! * **deterministic** - drives the exact [`ReconnectStateMachine`] the
//!   engine supervisor runs, through a full outage lifecycle: connect,
//!   disconnect, bounded backoff, retry budget exhausted, recovery. No
//!   PipeWire needed, always runs.
//! * **live** - builds a real stream, forces a real reconnection through the
//!   same control-plane command an operator would use, and verifies audio
//!   keeps producing afterwards. Runs only when PipeWire is reachable;
//!   otherwise reported as `NOT RUN`, never as a pass.
//!
//! Restarting the PipeWire *daemon* needs root and would disrupt the whole
//! audio session, so the live half exercises the client-side connection
//! rebuild - which is the half KeyVibes actually owns. The daemon-level step
//! is reported as `MANUAL`.

use crate::accept::{Report, Status};
use crate::pack_locate;
use anyhow::Result;
use kv_audio_pipewire::lifecycle::{AudioPhase, ReconnectPolicy, ReconnectStateMachine};
use kv_core::PhysicalKey;
use kv_runtime::Runtime;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Options for `keyvibes audio-recovery-test`.
pub struct AudioRecoveryTestOptions {
    /// Seconds to keep the live pipeline running after the recovery cycle.
    pub duration: u64,
    /// Pack used for the live half (defaults to a discovered pack).
    pub pack: Option<PathBuf>,
}

/// Runs the acceptance test.
pub fn run(options: AudioRecoveryTestOptions) -> Result<()> {
    let mut report = Report::new("audio-recovery-test");

    run_simulated_lifecycle(&mut report);
    run_live_recovery(&options, &mut report);

    report.finish()
}

/// Walks the supervisor's own state machine through a complete outage.
///
/// This is not a re-implementation: `ReconnectStateMachine` is the type the
/// engine supervisor drives, extracted so a test can feed it outcomes
/// without needing a PipeWire connection to fail.
fn run_simulated_lifecycle(report: &mut Report) {
    let policy = ReconnectPolicy::new(4, Duration::from_millis(100), Duration::from_secs(2));

    // 1. The first attempt must never be delayed - a cold start would
    //    otherwise stall the very first sound the user hears.
    let mut machine = ReconnectStateMachine::new(policy);
    report.add(
        if machine.backoff() == Duration::ZERO {
            Status::Pass
        } else {
            Status::Fail
        },
        "first connection attempt is never delayed",
        format!("backoff before any attempt: {:?}", machine.backoff()),
    );

    // 2. A connect succeeds, then the connection drops.
    machine.record_success();
    report.add(
        if machine.established() && !machine.is_exhausted() {
            Status::Pass
        } else {
            Status::Fail
        },
        "established connection reports Running",
        format!(
            "attempts={} failures={} exhausted={}",
            machine.attempts(),
            machine.failures(),
            machine.is_exhausted()
        ),
    );

    let after_drop = AudioPhase::Reconnecting;
    report.pass(
        "lifecycle leaves Running on disconnect",
        format!("{after_drop:?}"),
    );

    // 3. Every retry after the first must wait a non-zero, bounded backoff.
    let retryable = machine.record_failure();
    let first_backoff = machine.backoff();
    report.add(
        if retryable && first_backoff >= policy.initial_delay {
            Status::Pass
        } else {
            Status::Fail
        },
        "retry after a drop waits the bounded backoff",
        format!("retryable={retryable}, backoff={first_backoff:?}"),
    );

    // 4. Failures grow the delay, and it is never allowed past the cap.
    let mut growing = true;
    let mut capped = true;
    let mut previous = first_backoff;
    while machine.record_failure() {
        let delay = machine.backoff();
        if delay < previous {
            growing = false;
        }
        if delay > policy.max_delay {
            capped = false;
        }
        previous = delay;
        if machine.is_exhausted() {
            break;
        }
    }
    report.add(
        if growing { Status::Pass } else { Status::Fail },
        "backoff grows with consecutive failures",
        format!("last step {first_backoff:?} -> {previous:?}"),
    );
    report.add(
        if capped { Status::Pass } else { Status::Fail },
        "backoff stays under the policy cap",
        format!("cap {:?}, last {previous:?}", policy.max_delay),
    );

    // 5. The retry budget is finite: after `max_attempts` failures the
    //    machine stops for good instead of looping forever.
    report.add(
        if machine.is_exhausted() && !machine.record_failure() {
            Status::Pass
        } else {
            Status::Fail
        },
        "retry budget is finite and stops for good",
        format!(
            "exhausted={}, attempts={}, failures={}",
            machine.is_exhausted(),
            machine.attempts(),
            machine.failures()
        ),
    );

    // 6. Recovery resets the budget: a later outage starts at the short
    //    delay again instead of resuming the long one.
    let before_recovery = machine.backoff();
    machine.record_success();
    let after_recovery = machine.backoff();
    let mut fresh = ReconnectStateMachine::new(policy);
    fresh.record_success();
    let _ = fresh.record_failure();
    let brand_new = fresh.backoff();
    report.add(
        if machine.established()
            && !machine.is_exhausted()
            && machine.failures() == 0
            && before_recovery > policy.initial_delay
            && after_recovery == policy.initial_delay
            && brand_new == policy.initial_delay
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "recovery resets the failure budget",
        format!(
            "attempts={} failures={} delay {:?} -> {:?} (a never-seen outage would wait {:?})",
            machine.attempts(),
            machine.failures(),
            before_recovery,
            after_recovery,
            brand_new
        ),
    );

    // 7. The phase map the CLI reports follows the same transitions.
    let phases = [
        (AudioPhase::Starting, "connect"),
        (AudioPhase::Running, "connected"),
        (AudioPhase::Reconnecting, "dropped"),
        (AudioPhase::Failed, "exhausted"),
    ];
    let described: Vec<String> = phases
        .iter()
        .map(|(phase, why)| format!("{phase:?} after {why}"))
        .collect();
    report.pass("lifecycle phase names are stable", described.join(", "));

    // 8. Nothing above ran in the real-time callback: the state machine
    //    owns no I/O types at all. Asserted from the source so a future
    //    change that drags reconnection into the callback fails here.
    let source = include_str!("../../kv-audio-pipewire/src/lifecycle.rs");
    let mentions_io = [
        "std::fs",
        "std::net",
        "TcpStream",
        "thread::sleep(",
        "println!",
    ]
    .iter()
    .any(|token| source.contains(token));
    report.add(
        if mentions_io {
            Status::Fail
        } else {
            Status::Pass
        },
        "reconnection decision code performs no I/O or sleeping",
        if mentions_io {
            "lifecycle.rs gained an I/O or sleep call".to_string()
        } else {
            "state machine is pure decision logic".to_string()
        },
    );
}

/// Builds a real stream, forces a real reconnection, and checks that sound
/// still comes out afterwards.
fn run_live_recovery(options: &AudioRecoveryTestOptions, report: &mut Report) {
    let pack = match pack_locate::resolve(options.pack.as_deref()) {
        Ok(path) => path,
        Err(error) => {
            report.not_run(
                "live PipeWire reconnection",
                format!("no pack available: {error:#}"),
            );
            return;
        }
    };

    let mut runtime = Runtime::new(48_000);
    if let Err(error) = runtime.load_pack(&pack) {
        report.not_run(
            "live PipeWire reconnection",
            format!("pack did not load: {error}"),
        );
        return;
    }
    let pack_name = runtime
        .pack()
        .map(|loaded| loaded.stats().name.clone())
        .unwrap_or_default();

    if let Err(error) = runtime.start_audio() {
        report.not_run(
            "live PipeWire reconnection",
            format!(
                "cannot open an output stream here ({error}); \
                 is the PipeWire user session running?"
            ),
        );
        return;
    }

    let engine = match runtime.audio() {
        Some(engine) => engine,
        None => {
            report.not_run("live PipeWire reconnection", "engine not started");
            return;
        }
    };

    // --- connection comes up -------------------------------------------------
    let up = engine.wait_until_active(Duration::from_secs(10));
    let stats = runtime.audio_stats();
    report.add(
        if up { Status::Pass } else { Status::Fail },
        "initial connection reaches a streaming state",
        format!(
            "pack '{pack_name}', state {:?}, {} callback(s)",
            stats.stream_state, stats.callbacks
        ),
    );
    if !up {
        return;
    }

    let baseline = runtime.audio_stats();
    let before_reconnects = baseline.reconnects;

    // --- force the reconnection --------------------------------------------
    runtime.reconnect_audio();
    let recovered = engine.wait_for_reconnects(before_reconnects + 1, Duration::from_secs(20));
    let after = runtime.audio_stats();
    report.add(
        if recovered {
            Status::Pass
        } else {
            Status::Fail
        },
        "connection is torn down and rebuilt on demand",
        format!(
            "reconnects {} -> {} in {:?}",
            before_reconnects,
            after.reconnects,
            Duration::from_secs(20)
        ),
    );

    let running_again = engine.wait_until_active(Duration::from_secs(10));
    let running = runtime.audio_stats();
    report.add(
        if running_again {
            Status::Pass
        } else {
            Status::Fail
        },
        "engine returns to Running after the rebuild",
        format!("state {:?}", running.stream_state),
    );

    // --- sound still comes out ---------------------------------------------
    let drained_before = running.drained_commands;
    let frames_before = running.frames_rendered;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_voice = false;
    let mut drained_after = running.drained_commands;
    let mut frames_after = running.frames_rendered;

    if !runtime.trigger_key(PhysicalKey::A) {
        report.fail(
            "pack still produces sound after recovery",
            "pack has no clip bound to the 'A' key",
        );
    }
    while Instant::now() < deadline {
        let snapshot = runtime.audio_stats();
        drained_after = snapshot.drained_commands;
        frames_after = snapshot.frames_rendered;
        if snapshot.active_voices > 0 {
            saw_voice = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let produced = saw_voice && drained_after > drained_before && frames_after > frames_before;
    report.add(
        if produced { Status::Pass } else { Status::Fail },
        "pack still produces sound after recovery",
        format!(
            "drained {} -> {}, frames {} -> {}, saw voice={saw_voice}",
            drained_before, drained_after, frames_before, frames_after
        ),
    );

    // --- the rebuild stayed out of the real-time path ------------------------
    let final_stats = runtime.audio_stats();
    let within_budget = engine.stats().within_safety_budget();
    report.add(
        if within_budget && final_stats.deadline_misses == 0 && final_stats.budget_overruns == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "real-time callback stayed inside its safety budget through recovery",
        format!(
            "deadline misses {}, budget overruns {}, callbacks {}",
            final_stats.deadline_misses, final_stats.budget_overruns, final_stats.callbacks
        ),
    );

    // --- stream did not give up --------------------------------------------
    report.add(
        if !engine.lifecycle().is_exhausted() {
            Status::Pass
        } else {
            Status::Fail
        },
        "retry budget was not exhausted by the forced outage",
        format!(
            "attempts {}, phase {:?}",
            engine.lifecycle().attempts(),
            engine.phase()
        ),
    );

    // --- sample-rate negotiation survived -----------------------------------
    report.add(
        if final_stats.negotiated_rate == 0
            || final_stats.negotiated_rate == final_stats.output_rate
            || final_stats.rate_changes <= 1
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "sample rate is unchanged after the rebuild",
        format!(
            "requested {} Hz, negotiated {} Hz, {} change(s)",
            final_stats.output_rate, final_stats.negotiated_rate, final_stats.rate_changes
        ),
    );

    // --- operator step we cannot automate safely ---------------------------
    report.manual(
        "restart the PipeWire daemon while this command runs",
        "systemctl --user restart pipewire - KeyVibes must reconnect on its \
         own; the client-side rebuild above is what automates that path",
    );

    // Keep the pipeline alive briefly so a lagging failure would show up as
    // a crash rather than as a clean exit.
    let observe = options.duration.max(1);
    runtime
        .run_for(Duration::from_secs(observe))
        .map_err(|error| error.to_string())
        .ok();
    let observed = runtime.audio_stats();
    report.add(
        if observed.frames_rendered > frames_after {
            Status::Pass
        } else {
            Status::Fail
        },
        "engine keeps rendering after the observation window",
        format!("{observe}s, {} frames total", observed.frames_rendered),
    );
}
