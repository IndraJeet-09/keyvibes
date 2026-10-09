//! Phase 9 acceptance: `keyvibes hotplug-test`.
//!
//! The command reports three kinds of result and never blurs them:
//!
//! * **PASS/FAIL** - checks the process can run here and now, plus a
//!   deterministic walk of the exact device state machine the live pipeline
//!   uses (`DeviceRegistry`).
//! * **NOT RUN** - needs a keyboard this session cannot read, or needs a
//!   human to physically unplug something.
//! * **MANUAL** - the step the operator has to perform while the test runs.
//!
//! Hardware-dependent steps are reported as `NOT RUN` rather than quietly
//! counted as passes: a simulated pass proves the state machine, not the
//! cable.

use crate::pack_locate;
use anyhow::{bail, Result};
use evdev::{EventType, InputEvent, KeyCode as Key};
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_input_linux::diagnostics::InputStats;
use kv_input_linux::discovery::KeyboardInfo;
use kv_input_linux::pipeline::DeviceRegistry;
use kv_ring::SpscRing;
use kv_runtime::Runtime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Options for `keyvibes hotplug-test`.
pub struct HotplugTestOptions {
    /// Seconds to keep the live pipeline running.
    pub duration: u64,
    /// Pack used to drive the live half (defaults to a discovered pack).
    pub pack: Option<PathBuf>,
}

/// A source that always answers, so the test can count produced commands.
struct AlwaysSource {
    clip: Vec<i16>,
}

impl SoundSource for AlwaysSource {
    fn play(&self, _key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
        Some(unsafe {
            PlayCommand::new(
                self.clip.as_ptr(),
                self.clip.len() as u32,
                48000,
                1 << 32,
                1.0,
                1.0,
                false,
            )
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    NotRun,
    Manual,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::NotRun => "NOT RUN",
            Status::Manual => "MANUAL",
        }
    }
}

struct Check {
    status: Status,
    name: String,
    detail: String,
}

fn check(results: &mut Vec<Check>, status: Status, name: &str, detail: impl Into<String>) {
    results.push(Check {
        status,
        name: name.to_string(),
        detail: detail.into(),
    });
}

/// Runs the acceptance test.
pub fn run(options: HotplugTestOptions) -> Result<()> {
    let mut results = Vec::new();

    println!("KeyVibes hotplug test");
    println!();

    run_simulated_lifecycle(&mut results);
    run_live_pipeline(&options, &mut results);

    let mut failures = 0usize;
    for result in &results {
        println!(
            "  [{:<8}] {}{}",
            result.status.label(),
            result.name,
            if result.detail.is_empty() {
                String::new()
            } else {
                format!(" - {}", result.detail)
            }
        );
        if result.status == Status::Fail {
            failures += 1;
        }
    }

    let not_run = results
        .iter()
        .filter(|r| r.status == Status::NotRun || r.status == Status::Manual)
        .count();

    println!();
    if failures > 0 {
        bail!("{failures} hotplug check(s) failed");
    }
    if not_run > 0 {
        println!("PASS  keyvibes hotplug-test ({not_run} hardware step(s) not run on this host)");
    } else {
        println!("PASS  keyvibes hotplug-test");
    }
    Ok(())
}

/// Walks keyboard A / keyboard B through add, remove and reconnect against
/// the same `DeviceRegistry` the live pipeline drives.
fn run_simulated_lifecycle(results: &mut Vec<Check>) {
    const A: &str = "/dev/input/event7";
    const B: &str = "/dev/input/event9";

    let queue = Arc::new(SpscRing::with_capacity(64));
    let stats = Arc::new(InputStats::new());
    let source = Arc::new(AlwaysSource {
        clip: (0..512).map(|i| (i % 64) as i16 - 32).collect(),
    });
    let mut registry = DeviceRegistry::new(source, queue.clone(), stats.clone());

    let info = |path: &str, name: &str| KeyboardInfo {
        path: PathBuf::from(path),
        name: name.to_string(),
        phys: None,
    };
    let key = |code: Key, pressed: bool| {
        InputEvent::new(EventType::KEY.0, code.code(), i32::from(pressed))
    };
    let press = |registry: &mut DeviceRegistry<AlwaysSource>, path: &Path, code: Key| {
        registry.handle_event(path, &key(code, true));
        registry.handle_event(path, &InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0));
    };
    let drained = |queue: &Arc<SpscRing<PlayCommand>>| {
        let mut count = 0;
        while queue.pop().is_some() {
            count += 1;
        }
        count
    };

    // --- keyboard A connected -------------------------------------------
    let attached_a = registry.attach(&info(A, "keyboard A"));
    check(
        results,
        if attached_a {
            Status::Pass
        } else {
            Status::Fail
        },
        "keyboard A attached",
        format!("{} device(s) tracked", registry.len()),
    );

    // --- keyboard B connected -------------------------------------------
    let attached_b = registry.attach(&info(B, "keyboard B"));
    check(
        results,
        if attached_a && attached_b && registry.len() == 2 {
            Status::Pass
        } else {
            Status::Fail
        },
        "keyboard B attached alongside A",
        format!("{} device(s) tracked", registry.len()),
    );

    press(&mut registry, Path::new(A), Key::KEY_S);
    press(&mut registry, Path::new(B), Key::KEY_L);
    let after_first_presses = drained(&queue);
    check(
        results,
        if after_first_presses == 2 && registry.held_keys() == 2 {
            Status::Pass
        } else {
            Status::Fail
        },
        "both keyboards produce commands",
        format!(
            "{after_first_presses} command(s), {} key(s) held",
            registry.held_keys()
        ),
    );

    // --- keyboard A removed ---------------------------------------------
    let removed = registry.detach(Path::new(A));
    let a_state_gone = registry.state(Path::new(A)).is_none();
    let b_still_held = registry
        .state(Path::new(B))
        .map(|s| s.is_pressed(PhysicalKey::L))
        .unwrap_or(false);
    check(
        results,
        if removed && a_state_gone && b_still_held && registry.held_keys() == 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "removing A preserves B's pressed state",
        format!(
            "{} device(s) tracked, {} key(s) held",
            registry.len(),
            registry.held_keys()
        ),
    );

    press(&mut registry, Path::new(B), Key::KEY_K);
    let after_removal = drained(&queue);
    check(
        results,
        if after_removal == 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "remaining keyboard still accepts input",
        format!("{after_removal} command(s) after A was unplugged"),
    );

    // --- keyboard A reconnected -----------------------------------------
    let reattached = registry.attach(&info(A, "keyboard A"));
    let stale_state_cleared = registry
        .state(Path::new(A))
        .map(|s| !s.is_pressed(PhysicalKey::S))
        .unwrap_or(false);
    press(&mut registry, Path::new(A), Key::KEY_D);
    let after_reconnect = drained(&queue);
    check(
        results,
        if reattached && stale_state_cleared && after_reconnect == 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "keyboard A works after reconnect",
        format!("{after_reconnect} command(s), stale pressed state cleared: {stale_state_cleared}"),
    );

    // --- SYN_DROPPED -----------------------------------------------------
    let held_before = registry.held_keys();
    registry.handle_event(
        Path::new(A),
        &InputEvent::new(EventType::SYNCHRONIZATION.0, 1, 0),
    );
    let held_after = registry.held_keys();
    let dropped_count = stats.snapshot().sync_dropped;
    check(
        results,
        if held_after == held_before - 1 && dropped_count == 1 {
            Status::Pass
        } else {
            Status::Fail
        },
        "SYN_DROPPED resets only the affected keyboard",
        format!("{held_before} -> {held_after} key(s) held"),
    );

    // --- events for a vanished path --------------------------------------
    registry.handle_event(Path::new("/dev/input/event404"), &key(Key::KEY_A, true));
    let commands_after_stray = stats.snapshot().commands_generated;
    check(
        results,
        if commands_after_stray == 4 {
            Status::Pass
        } else {
            Status::Fail
        },
        "events for an unknown device are ignored",
        format!(
            "{commands_after_stray} command(s) generated in total; an unknown \
             device path produced none"
        ),
    );

    // --- bounded read-error recovery -------------------------------------
    let mut gave_up = false;
    for _ in 0..32 {
        if registry.record_read_error(Path::new(B)) {
            gave_up = true;
            break;
        }
    }
    check(
        results,
        if gave_up { Status::Pass } else { Status::Fail },
        "read errors are retried only a bounded number of times",
        "device would be detached instead of retried forever",
    );

    check(
        results,
        Status::Pass,
        "process remained alive through the whole lifecycle",
        "simulated add/remove/reconnect completed without a panic",
    );
}

/// Starts the real evdev pipeline and keeps it running.
fn run_live_pipeline(options: &HotplugTestOptions, results: &mut Vec<Check>) {
    let pack_path = match pack_locate::resolve(options.pack.as_deref()) {
        Ok(path) => path,
        Err(error) => {
            check(
                results,
                Status::NotRun,
                "live keyboard pipeline",
                format!("no pack available: {error}"),
            );
            return;
        }
    };

    let mut runtime = Runtime::new(48000);
    if let Err(error) = runtime.load_pack(&pack_path) {
        check(
            results,
            Status::NotRun,
            "live keyboard pipeline",
            format!("could not load {}: {error}", pack_path.display()),
        );
        return;
    }

    let started = match runtime.start_input() {
        Ok(started) => started,
        Err(error) => {
            check(
                results,
                Status::Fail,
                "live keyboard pipeline starts",
                format!("{error}"),
            );
            return;
        }
    };
    check(
        results,
        Status::Pass,
        "live keyboard pipeline starts",
        format!("{started:?}"),
    );

    let duration = Duration::from_secs(options.duration.max(1));
    let deadline = Instant::now() + duration;
    let mut iterations = 0u32;
    while Instant::now() < deadline {
        iterations += 1;
        std::thread::sleep(Duration::from_millis(100));
    }

    let alive = runtime
        .input_backend()
        .map(|backend| backend.is_running())
        .unwrap_or(false);
    check(
        results,
        if alive { Status::Pass } else { Status::Fail },
        "pipeline thread still running after the wait",
        format!("{duration:?}, {iterations} wake-up(s)"),
    );

    let attached = runtime
        .input_backend()
        .map(|backend| backend.device_count())
        .unwrap_or(0);
    let stats = runtime.input_stats();

    if attached == 0 {
        check(
            results,
            Status::NotRun,
            "readable keyboards attached",
            "no keyboard is readable by this session - add yourself to the \
             'input' group and log out/in (see docs/linux-permissions.md)",
        );
        check(
            results,
            Status::NotRun,
            "unplug / reconnect with keys pressed",
            "needs a physical keyboard attached to this machine",
        );
        return;
    }

    check(
        results,
        Status::Pass,
        "readable keyboards attached",
        format!("{attached} device(s)"),
    );
    check(
        results,
        Status::Pass,
        "no crash during the observation window",
        format!(
            "{} press(es), {} removal(s), {} read error(s)",
            stats.key_presses, stats.devices_removed, stats.read_errors
        ),
    );
    check(
        results,
        Status::Manual,
        "unplug keyboard A, reconnect it, keep typing",
        "run this command again while doing so; it must stay alive and \
         keep accepting keys",
    );
}
