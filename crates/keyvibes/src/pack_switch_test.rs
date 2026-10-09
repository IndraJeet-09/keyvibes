//! Phase 14 acceptance: `keyvibes pack-switch-test`.
//!
//! Switching packs must not stop the audio stream. The contract is:
//!
//! * the swap is decided and executed on the control plane,
//! * input threads pick the new pack up on their next key press - no
//!   reconnect, no stream teardown, no missed press,
//! * the pack the mixer may still be rendering from is **parked**, not
//!   freed, and is released only once nothing can reference it,
//! * sound keeps coming out across the whole switch.
//!
//! The test runs a sustained load while it swaps, so the old pack really is
//! still in use at the moment of the switch, and then shows the parked pack
//! draining to zero once the load stops.
//!
//! Steps that need a PipeWire session are reported as `NOT RUN` when there
//! is none - never as a pass.

use crate::accept::{Report, Status};
use crate::pack_locate;
use anyhow::Result;
use kv_core::{PhysicalKey, SoundSource, VariantState};
use kv_runtime::{Runtime, SimInputOptions};
use std::time::{Duration, Instant};

/// Runs the acceptance test.
pub fn run() -> Result<()> {
    let mut report = Report::new("pack-switch-test");

    // --- 1. both packs are installed and genuinely different ---------------
    let holy = match pack_locate::resolve_selector("Holy Panda") {
        Ok(path) => path,
        Err(error) => {
            report.not_run("two packs are installed", format!("{error:#}"));
            return report.finish();
        }
    };
    let linear = match pack_locate::resolve_selector("Linear") {
        Ok(path) => path,
        Err(error) => {
            report.not_run("two packs are installed", format!("{error:#}"));
            return report.finish();
        }
    };

    let holy_clips = clip_count(&holy);
    let linear_clips = clip_count(&linear);
    report.add(
        if holy_clips == Some(18) && linear_clips == Some(9) && holy != linear {
            Status::Pass
        } else {
            Status::Fail
        },
        "two distinct packs are installed",
        format!(
            "Holy Panda {} clip(s), Linear {} clip(s), paths differ: {}",
            holy_clips
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into()),
            linear_clips
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into()),
            holy != linear
        ),
    );

    // --- 2. build the runtime on the first pack ---------------------------
    let mut runtime = Runtime::new(48_000);
    if let Err(error) = runtime.load_pack(&holy) {
        report.fail("Holy Panda loads", error.to_string());
        return report.finish();
    }
    report.add(
        if runtime
            .pack()
            .is_some_and(|p| p.stats().name == "Holy Panda")
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "Holy Panda loads",
        describe_pack(&runtime),
    );

    // Two variants mean two different sample addresses for the same key -
    // the behavioural tell we use to prove the input side really changed.
    let before_rotates = varies_per_press(&runtime);
    report.add(
        if before_rotates {
            Status::Pass
        } else {
            Status::Fail
        },
        "starting pack rotates variants per press",
        format!("two presses of 'A' give different sample addresses: {before_rotates}"),
    );

    // --- 3. live: stream up, load running ---------------------------------
    if let Err(error) = runtime.start_audio() {
        report.not_run(
            "hot swap with the stream running",
            format!("cannot open an output stream here ({error}); is PipeWire running?"),
        );
        return report.finish();
    }
    let lifecycle = match runtime.audio() {
        Some(engine) => {
            if !engine.wait_until_active(Duration::from_secs(10)) {
                report.fail(
                    "hot swap with the stream running",
                    "stream never became active",
                );
                return report.finish();
            }
            Some(engine.lifecycle())
        }
        None => {
            report.not_run("hot swap with the stream running", "engine not started");
            return report.finish();
        }
    };

    let keys = [
        PhysicalKey::A,
        PhysicalKey::S,
        PhysicalKey::D,
        PhysicalKey::F,
        PhysicalKey::J,
        PhysicalKey::K,
        PhysicalKey::L,
        PhysicalKey::Space,
        PhysicalKey::Digit1,
    ];
    let load = SimInputOptions::stress(&keys, 40.0, 0x5717C4);
    if let Err(error) = runtime.start_simulated(load) {
        report.fail("load generator starts", error.to_string());
        return report.finish();
    }

    std::thread::sleep(Duration::from_millis(700));
    let before = runtime.audio_stats();
    report.add(
        if before.active_voices > 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "voices are sounding when the swap happens",
        format!(
            "{} voice(s), {} frame(s) rendered so far",
            before.active_voices, before.frames_rendered
        ),
    );

    // --- 4. swap while everything is running ------------------------------
    let started = Instant::now();
    let swap = runtime.switch_pack(&linear);
    let swap_elapsed = started.elapsed();

    let swap = match swap {
        Ok(report_value) => report_value,
        Err(error) => {
            report.fail("pack is swapped while the stream runs", error.to_string());
            return report.finish();
        }
    };

    report.add(
        if swap.from == "Holy Panda" && swap.to == "Linear" {
            Status::Pass
        } else {
            Status::Fail
        },
        "pack is swapped while the stream runs",
        format!(
            "{} -> {} in {:?} ({} pack(s) parked)",
            swap.from, swap.to, swap_elapsed, swap.retired
        ),
    );

    report.add(
        if runtime
            .pack()
            .is_some_and(|p| p.stats().name == "Linear" && p.stats().clips == 9)
        {
            Status::Pass
        } else {
            Status::Fail
        },
        "the runtime now reports the new pack",
        describe_pack(&runtime),
    );

    let handle_tracks = runtime
        .player()
        .is_some_and(|player| player.pack().stats().name == "Linear");
    report.add(
        if handle_tracks {
            Status::Pass
        } else {
            Status::Fail
        },
        "input threads' source now serves the new pack",
        if handle_tracks {
            "the shared source's current pack is Linear".to_string()
        } else {
            "the shared source still points at the old pack".to_string()
        },
    );

    let after_rotates = varies_per_press(&runtime);
    report.add(
        if !after_rotates {
            Status::Pass
        } else {
            Status::Fail
        },
        "new presses come from the new pack",
        if !after_rotates {
            "two presses of 'A' now give the same address (Linear has one variant)".to_string()
        } else {
            "two presses still differ - the input side did not switch".to_string()
        },
    );

    report.add(
        if runtime.retired_packs() >= 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "the previous pack is parked, not freed",
        format!("parked: {:?}", runtime.retired_pack_names()),
    );

    // --- 5. the stream never stopped --------------------------------------
    std::thread::sleep(Duration::from_millis(500));
    let after = runtime.audio_stats();
    let frames_kept_growing = after.frames_rendered > before.frames_rendered;
    let same_connection = after.reconnects == before.reconnects;
    let healthy = lifecycle
        .as_ref()
        .is_some_and(|state| !state.is_exhausted());
    let realtime_ok = after.deadline_misses == before.deadline_misses
        && after.budget_overruns == before.budget_overruns;
    report.add(
        if frames_kept_growing && same_connection && healthy && realtime_ok {
            Status::Pass
        } else {
            Status::Fail
        },
        "the stream keeps rendering across the swap",
        format!(
            "frames {} -> {}, reconnects {}, deadline misses {} -> {}, state {:?}",
            before.frames_rendered,
            after.frames_rendered,
            after.reconnects,
            before.deadline_misses,
            after.deadline_misses,
            after.stream_state
        ),
    );

    // --- 6. the parked pack drains once nothing can use it -----------------
    runtime.stop_simulated();
    let quiet_deadline = Instant::now() + Duration::from_secs(5);
    let mut quiescent = false;
    while Instant::now() < quiet_deadline {
        let stats = runtime.audio_stats();
        if stats.active_voices == 0 && runtime.command_queue().is_empty() {
            quiescent = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let retire_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < retire_deadline {
        if runtime.retire_packs() > 0 || runtime.retired_packs() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let remaining = runtime.retired_packs();
    report.add(
        if quiescent && remaining == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "the parked pack is released once it can no longer be referenced",
        format!("stream went quiet: {quiescent}, pack(s) still parked after 5s: {remaining}"),
    );

    // --- 7. it still makes sound afterwards --------------------------------
    let mut heard = false;
    if runtime.trigger_key(PhysicalKey::A) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if runtime.audio_stats().active_voices > 0 {
                heard = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    report.add(
        if heard { Status::Pass } else { Status::Fail },
        "sound still comes out after the switch",
        if heard {
            "a voice was scheduled from Linear".to_string()
        } else {
            "no voice appeared within 3s".to_string()
        },
    );

    let final_stats = runtime.audio_stats();
    report.add(
        if final_stats.deadline_misses == 0 && final_stats.budget_overruns == 0 {
            Status::Pass
        } else {
            Status::Fail
        },
        "real-time budget untouched by the whole exercise",
        format!(
            "deadline misses {}, budget overruns {}, {} callback(s)",
            final_stats.deadline_misses, final_stats.budget_overruns, final_stats.callbacks
        ),
    );

    report.finish()
}

fn clip_count(path: &std::path::Path) -> Option<u32> {
    kv_pack::KvPack::open(path)
        .ok()
        .map(|pack| pack.stats().clips)
}

fn describe_pack(runtime: &Runtime) -> String {
    match runtime.pack() {
        Some(pack) => {
            let stats = pack.stats();
            format!(
                "{}: {} key(s), {} clip(s)",
                stats.name, stats.keys, stats.clips
            )
        }
        None => "no pack loaded".to_string(),
    }
}

/// Whether two presses of the same key select different sample addresses.
///
/// Two-variant packs rotate; single-variant packs repeat. That difference is
/// visible only through the pack the shared source is actually serving.
fn varies_per_press(runtime: &Runtime) -> bool {
    let Some(player) = runtime.player() else {
        return false;
    };
    let mut state = VariantState::default();
    let first = match player.play(PhysicalKey::A, &mut state) {
        Some(command) => command.sample_ptr,
        None => return false,
    };
    let second = match player.play(PhysicalKey::A, &mut state) {
        Some(command) => command.sample_ptr,
        None => return false,
    };
    first != second
}
