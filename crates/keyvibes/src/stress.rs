//! Phase 8: real-time latency and XRUN validation.
//!
//! Measures the **software / audio-engine** path only:
//!
//! ```text
//! event received -> command queued -> dequeued by RT -> rendered
//! ```
//!
//! Nothing here measures the DAC, amplifier, transducer, or acoustic
//! propagation time, and no number printed by this module should be read as
//! "time from key press to sound at the ear".

use crate::pack_locate;
use anyhow::{bail, Context, Result};
use kv_audio_pipewire::{NodeXruns, StreamStats, NODE_NAME};
use kv_core::PhysicalKey;
use kv_runtime::{Runtime, SimInputOptions};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Keys the load script hammers.
fn stress_keys() -> Vec<PhysicalKey> {
    use PhysicalKey::*;
    vec![A, S, D, F, J, K, L, Space, Digit1]
}

/// `keyvibes stress`
pub struct StressOptions {
    pub pack: Option<PathBuf>,
    pub duration: u64,
    pub rate: u32,
    pub keys_per_second: f64,
    /// Mixer settings loaded from the user configuration file.
    pub settings: kv_core::Settings,
    /// PipeWire node to route to, from `output_device` in the config.
    pub output_device: Option<String>,
}

/// Runs the load test and returns `Ok(())` only when every check passes.
pub fn run(options: StressOptions) -> Result<()> {
    let pack_path = pack_locate::resolve(options.pack.as_deref())?;
    let duration = Duration::from_secs(options.duration.max(1));
    let mut runtime = Runtime::new(options.rate);
    runtime.set_settings(options.settings);
    runtime.set_output_device(options.output_device);

    runtime
        .load_pack(&pack_path)
        .with_context(|| format!("failed to load {}", pack_path.display()))?;
    println!("Pack:    {}", pack_path.display());

    runtime
        .start_audio()
        .context("failed to start audio output (is PipeWire running?)")?;
    if !runtime.wait_for_audio(Duration::from_secs(10)) {
        bail!("audio stream never became active within 10s");
    }

    // Baseline taken while the stream is running and before any load, so the
    // delta covers exactly the stress window.
    let xrun_baseline = NodeXruns::read(NODE_NAME);
    if xrun_baseline.is_none() {
        println!("  warning: could not read the PipeWire ERR counter yet");
    }

    let script = SimInputOptions::stress(&stress_keys(), options.keys_per_second, 0x5EED_1234);
    runtime
        .start_simulated(script)
        .context("failed to start the load generator")?;

    // The final ERR reading has to happen *while the stream still exists*:
    // once the supervisor tears it down the node disappears from the graph.
    // So the timer reads first and only then asks for shutdown.
    let control = runtime
        .audio_control()
        .context("audio engine has no control handle")?;
    let (xrun_tx, xrun_rx) = mpsc::channel::<Option<NodeXruns>>();
    let timer = std::thread::Builder::new()
        .name("keyvibes-stress-timer".to_string())
        .spawn(move || {
            std::thread::sleep(duration);
            let reading = NodeXruns::read(NODE_NAME);
            let _ = xrun_tx.send(reading);
            control.shutdown();
        })
        .context("failed to spawn the stress timer")?;

    println!(
        "Running {duration:?} of scripted input at ~{} keys/s ...",
        options.keys_per_second
    );
    let started = Instant::now();
    runtime.run().context("audio supervisor terminated")?;
    let elapsed = started.elapsed();
    let xrun_final = xrun_rx.recv().unwrap_or(None);
    let _ = timer.join();

    let xruns = match (xrun_baseline, xrun_final) {
        (Some(base), Some(final_reading)) if base.node_id == final_reading.node_id => {
            Some(final_reading.errors.saturating_sub(base.errors))
        }
        // Rebuilt mid-run: the counter restarted, so everything the new node
        // accumulated still belongs to our window.
        (_, Some(final_reading)) => Some(final_reading.errors),
        (None, None) | (Some(_), None) => None,
    };

    let audio = runtime.audio_stats();
    let input = runtime.input_stats();

    report(elapsed, &audio, &input, xruns, options.keys_per_second);

    let failures = evaluate(&audio, &input, xruns);
    if failures.is_empty() {
        println!("\nPASS  keyvibes stress");
        Ok(())
    } else {
        println!("\nFAIL  keyvibes stress");
        for failure in &failures {
            println!("  - {failure}");
        }
        bail!("{} real-time check(s) failed", failures.len());
    }
}

fn us(ns: u64) -> String {
    format!("{:.1}us", ns as f64 / 1_000.0)
}

fn report(
    elapsed: Duration,
    audio: &StreamStats,
    input: &kv_input_linux::diagnostics::InputStatsSnapshot,
    xruns: Option<u64>,
    keys_per_second: f64,
) {
    let mean_callback = audio
        .callback_ns_total
        .checked_div(audio.callbacks)
        .unwrap_or(0);

    println!("\nKeyVibes stress - software / audio-engine latency");
    println!("  (excludes DAC, amplifier, transducer and acoustic time)");
    println!(
        "  duration        {}s (generator offered ~{} keys/s)",
        elapsed.as_secs(),
        keys_per_second
    );
    println!(
        "  events          {} presses, {} commands, {} dropped",
        input.key_presses, input.commands_generated, input.commands_dropped
    );
    println!(
        "  input->command  p50 {}  p95 {}  p99 {}  max {}  ({} samples)",
        us(input.command_latency_p50_ns),
        us(input.command_latency_p95_ns),
        us(input.command_latency_p99_ns),
        us(input.command_latency_max_ns),
        input.command_latency_samples
    );
    println!(
        "  queue->render   p50 {}  p95 {}  p99 {}  max {}",
        us(audio.queue_latency_p50_ns),
        us(audio.queue_latency_p95_ns),
        us(audio.queue_latency_p99_ns),
        us(audio.queue_latency_max_ns)
    );
    println!(
        "  callback        p50 {}  p95 {}  p99 {}  max {}  mean {}",
        us(audio.callback_p50_ns),
        us(audio.callback_p95_ns),
        us(audio.callback_p99_ns),
        us(audio.callback_ns_max),
        us(mean_callback)
    );
    println!(
        "  callback slowest section: dequeue {}  drain {}  render {}",
        us(audio.dequeue_ns_max),
        us(audio.drain_ns_max),
        us(audio.render_ns_max)
    );
    println!(
        "  scheduling      quantum {}  safety budget {}  ({:.1}% of quantum)",
        us(audio.quantum_ns),
        us(audio.safety_budget_ns),
        audio.safety_budget_ns as f64 * 100.0 / (audio.quantum_ns as f64).max(1.0)
    );
    println!(
        "  rendering       {} frames, {} callbacks, {} commands drained, peak {:.4}",
        audio.frames_rendered, audio.callbacks, audio.drained_commands, audio.peak
    );
    println!(
        "  xruns (PipeWire ERR)  {}",
        match xruns {
            Some(count) => count.to_string(),
            None => "UNMEASURABLE (pw-top not available)".to_string(),
        }
    );
    println!(
        "  underruns (client)    {}   deadline misses {}   late callbacks {}",
        audio.producer_underruns(),
        audio.deadline_misses,
        audio.late_callbacks
    );
    println!(
        "  budget overruns {}   stream errors {}   reconnects {}",
        audio.budget_overruns, audio.stream_errors, audio.reconnects
    );
    println!(
        "  voices peak     {}   stream streaming: {}",
        audio.active_voices, audio.saw_streaming
    );
}

fn evaluate(
    audio: &StreamStats,
    input: &kv_input_linux::diagnostics::InputStatsSnapshot,
    xruns: Option<u64>,
) -> Vec<String> {
    let mut failures = Vec::new();

    match xruns {
        None => failures.push(
            "PipeWire XRUN count is unavailable: install pw-top (pipewire-bin / pipewire-tools)"
                .to_string(),
        ),
        Some(0) => {}
        Some(count) => failures.push(format!("PipeWire reported {count} XRUN(s)")),
    }

    if audio.deadline_misses != 0 {
        failures.push(format!(
            "{} audio callback deadline miss(es)",
            audio.deadline_misses
        ));
    }
    if audio.budget_overruns != 0 {
        failures.push(format!(
            "{} callback over the safety budget",
            audio.budget_overruns
        ));
    }
    if audio.late_callbacks != 0 {
        failures.push(format!("{} late callback(s)", audio.late_callbacks));
    }
    if audio.producer_underruns() != 0 {
        failures.push(format!(
            "{} producer underrun(s) (no buffer filled)",
            audio.producer_underruns()
        ));
    }
    if audio.stream_errors != 0 {
        failures.push(format!("{} PipeWire stream error(s)", audio.stream_errors));
    }
    if audio.callbacks == 0 || audio.frames_rendered == 0 {
        failures.push("audio never rendered a frame".to_string());
    }
    if !audio.within_safety_budget() {
        failures.push(format!(
            "callback max {} exceeds safety budget {}",
            us(audio.callback_ns_max),
            us(audio.safety_budget_ns)
        ));
    }
    if input.commands_dropped != 0 {
        failures.push(format!(
            "{} command(s) dropped (queue full or unmapped)",
            input.commands_dropped
        ));
    }
    if input.command_latency_samples == 0 {
        failures.push("no input-to-command samples recorded".to_string());
    }

    failures
}
