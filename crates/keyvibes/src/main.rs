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
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::Duration;

mod accept;
mod acceptance_test;
mod alloc_probe;
mod audio_cmd;
mod audio_recovery_test;
mod bench;
mod cli;
mod cli_test;
mod compat_test;
mod config;
mod config_test;
mod control;
mod diagnose;
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
mod soundpack_acceptance_test;
mod stress;

use cli::{Cli, Command, ConfigAction, CtlAction, OnOff, PackAction};
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

/// Restores the default disposition of `SIGPIPE`.
///
/// Rust ignores the signal at startup, so writing to a closed pipe - `keyvibes
/// pack list | head`, say - surfaces as `EPIPE`, which `println!` turns into a
/// panic. Every other CLI lets the short write end the process instead.
///
/// # Safety
///
/// Installs `SIG_DFL`, which takes no argument. Called before any thread or
/// signal handler exists, so there is nothing to disturb.
fn reset_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() -> Result<()> {
    // `| head` and friends close the pipe; let that end the process quietly
    // instead of panicking on EPIPE.
    reset_sigpipe();

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
        Some(Command::Ctl { action, socket }) => run_ctl(action, socket, cli.verbose),
        Some(Command::AcceptanceTest) => {
            let (_path, config) = load_config()?;
            acceptance_test::run(acceptance_test::AcceptanceOptions {
                pack: Some(select_pack(cli.sound_pack.as_deref(), &config)?),
                rate: 48_000,
                settings: config.settings(),
                output_device: config.output_device.clone(),
            })
        }
        Some(Command::SecurityTest) => security_test::run(),
        Some(Command::SoundpackAcceptanceTest) => soundpack_acceptance_test::run(),
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
    if !meta.category.is_empty() {
        println!("  category:    {}", meta.category);
    }
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
    if header.release_table_size > 0 {
        println!(
            "  release table offset {:>6} size {:>6} ({} keys)",
            header.release_table_offset,
            header.release_table_size,
            pack.release_key_count()
        );
    }
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

    let missing = pack.missing_keys();
    println!(
        "  coverage:    {}/{} physical keys",
        PhysicalKey::COUNT - missing.len(),
        PhysicalKey::COUNT
    );
    if pack.has_release_sounds() {
        println!(
            "  release:     {} of {} keys carry a release clip",
            pack.release_key_count(),
            PhysicalKey::COUNT
        );
    }
    if !missing.is_empty() {
        let shown: Vec<String> = missing.iter().take(12).map(ToString::to_string).collect();
        let more = if missing.len() > shown.len() {
            format!(" (+{} more)", missing.len() - shown.len())
        } else {
            String::new()
        };
        println!(
            "               no sound for {}{} - those presses are silent, never \
             substituted with another key's clip",
            shown.join(", "),
            more
        );
    }

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

/// `keyvibes ctl <action>`: one request to the engine over its local socket.
fn run_ctl(action: CtlAction, socket: Option<PathBuf>, verbose: bool) -> Result<()> {
    use kv_control::Request;

    let (request, watch) = match &action {
        CtlAction::Ping => (Request::Ping, false),
        CtlAction::Status => (Request::GetState, false),
        CtlAction::On => (Request::SetEnabled { enabled: true }, false),
        CtlAction::Off => (Request::SetEnabled { enabled: false }, false),
        CtlAction::Volume { value } => (Request::SetVolume { volume: *value }, false),
        CtlAction::Variation {
            pitch,
            no_pitch,
            gain,
            no_gain,
        } => (
            Request::SetVariation {
                pitch: if *pitch {
                    Some(true)
                } else if *no_pitch {
                    Some(false)
                } else {
                    None
                },
                gain: if *gain {
                    Some(true)
                } else if *no_gain {
                    Some(false)
                } else {
                    None
                },
            },
            false,
        ),
        CtlAction::Spatial { state } => (
            Request::SetSpatial {
                enabled: matches!(state, OnOff::On),
            },
            false,
        ),
        CtlAction::Release { state } => (
            Request::SetReleaseSounds {
                enabled: matches!(state, OnOff::On),
            },
            false,
        ),
        CtlAction::Pack { selector } => (
            Request::SetPack {
                selector: selector.clone(),
            },
            false,
        ),
        CtlAction::Preview { pack, keys } => (
            Request::Preview {
                pack: pack.clone(),
                keys: keys.clone(),
            },
            false,
        ),
        CtlAction::Watch => (Request::Subscribe, true),
        CtlAction::Shutdown => (Request::Shutdown, false),
    };

    if watch {
        control::ctl_watch(socket)?;
        return Ok(());
    }

    let response = control::ctl_once(request, socket)?;
    if !response.ok {
        anyhow::bail!(
            "engine rejected the request: {}",
            response
                .error
                .unwrap_or_else(|| "unknown error".to_string())
        );
    }
    if verbose {
        if let Some(state) = &response.state {
            println!("{}", serde_json::to_string_pretty(state)?);
        }
    } else {
        // Compact single-line JSON: parseable, and short enough for a prompt.
        if let Some(state) = &response.state {
            println!("{}", serde_json::to_string(state)?);
        } else {
            println!("{{\"ok\":true}}");
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
    let missing = runtime
        .pack()
        .map(|loaded| loaded.missing_keys())
        .unwrap_or_default();
    if missing.is_empty() {
        println!(
            "  coverage: {}/{} physical keys",
            PhysicalKey::COUNT,
            PhysicalKey::COUNT
        );
    } else {
        let shown: Vec<String> = missing.iter().take(8).map(ToString::to_string).collect();
        let more = if missing.len() > shown.len() {
            format!(" (+{} more)", missing.len() - shown.len())
        } else {
            String::new()
        };
        println!(
            "  coverage: {}/{} physical keys - no sound for {}{}",
            PhysicalKey::COUNT - missing.len(),
            PhysicalKey::COUNT,
            shown.join(", "),
            more
        );
    }
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

    // Control plane: a per-user unix socket the desktop UI, `keyvibes ctl`,
    // and the acceptance tests talk to. A second engine refuses to bind -
    // duplicate instances are an error, not two sockets fighting over one
    // keyboard.
    let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<control::EngineJob>();
    let shutdown = Arc::new(AtomicBool::new(false));
    install_termination_handlers(&shutdown);
    let shared = Arc::new(control::ControlShared::new(
        &runtime,
        config.clone(),
        simulate,
        config_path.clone(),
        jobs_tx,
        shutdown.clone(),
    ));
    let server_handle =
        match control::spawn(shared.clone(), None).context("failed to start the control server")? {
            Some(handle) => Some(handle),
            None => anyhow::bail!(
                "another KeyVibes engine is already running (its control socket is busy)\n\
             \x20 stop it first, or run `keyvibes ctl shutdown` to ask it to exit"
            ),
        };
    println!(
        "Control socket: {}",
        kv_control::default_socket_path().display()
    );
    println!("  drive it with `keyvibes ctl <on|off|volume|pack|...>` or the KeyVibes UI");

    if duration > 0 {
        println!("Playing for {duration}s - press keys to trigger sounds.");
        // A watchdog sets the shared flag; the main loop notices within 250ms.
        let timer_shutdown = shutdown.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(duration));
            timer_shutdown.store(true, Ordering::Relaxed);
        });
    } else {
        println!("Playing - press keys to trigger sounds, Ctrl+C to stop.");
    }

    control::run_main_loop(&mut runtime, &shared, &jobs_rx, &config_path, verbose)
        .context("control loop terminated")?;

    // The control server polls `shutdown` every 50ms; flag it and join so
    // its Drop removes the socket before this process exits. Without the
    // join, the file would linger until the next start treated it as stale.
    shutdown.store(true, Ordering::Relaxed);
    if let Some(handle) = server_handle {
        let _ = handle.join();
    }

    if duration > 0 {
        let input = runtime.input_stats();
        let audio = runtime.audio_stats();
        println!(
            "Done: {} presses, {} commands queued, {} frame(s) rendered, peak {:.3}",
            input.key_presses, input.commands_generated, audio.frames_rendered, audio.peak
        );
        if input.missing_sounds > 0 {
            println!(
                "  {} press(es) had no clip in the selected pack and stayed silent \
                 (never substituted)",
                input.missing_sounds
            );
        }
        if audio.frames_rendered == 0 {
            anyhow::bail!("audio never rendered a frame");
        }
        if audio.peak <= 0.0 {
            anyhow::bail!("audio rendered only silence");
        }
    }
    Ok(())
}

/// Turns SIGINT/SIGTERM into a flag the control loop polls.
///
/// The default disposition would kill the process without dropping the
/// control server, leaving its socket file behind for the next start to
/// clean up as "stale" - correct, but the engine would also skip its clean
/// audio shutdown. One process-wide handler, no allocation, no I/O.
fn install_termination_handlers(flag: &Arc<AtomicBool>) {
    static FLAG_PTR: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());
    FLAG_PTR.store(Arc::as_ptr(flag) as *mut AtomicBool, Ordering::Release);

    extern "C" fn handle(_sig: libc::c_int) {
        let ptr = FLAG_PTR.load(Ordering::Acquire);
        if !ptr.is_null() {
            // SAFETY: the pointer is set once before any handler can run and
            // never cleared; the Arc behind it outlives the process.
            unsafe { (*ptr).store(true, Ordering::SeqCst) };
        }
    }

    unsafe {
        libc::signal(libc::SIGINT, handle as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle as *const () as libc::sighandler_t);
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
    let found = pack_locate::discover_unique();

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

    struct Row {
        name: String,
        keys: u32,
        clips: u32,
        rate: u32,
        path: String,
        error: Option<String>,
        coverage: Option<u32>,
        category: String,
        release_keys: u32,
    }

    let mut rows: Vec<Row> = Vec::new();
    for path in &found {
        match KvPack::open(path) {
            Ok(loaded) => {
                let stats = loaded.stats();
                width = width.max(stats.name.len());
                // `-v` exists to answer "why is that key silent?", so it shows
                // coverage - the count a user can check against 104.
                let coverage = if verbose {
                    Some((PhysicalKey::COUNT - loaded.missing_keys().len()) as u32)
                } else {
                    None
                };
                rows.push(Row {
                    name: stats.name,
                    keys: stats.keys,
                    clips: stats.clips,
                    rate: stats.sample_rate,
                    path: path.display().to_string(),
                    error: None,
                    coverage,
                    category: loaded.metadata().category.clone(),
                    release_keys: stats.release_keys,
                });
            }
            Err(error) => {
                width = width.max(4);
                rows.push(Row {
                    name: path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string(),
                    keys: 0,
                    clips: 0,
                    rate: 0,
                    path: path.display().to_string(),
                    error: Some(error.to_string()),
                    coverage: None,
                    category: String::new(),
                    release_keys: 0,
                });
            }
        }
    }
    for row in rows {
        match row.error {
            Some(error) => println!("  {:<width$}  INVALID: {error}", row.name),
            None => {
                let category = if row.category.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", row.category)
                };
                match row.coverage {
                    Some(covered) => println!(
                        "  {:<width$}{category}  {keys:>2} keys  {clips:>3} clips  \
                     coverage {covered}/{total}  release={release}  {rate:>5} Hz  {path}",
                        row.name,
                        keys = row.keys,
                        clips = row.clips,
                        total = PhysicalKey::COUNT,
                        release = row.release_keys,
                        rate = row.rate,
                        path = row.path,
                        width = width
                    ),
                    None => {
                        println!(
                            "  {:<width$}{category}  {keys:>2} keys  {clips:>3} clips  \
                         release={release}  {rate:>5} Hz  {path}",
                            row.name,
                            keys = row.keys,
                            clips = row.clips,
                            release = row.release_keys,
                            rate = row.rate,
                            path = row.path,
                            width = width
                        )
                    }
                }
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

    let backend = LinuxInputBackend::new(
        queue,
        stats.clone(),
        probe,
        None,
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    )
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

#[cfg(test)]
mod production_pack_test;
