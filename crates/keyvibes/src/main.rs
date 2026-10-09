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
use kv_runtime::{InputStart, Runtime};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod accept;
mod audio_cmd;
mod audio_recovery_test;
mod cli;
mod compat_test;
mod config;
mod config_test;
mod diagnostics;
mod hotplug_test;
mod idle_test;
mod pack_locate;
mod pack_switch_test;
mod pack_test;
mod stress;

use cli::{Cli, Command, PackAction};
use config::Config;
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
            enqueued_ns: 0,
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
            PackAction::List => list_packs(cli.verbose),
            PackAction::Build { manifest, output } => build_pack(&manifest, &output, cli.verbose),
            PackAction::Validate { pack } => validate_pack(&pack),
            PackAction::Inspect { pack, clips } => inspect_pack(&pack, clips),
        },
        Some(Command::Run {
            rate,
            simulate,
            duration,
        }) => run_engine(
            cli.sound_pack.as_deref(),
            rate,
            simulate,
            duration,
            cli.verbose,
        ),
        Some(Command::Stress {
            duration,
            rate,
            keys_per_second,
        }) => {
            let (_path, config) = load_config()?;
            let pack = select_pack(cli.sound_pack.as_deref(), &config)?;
            let options = stress::StressOptions {
                pack: Some(pack),
                duration,
                rate,
                keys_per_second,
                settings: config.settings(),
                output_device: config.output_device,
            };
            stress::run(options)
        }
        Some(Command::InputTest { seconds }) => input_test(seconds, cli.verbose),
        Some(Command::HotplugTest { duration }) => {
            hotplug_test::run(hotplug_test::HotplugTestOptions {
                duration,
                pack: Some(select_pack(cli.sound_pack.as_deref(), &Config::default())?),
            })
        }
        Some(Command::AudioRecoveryTest { duration }) => {
            audio_recovery_test::run(audio_recovery_test::AudioRecoveryTestOptions {
                duration,
                pack: Some(select_pack(cli.sound_pack.as_deref(), &Config::default())?),
            })
        }
        Some(Command::IdleTest { idle_after }) => idle_test::run(idle_test::IdleTestOptions {
            idle_after,
            pack: Some(select_pack(cli.sound_pack.as_deref(), &Config::default())?),
        }),
        Some(Command::PackSwitchTest) => pack_switch_test::run(),
        Some(Command::CompatibilityTest) => compat_test::run(),
        Some(Command::PackTest) => pack_test::run(),
        Some(Command::ConfigTest) => config_test::run(),
        Some(Command::Analyze { sources }) => audio_cmd::analyze(&sources),
        Some(Command::Process { input, output }) => audio_cmd::process(&input, &output),
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
        kv_pack::BuildEvent::Processed { key, variant, report } => {
            if verbose {
                println!(
                    "    processed {key} variant {variant}: {} -> {} frames, peak {:.2} -> {:.2} dBFS, rms {:.1} -> {:.1} dBFS, gain {:+.1} dB",
                    report.original_frames,
                    report.processed_frames,
                    report.original_peak_dbfs,
                    report.final_peak_dbfs,
                    report.original_rms_dbfs,
                    report.final_rms_dbfs,
                    report.gain_db,
                );
                for warning in &report.warnings {
                    println!("      warning: {warning}");
                }
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

/// `keyvibes run [--pack <name-or-path>] [--rate <hz>] [--simulate] [--duration <s>]`
fn run_engine(
    selector: Option<&str>,
    rate: u32,
    simulate: bool,
    duration: u64,
    verbose: bool,
) -> Result<()> {
    let (config_path, config) = load_config()?;
    if !config.enabled {
        anyhow::bail!(
            "KeyVibes is disabled in {} (set `enabled = true`)",
            config_path.display()
        );
    }
    let pack = select_pack(selector, &config)?;

    let mut runtime = Runtime::new(rate);
    runtime.set_settings(config.settings());
    runtime.set_output_device(config.output_device.clone());
    runtime
        .load_pack(&pack)
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
    if verbose {
        println!("  config: {}", config_path.display());
        println!("  volume: {}", config.volume);
        println!(
            "  device: {}",
            config.output_device.as_deref().unwrap_or("<default sink>")
        );
    }

    runtime
        .start_audio()
        .context("failed to start audio output (is PipeWire running?)")?;

    if simulate {
        let keys = kv_runtime::default_sim_keys();
        let options = kv_runtime::SimInputOptions::from_keys(&keys, Duration::from_millis(60), 0);
        runtime
            .start_simulated(options)
            .context("failed to start simulated input")?;
        println!("Simulated input: diagnostic mode, no physical keyboard is read.");
    } else {
        match runtime.start_input() {
            Ok(InputStart::Keyboards(count)) => {
                println!("{count} keyboard(s) attached.");
            }
            Ok(InputStart::WaitingForKeyboard) => {
                println!("No readable keyboard yet - waiting for one to appear.");
                println!(
                    "  hint: add yourself to the 'input' group, then log out/in\n\
                     \x20      (see docs/linux-permissions.md)"
                );
            }
            Err(error) => return Err(error).context("failed to start keyboard input"),
        }
    }

    if verbose {
        let devices = runtime
            .input_backend()
            .map(|b| b.device_count())
            .unwrap_or(0);
        println!("  input devices: {devices}");
    }

    if duration > 0 {
        println!("Playing for {duration}s - press keys to trigger sounds.");
        runtime
            .run_for(Duration::from_secs(duration))
            .context("audio loop terminated")?;
        let input = runtime.input_stats();
        let audio = runtime.audio_stats();
        println!(
            "Done: {} presses, {} commands queued, {} frames rendered, peak {:.3}",
            input.key_presses, input.commands_generated, audio.frames_rendered, audio.peak
        );
        if audio.frames_rendered == 0 {
            anyhow::bail!("audio never rendered a frame");
        }
        if audio.peak <= 0.0 {
            anyhow::bail!("audio rendered only silence");
        }
        Ok(())
    } else {
        println!("Playing - press keys to trigger sounds, Ctrl+C to stop.");
        runtime.run().context("audio loop terminated")
    }
}

/// Loads the user configuration from its default location.
///
/// A missing file yields the defaults; a file that exists but cannot be
/// read is a hard error, because guessing there would silently ignore what
/// the user wrote.
fn load_config() -> Result<(PathBuf, Config)> {
    let path = Config::default_path().context("cannot determine the config file location")?;
    let config =
        Config::load(&path).with_context(|| format!("failed to load {}", path.display()))?;
    Ok((path, config))
}

/// Picks the pack for a command: `--pack`, then `pack = ...` in the config,
/// then the first installed pack.
fn select_pack(selector: Option<&str>, config: &Config) -> Result<PathBuf> {
    if let Some(selector) = selector {
        return pack_locate::resolve_selector(selector);
    }
    if let Some(selector) = config.pack.as_deref() {
        return pack_locate::resolve_selector(selector);
    }
    pack_locate::resolve(None)
}

/// `keyvibes pack list`
fn list_packs(verbose: bool) -> Result<()> {
    let dirs = pack_locate::search_dirs();
    let found = pack_locate::discover();

    if verbose {
        println!("Searched:");
        for dir in &dirs {
            println!(
                "  {}{}",
                dir.display(),
                if dir.exists() { "" } else { " (missing)" }
            );
        }
        println!();
    }

    if found.is_empty() {
        println!("No packs installed. Searched:");
        for dir in &dirs {
            println!("  {}", dir.display());
        }
        println!();
        println!("Build one with `keyvibes pack build <pack.toml> -o <out.kvpack>`");
        return Ok(());
    }

    println!("{} pack(s):", found.len());
    let mut width = 4usize;
    let mut rows = Vec::new();
    for path in &found {
        match KvPack::open(path) {
            Ok(loaded) => {
                let stats = loaded.stats();
                width = width.max(stats.name.len());
                rows.push((
                    stats.name,
                    stats.keys,
                    stats.clips,
                    stats.sample_rate,
                    path.display().to_string(),
                    None::<String>,
                ));
            }
            Err(error) => {
                width = width.max(4);
                rows.push((
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string(),
                    0,
                    0,
                    0,
                    path.display().to_string(),
                    Some(error.to_string()),
                ));
            }
        }
    }
    for (name, keys, clips, rate, path, error) in rows {
        match error {
            Some(error) => println!("  {name:<width$}  INVALID: {error}"),
            None => {
                println!("  {name:<width$}  {keys:>2} keys  {clips:>3} clips  {rate:>5} Hz  {path}")
            }
        }
    }
    println!();
    println!("Load one with `keyvibes --pack \"<name>\" run`");
    Ok(())
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

    let backend = LinuxInputBackend::new(queue, stats.clone(), probe, None)
        .context("failed to start capture")?;

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
    // discovery, device open, and the pipeline thread are all exercised.
    let input_stats = Arc::new(InputStats::new());
    let input_queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(256));
    let source = Arc::new(SilentSource);

    match LinuxInputBackend::new(input_queue, input_stats.clone(), source, None) {
        Ok(mut backend) => {
            println!("  Keyboards:   {} device(s)", backend.device_count());
            if let Err(e) = backend.enable_hotplug() {
                println!("  Hotplug:     unavailable - {e}");
            }
            // Let the pipeline spin up and process any pending events.
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(InputError::NoKeyboardsFound) => println!("  Keyboards:   none found"),
        Err(e) => println!("  Keyboards:   error - {e}"),
    }

    let diagnostics = Diagnostics::from_stats(&input_stats.snapshot());
    println!("{}", diagnostics.report());
    Ok(())
}
