//! KeyVibes - Linux native low-latency keyboard sound engine.

use anyhow::{Context, Result};
use clap::Parser;
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_input_linux::diagnostics::InputStats;
use kv_input_linux::discovery::discover_keyboards;
use kv_input_linux::LinuxInputBackend;
use kv_pack::KvPack;
use kv_ring::SpscRing;
use kv_runtime::{InputStart, Runtime};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod accept;
mod alloc_probe;
mod audio_cmd;
mod audio_recovery_test;
mod bench;
mod cli;
mod cli_test;
mod compat_test;
mod config;
mod config_test;
mod doctor;
mod doctor_test;
mod hotplug_test;
mod idle_test;
mod pack_locate;
mod pack_switch_test;
mod pack_test;
mod procfs;
mod security_test;
mod soak_test;
mod stress;

use cli::{Cli, Command, ConfigAction, PackAction};
use config::Config;

/// Every heap allocation this process makes is counted, so the benchmark
/// can prove the real-time path never makes one.
#[global_allocator]
static ALLOCATOR: alloc_probe::Counting = alloc_probe::Counting;

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
        Some(Command::Benchmark) => bench::run(cli.sound_pack.as_deref()),
        Some(Command::SoakTest { duration }) => {
            let duration = soak_test::parse_duration(&duration)?;
            let (_path, config) = load_config()?;
            soak_test::run(soak_test::SoakOptions {
                duration,
                pack: Some(select_pack(cli.sound_pack.as_deref(), &config)?),
                rate: 48_000,
                settings: config.settings(),
                output_device: config.output_device.clone(),
            })
        }
        Some(Command::DoctorTest) => doctor_test::run(),
        Some(Command::Doctor) => doctor::run(cli.sound_pack.as_deref()),
        Some(Command::Config { action }) => match action {
            ConfigAction::Show => config_show(),
            ConfigAction::Path => config_path(),
            ConfigAction::Init { force } => config_init(force),
        },
        Some(Command::CliTest) => cli_test::run(),
        Some(Command::SecurityTest) => security_test::run(),
        Some(Command::ConfigTest) => config_test::run(),
        Some(Command::Analyze { sources }) => audio_cmd::analyze(&sources),
        Some(Command::Process { input, output }) => audio_cmd::process(&input, &output),
        // A bare `keyvibes` is `keyvibes run`: the program exists to make
        // sound, so the no-argument case does that.
        None => run_engine(cli.sound_pack.as_deref(), 48_000, false, 0, cli.verbose),
    }
}

/// `keyvibes pack build <manifest> -o <output>`
fn build_pack(manifest: &Path, output: &Path, verbose: bool) -> Result<()> {
    if !manifest.is_file() {
        anyhow::bail!(
            "no manifest at {}\n  a pack manifest is a TOML file - start from \
             `assets/soundpacks/default-src/pack.toml` and pass its path as the \
             first argument",
            manifest.display()
        );
    }
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

/// Rejects an argument that is not a readable file, before any pack code
/// touches it, so the message can say where to look instead of repeating an
/// OS errno.
fn require_pack_file(path: &Path) -> Result<()> {
    if path.is_dir() {
        anyhow::bail!(
            "{} is a directory, not a sound pack\n  fix: point at a `.kvpack` \
             file - `keyvibes pack list` shows the installed ones",
            path.display()
        );
    }
    if !path.exists() {
        anyhow::bail!(
            "no file at {}\n  fix: check the path, or use `--pack <name>` for \
             an installed pack - `keyvibes pack list` shows them",
            path.display()
        );
    }
    Ok(())
}

/// `keyvibes pack validate <pack>`
fn validate_pack(path: &Path) -> Result<()> {
    require_pack_file(path)?;
    let pack = KvPack::open(path).with_context(|| format!("failed to read {}", path.display()))?;

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
    require_pack_file(path)?;
    let pack = KvPack::open(path).with_context(|| format!("failed to read {}", path.display()))?;

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

/// `keyvibes config path`
///
/// Prints one line and nothing else, so a script can capture it.
fn config_path() -> Result<()> {
    let path = Config::default_path().context("cannot resolve a configuration path")?;
    println!("{}", path.display());
    Ok(())
}

/// `keyvibes config show`
fn config_show() -> Result<()> {
    let path = Config::default_path().context("cannot resolve a configuration path")?;
    let on_disk = read_config_table(&path)?;
    let config = Config::load(&path)?;

    println!("configuration file: {}", path.display());
    match &on_disk {
        Some(_) => println!("  in use"),
        None => println!("  missing - every value below is a default"),
    }

    // A value is "file" only when the key is actually present: an explicit
    // `enabled = true` and an absent `enabled` both end up true, but only one
    // of them is the user's doing.
    let origin = |key: &str| match &on_disk {
        Some(table) if table.contains_key(key) => "file",
        _ => "default",
    };

    let rows: [(&str, String, &str); 9] = [
        ("enabled", config.enabled.to_string(), "enabled"),
        ("pack", display_option(&config.pack), "pack"),
        (
            "output_device",
            display_option(&config.output_device),
            "output_device",
        ),
        ("volume", format!("{}", config.volume), "volume"),
        (
            "pitch_variation",
            config.pitch_variation.to_string(),
            "pitch_variation",
        ),
        (
            "gain_variation",
            config.gain_variation.to_string(),
            "gain_variation",
        ),
        (
            "release_sounds",
            config.release_sounds.to_string(),
            "release_sounds",
        ),
        (
            "spatial_audio",
            config.spatial_audio.to_string(),
            "spatial_audio",
        ),
        ("version", config.version.to_string(), "version"),
    ];

    println!();
    for (label, value, key) in rows {
        println!("{label:<16}= {value:<10} ({})", origin(key));
    }
    Ok(())
}

/// `keyvibes config init [--force]`
fn config_init(force: bool) -> Result<()> {
    let path = Config::default_path().context("cannot resolve a configuration path")?;
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists\n  why:  it may contain settings you wrote\n  fix:  \
             edit it directly, or pass --force to replace it with the defaults",
            path.display()
        );
    }

    Config::default()
        .save(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;

    println!("wrote {}", path.display());
    println!("  edit it, then run `keyvibes doctor` to check the result");
    Ok(())
}

/// The configuration file as the user wrote it, or `None` when there is none.
///
/// Kept separate from [`Config::load`] so `config show` can tell a key the
/// user set from one that merely has its default value.
fn read_config_table(path: &Path) -> Result<Option<toml::Table>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let table = text
        .parse::<toml::Table>()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    Ok(Some(table))
}

/// `Some`/`None` rendered the way the configuration file spells it.
fn display_option(value: &Option<String>) -> String {
    match value {
        Some(value) => value.clone(),
        None => "unset".to_string(),
    }
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
