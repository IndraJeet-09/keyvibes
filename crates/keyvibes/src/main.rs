//! KeyVibes - Linux native low-latency keyboard sound engine.

use anyhow::{Context, Result};
use clap::Parser;
use kv_audio_pipewire::PipeWireStream;
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_input_linux::diagnostics::InputStats;
use kv_input_linux::error::InputError;
use kv_input_linux::{discovery::discover_keyboards, LinuxInputBackend};
use kv_pack::KvPack;
use kv_ring::SpscRing;
use kv_runtime::Runtime;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

mod cli;
mod diagnostics;

use cli::{Cli, Command, PackAction};
use diagnostics::Diagnostics;

/// Sound source used by diagnostics: captures keys but plays nothing.
struct SilentSource;

impl SoundSource for SilentSource {
    fn play(&self, _key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
        None
    }
}

/// Silent buffer handed to the command queue during `input-test`. Nothing
/// renders it: the queue has no consumer in that mode, so the counters climb
/// exactly the way they would when the engine stops consuming.
static PROBE_SAMPLES: [i16; 32] = [0; 32];

/// Sound source for `input-test`: always emits a command so that
/// `commands_generated` / `commands_dropped` statistics reflect real capture.
struct EventProbe {
    verbose: bool,
}

impl SoundSource for EventProbe {
    fn play(&self, key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
        if self.verbose {
            println!("  {key}");
        }
        Some(PlayCommand {
            sample_ptr: PROBE_SAMPLES.as_ptr(),
            sample_len: PROBE_SAMPLES.len() as u32,
            source_rate: 48_000,
            pitch_step: 1 << 32,
            left_gain: 0.0,
            right_gain: 0.0,
            release: false,
        })
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list_keyboards {
        return list_keyboards();
    }

    if cli.diagnostics {
        return run_diagnostics();
    }

    match cli.command {
        Some(Command::Pack { action }) => match action {
            PackAction::Build { manifest, output } => build_pack(&manifest, &output, cli.verbose),
            PackAction::Validate { pack } => validate_pack(&pack),
            PackAction::Inspect { pack, clips } => inspect_pack(&pack, clips),
        },
        Some(Command::Run { pack, rate }) => run_engine(&pack, rate, cli.verbose),
        Some(Command::InputTest { seconds }) => input_test(seconds, cli.verbose),
        None => {
            println!("KeyVibes v{}", env!("CARGO_PKG_VERSION"));
            println!("Run with --help to see available commands.");
            Ok(())
        }
    }
}

/// `keyvibes pack build <manifest> -o <output>`
fn build_pack(manifest: &Path, output: &Path, verbose: bool) -> Result<()> {
    println!("Building {}", manifest.display());

    let report = kv_pack::build_from_manifest(manifest, output, |event| match event {
        kv_pack::BuildEvent::Start { total_sources } => {
            if verbose {
                println!("  reading {total_sources} source file(s)");
            }
        }
        kv_pack::BuildEvent::Source { index, total, path } => {
            if verbose {
                println!("  [{index}/{total}] {}", path.display());
            }
        }
        kv_pack::BuildEvent::Writing => println!("  writing pack..."),
        kv_pack::BuildEvent::Validating => println!("  validating pack..."),
        kv_pack::BuildEvent::Finished => {}
    })
    .with_context(|| format!("failed to build pack from {}", manifest.display()))?;

    println!(
        "Built {}: {} keys, {} clips, {} Hz, {} bytes in {:?}",
        report.output.display(),
        report.key_count,
        report.clip_count,
        report.sample_rate,
        report.file_size,
        report.elapsed,
    );
    Ok(())
}

/// `keyvibes pack validate <pack>`
fn validate_pack(path: &Path) -> Result<()> {
    let pack =
        KvPack::open(path).with_context(|| format!("failed to validate {}", path.display()))?;

    let stats = pack.stats();
    println!("{}: OK", path.display());
    println!("  pack:    {}", stats.name);
    println!(
        "  content: {} keys, {} clips, {} logical frames",
        stats.keys, stats.clips, stats.sample_frames
    );
    println!(
        "  format:  {} Hz, {} channel, mapped {} bytes",
        stats.sample_rate, stats.channels, stats.mapped_size
    );
    Ok(())
}

/// `keyvibes pack inspect <pack> [--clips]`
fn inspect_pack(path: &Path, show_clips: bool) -> Result<()> {
    let pack = KvPack::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    let header = pack.header();
    let meta = pack.metadata();
    let stats = pack.stats();

    println!("Pack: {}", stats.name);
    if !meta.author.is_empty() {
        println!("  author:      {}", meta.author);
    }
    if !meta.description.is_empty() {
        println!("  description: {}", meta.description);
    }
    if !meta.license.is_empty() {
        println!("  license:     {}", meta.license);
    }
    if !meta.source.is_empty() {
        println!("  source:      {}", meta.source);
    }

    println!("Format:");
    println!(
        "  version {}, {}-byte header, {} bytes total",
        header.format_version,
        kv_pack::HEADER_SIZE,
        header.file_size
    );

    println!("Layout:");
    println!(
        "  metadata  offset {:>10} size {:>6}",
        header.metadata_offset, header.metadata_size
    );
    println!(
        "  key table offset {:>10} size {:>6}",
        header.key_table_offset, header.key_table_size
    );
    println!(
        "  clip table offset {:>9} size {:>6}",
        header.clip_table_offset, header.clip_table_size
    );
    println!(
        "  samples   offset {:>10} size {:>6}",
        header.sample_data_offset, header.sample_data_size
    );

    println!(
        "  {} keys, {} clips, {} Hz, {} channel, {} logical frames",
        stats.keys, stats.clips, stats.sample_rate, stats.channels, stats.sample_frames
    );

    println!("Keys:");
    for key in pack.keys() {
        println!(
            "  {:<16} variants={} first_clip={}",
            key.physical_key, key.variant_count, key.first_clip
        );
    }

    if show_clips {
        println!("Clips:");
        for (index, clip) in pack.clips().iter().enumerate() {
            println!(
                "  [{index:>3}] offset={} frames={} guards={}/{} stored={}",
                clip.sample_offset,
                clip.sample_frames,
                clip.guard_before,
                clip.guard_after,
                clip.stored_frames
            );
        }
    }

    Ok(())
}

/// `keyvibes run --pack <pack> [--rate <hz>]`
fn run_engine(pack: &Path, rate: u32, verbose: bool) -> Result<()> {
    let mut runtime = Runtime::new(rate);
    runtime
        .load_pack(pack)
        .with_context(|| format!("failed to load {}", pack.display()))?;

    let stats = runtime
        .pack()
        .map(KvPack::stats)
        .context("pack should be loaded")?;
    println!("Loaded {}", pack.display());
    println!(
        "  {}: {} keys, {} clips, {} Hz",
        stats.name, stats.keys, stats.clips, stats.sample_rate
    );

    runtime
        .start_audio()
        .context("failed to start audio output")?;
    runtime
        .start_input()
        .context("failed to start keyboard input")?;

    if verbose {
        let devices = runtime
            .input_backend()
            .map(|b| b.device_count())
            .unwrap_or(0);
        println!("  input devices: {devices}");
    }

    println!("Playing - press keys to trigger sounds, Ctrl+C to stop.");
    runtime.run().context("audio loop terminated")
}

/// `keyvibes --list-keyboards`
fn list_keyboards() -> Result<()> {
    let keyboards = discover_keyboards()?;

    if keyboards.is_empty() {
        println!("No keyboards found.");
        return Ok(());
    }

    println!("{} keyboard(s):", keyboards.len());
    for keyboard in keyboards {
        println!("  {} ({})", keyboard.name, keyboard.path.display());
    }
    Ok(())
}

/// `keyvibes input-test [--verbose] [-t <seconds>]`
fn input_test(seconds: u64, verbose: bool) -> Result<()> {
    let keyboards = discover_keyboards().context("failed to enumerate input devices")?;
    if keyboards.is_empty() {
        anyhow::bail!("No keyboards found (see docs/linux-input.md for troubleshooting)");
    }

    println!("{} keyboard(s):", keyboards.len());
    for keyboard in &keyboards {
        println!("  {} ({})", keyboard.name, keyboard.path.display());
        if let Some(phys) = &keyboard.phys {
            println!("      phys: {phys}");
        }
    }

    let stats = Arc::new(InputStats::new());
    let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(256));
    let probe = Arc::new(EventProbe { verbose });

    let backend =
        LinuxInputBackend::new(queue, stats.clone(), probe).context("failed to start capture")?;

    println!(
        "Capturing for {seconds}s - press keys now{}",
        if verbose {
            " (each press is printed)"
        } else {
            ""
        }
    );
    std::thread::sleep(Duration::from_secs(seconds));

    let snapshot = stats.snapshot();
    println!("{}", snapshot.format());
    println!("Devices tracked: {}", backend.device_count());
    Ok(())
}

/// `keyvibes --diagnostics`
fn run_diagnostics() -> Result<()> {
    println!("KeyVibes diagnostics");

    // Audio backend: connect a throwaway stream and report the result.
    let audio_queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(256));
    match PipeWireStream::new(48000, audio_queue) {
        Ok(stream) => {
            let stats = stream.get_stats();
            println!(
                "  PipeWire:    connected (frames={}, callbacks={})",
                stats.frames_rendered, stats.callbacks
            );
        }
        Err(e) => println!("  PipeWire:    unavailable - {e}"),
    }

    // Input backend: start a brief capture session with a silent source so
    // discovery, device open, and reader threads are all exercised.
    let input_stats = Arc::new(InputStats::new());
    let input_queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(256));
    let source = Arc::new(SilentSource);

    match LinuxInputBackend::new(input_queue, input_stats.clone(), source.clone()) {
        Ok(mut backend) => {
            println!("  Keyboards:   {} device(s)", backend.device_count());
            if let Err(e) = backend.enable_hotplug(source) {
                println!("  Hotplug:     unavailable - {e}");
            }
            // Let the reader threads spin up and process any pending events.
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(InputError::NoKeyboardsFound) => println!("  Keyboards:   none found"),
        Err(e) => println!("  Keyboards:   error - {e}"),
    }

    let diagnostics = Diagnostics::from_stats(&input_stats.snapshot());
    println!("{}", diagnostics.report());
    Ok(())
}
