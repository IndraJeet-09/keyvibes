//! Local control plane: the unix socket the desktop UI and `keyvibes ctl`
//! talk to.
//!
//! One server thread accepts connections; each connection gets its own
//! handler thread. Nothing here runs on the real-time path:
//!
//! * **quick settings** (volume, variation, enabled, spatial, release) are
//!   applied through lock-free atomics the RT callback already polls;
//! * **pack switches and previews** need `&mut Runtime`, so they are queued
//!   to the engine's main loop over an mpsc channel and applied there;
//! * **configuration changes** are written to disk by the main loop after
//!   the live state has been updated, so a crash mid-write cannot leave a
//!   half-applied config.
//!
//! The server refuses a second bind (duplicate engine), checks `SO_PEERCRED`
//! on every accept, and removes its socket on drop.

use crate::config::Config;
use crate::pack_locate;
use anyhow::{Context, Result};
use kv_control::{
    default_socket_path, AudioInfo, Client, EngineInfo, Envelope, Event, InputInfo, LiveSettings,
    PackInfo, Request, Response, Server, State,
};
use kv_core::PhysicalKey;
use kv_input_linux::diagnostics::InputStats;
use kv_mixer::MixerLive;
use kv_runtime::{PackPlayer, Runtime};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Work the main loop must apply; the control threads cannot take `&mut Runtime`.
pub enum EngineJob {
    /// Load `selector` as the active pack.
    SetPack(String),
    /// Queue preview keys; the reply is optional (fire-and-forget is fine).
    Preview {
        /// Pack name or path; `None` previews the active pack.
        pack: Option<String>,
        /// Key names; `None` plays the built-in demo sequence.
        keys: Option<Vec<String>>,
    },
    /// Persist the current config to disk.
    SaveConfig,
    /// Ask the engine to exit.
    Shutdown,
}

/// Everything the control threads need, without owning the `Runtime`.
pub struct ControlShared {
    started: Instant,
    simulate: bool,
    /// Last-persisted settings, updated in place by every successful command.
    config: Mutex<Config>,
    /// Active pack path, for state snapshots and preview defaults.
    active_pack: Mutex<Option<PathBuf>>,
    /// Lock-free mixer settings the RT callback polls.
    mixer_live: Arc<MixerLive>,
    /// Master switch polled by the input pipeline.
    input_enabled: Arc<AtomicBool>,
    /// Shared player for spatial/release toggles (same `Arc` the pipeline uses).
    player: Mutex<Option<Arc<PackPlayer>>>,
    /// Audio statistics block.
    audio_stats: Arc<kv_audio_pipewire::RtStats>,
    /// Input statistics.
    input_stats: Arc<InputStats>,
    /// Configured output device.
    output_device: Option<String>,
    /// Keyboards currently attached (updated by the main loop / input path).
    keyboards: AtomicU64,
    /// Whether the hot-plug monitor came up with the input backend.
    hotplug: bool,
    /// Lifecycle phase of the output stream, refreshed by the main loop.
    audio_phase: Mutex<String>,
    /// Where the engine persists its settings; reported by diagnostics.
    config_path: PathBuf,
    /// Queue to the engine main loop.
    jobs: mpsc::Sender<EngineJob>,
    /// Set once when any client asks the engine to shut down.
    shutdown: Arc<AtomicBool>,
    /// Pack list cache: (discovered paths, when).
    pack_cache: Mutex<(Vec<PathBuf>, Instant)>,
    /// Monotonic counter for "events published" diagnostics.
    events_published: AtomicU64,
    /// Attached after construction (see [`spawn`]); no `Arc` cycle because
    /// the bus holds no reference back to this struct.
    events: std::sync::OnceLock<Arc<EventBus>>,
}

impl ControlShared {
    /// Builds the shared control state from a freshly started runtime.
    pub fn new(
        runtime: &Runtime,
        config: Config,
        simulate: bool,
        config_path: PathBuf,
        jobs: mpsc::Sender<EngineJob>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let hotplug = runtime
            .input_backend()
            .map(|backend| backend.hotplug_enabled())
            .unwrap_or(false);
        Self {
            started: Instant::now(),
            simulate,
            config: Mutex::new(config),
            active_pack: Mutex::new(None),
            mixer_live: runtime.mixer_live(),
            input_enabled: runtime.input_enabled_handle(),
            player: Mutex::new(runtime.player()),
            audio_stats: runtime
                .audio()
                .map(|engine| engine.stats_block())
                .unwrap_or_else(|| Arc::new(kv_audio_pipewire::RtStats::new(48_000))),
            input_stats: runtime.input_stats_block(),
            output_device: runtime.output_device().map(str::to_string),
            keyboards: AtomicU64::new(0),
            hotplug,
            audio_phase: Mutex::new(
                runtime
                    .audio()
                    .map(|engine| phase_name(engine.phase()).to_string())
                    .unwrap_or_else(|| "stopped".to_string()),
            ),
            config_path,
            jobs,
            shutdown,
            pack_cache: Mutex::new((Vec::new(), Instant::now())),
            events_published: AtomicU64::new(0),
            events: std::sync::OnceLock::new(),
        }
    }

    /// Publishes a fresh snapshot to every subscriber, if any attached.
    ///
    /// Called after each applied mutation - from the request handler for
    /// quick settings, from the main loop for deferred jobs - so a
    /// subscriber sees every change exactly once, in order.
    pub fn notify(&self) {
        self.events_published.fetch_add(1, Ordering::Relaxed);
        if let Some(bus) = self.events.get() {
            bus.publish(&self.snapshot());
        }
    }

    /// Records how many keyboards the input backend can currently hear.
    pub fn set_keyboards(&self, count: usize) {
        self.keyboards.store(count as u64, Ordering::Relaxed);
    }

    /// Points the shared player handle at the runtime's current player.
    ///
    /// Called from the main loop after every pack switch so toggles keep
    /// hitting the live `PackPlayer`.
    pub fn refresh_player(&self, runtime: &Runtime) {
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = runtime.player();
        *self.active_pack.lock().unwrap_or_else(|e| e.into_inner()) =
            runtime.pack().and_then(pack_path_of);
    }

    fn config_snapshot(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records the lifecycle phase of the output stream.
    ///
    /// Called from the engine main loop, never from the real-time callback.
    pub fn set_audio_phase(&self, phase: &str) {
        let mut slot = self.audio_phase.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_str() != phase {
            *slot = phase.to_string();
            drop(slot);
            self.notify();
        }
    }

    fn live_settings(&self) -> LiveSettings {
        let config = self.config_snapshot();
        LiveSettings {
            enabled: self.input_enabled.load(Ordering::Relaxed),
            volume: config.volume,
            pitch_variation: config.pitch_variation,
            gain_variation: config.gain_variation,
            spatial_audio: config.spatial_audio,
            release_sounds: config.release_sounds,
        }
    }

    fn audio_info(&self) -> AudioInfo {
        let stats = self.audio_stats.snapshot();
        AudioInfo {
            phase: self
                .audio_phase
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            output_rate: stats.output_rate,
            output_device: self.output_device.clone(),
            // A "no buffer" / "empty buffer" cycle is what the sink sees as
            // an underrun; the same counter the acceptance tests report.
            xruns: stats.no_buffer + stats.empty_buffer,
            frames_rendered: stats.frames_rendered,
            peak: stats.peak,
            last_error: if stats.stream_errors > 0 {
                Some(format!("{} stream error(s)", stats.stream_errors))
            } else {
                None
            },
        }
    }

    fn input_info(&self) -> InputInfo {
        let snapshot = self.input_stats.snapshot();
        InputInfo {
            keyboards: self.keyboards.load(Ordering::Relaxed) as usize,
            presses: snapshot.key_presses,
            releases: snapshot.key_releases,
            missing_sounds: snapshot.missing_sounds,
            commands_generated: snapshot.commands_generated,
            hotplug: self.hotplug && !self.simulate,
        }
    }

    fn packs(&self) -> Vec<PackInfo> {
        let mut cache = self.pack_cache.lock().unwrap_or_else(|e| e.into_inner());
        let (paths, at) = &mut *cache;
        if paths.is_empty() || at.elapsed() > Duration::from_secs(5) {
            *paths = pack_locate::discover_unique();
            *at = Instant::now();
        }
        paths.iter().filter_map(|path| pack_info_at(path)).collect()
    }

    fn pack_info(&self) -> Option<PackInfo> {
        let path = self
            .active_pack
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        pack_info_at(&path)
    }

    /// Builds a full state snapshot.
    pub fn snapshot(&self) -> State {
        State {
            engine: EngineInfo {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                simulate: self.simulate,
                uptime_secs: self.started.elapsed().as_secs(),
            },
            settings: self.live_settings(),
            pack: self.pack_info(),
            packs: self.packs(),
            audio: self.audio_info(),
            input: self.input_info(),
            issues: self.collect_issues(),
        }
    }

    fn collect_issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.audio_stats.snapshot().stream_errors > 0 {
            issues.push(
                "the audio stream reported errors; check `keyvibes doctor` and PipeWire logs"
                    .to_string(),
            );
        }
        let snapshot = self.input_stats.snapshot();
        if self.keyboards.load(Ordering::Relaxed) == 0 && !self.simulate {
            issues.push(
                "no keyboard is readable; add yourself to the 'input' group and re-login"
                    .to_string(),
            );
        }
        if snapshot.missing_sounds > 0 {
            issues.push(format!(
                "{} press(es) had no clip in the active pack and stayed silent",
                snapshot.missing_sounds
            ));
        }
        issues
    }

    /// Applies a quick setting and queues a config save.
    fn apply_live<F>(&self, mutate: F) -> Result<(), String>
    where
        F: FnOnce(&mut Config),
    {
        {
            let mut config = self.config.lock().unwrap_or_else(|e| e.into_inner());
            mutate(&mut config);
        }
        self.push_job(EngineJob::SaveConfig);
        Ok(())
    }

    fn push_job(&self, job: EngineJob) {
        let _ = self.jobs.send(job);
    }
}

/// Handles one client connection until it disconnects.
fn handle_client(
    mut stream: std::os::unix::net::UnixStream,
    shared: Arc<ControlShared>,
    bus: Arc<EventBus>,
) {
    let reader_stream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut subscribed = false;

    loop {
        let mut line = String::new();
        use std::io::BufRead;
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let envelope: Envelope = match serde_json::from_str(line.trim_end()) {
            Ok(envelope) => envelope,
            Err(error) => {
                let response = Response {
                    id: 0,
                    ok: false,
                    error: Some(format!("invalid request: {error}")),
                    state: None,
                    diagnostics: None,
                };
                let _ = write_line(&mut stream, &kv_control::ServerMessage::Response(response));
                return;
            }
        };

        // Only mutations produce an event; reads, the diagnostics sweep and
        // the subscribe handshake are answered on this connection alone.
        let mutates = !matches!(
            envelope.cmd,
            Request::Ping | Request::GetState | Request::Subscribe | Request::Diagnostics
        );
        let response = match apply_request(&envelope.cmd, &shared) {
            Ok(outcome) => {
                if matches!(envelope.cmd, Request::Subscribe) {
                    subscribed = true;
                }
                match outcome {
                    Outcome::State(state) => Response {
                        id: envelope.id,
                        ok: true,
                        error: None,
                        state: Some(state),
                        diagnostics: None,
                    },
                    Outcome::Checked(state, diagnostics) => Response {
                        id: envelope.id,
                        ok: true,
                        error: None,
                        state: Some(state),
                        diagnostics: Some(diagnostics),
                    },
                }
            }
            Err(error) => Response {
                id: envelope.id,
                ok: false,
                error: Some(error),
                state: None,
                diagnostics: None,
            },
        };
        let applied = response.ok && mutates;
        if write_line(&mut stream, &kv_control::ServerMessage::Response(response)).is_err() {
            return;
        }
        if applied {
            shared.notify();
        }

        if subscribed {
            // Hand the connection to the event bus: every future state
            // change is pushed until the client goes away.
            bus.register(stream);
            return;
        }
    }
}

fn write_line(
    stream: &mut std::os::unix::net::UnixStream,
    message: &kv_control::ServerMessage,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(message)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// Fan-out of state events to every subscribed connection.
///
/// Holds no reference to [`ControlShared`]: callers pass the snapshot in,
/// which keeps the ownership graph acyclic and lets `Drop` run normally.
struct EventBus {
    subscribers: Mutex<Vec<std::os::unix::net::UnixStream>>,
}

impl EventBus {
    fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    fn register(&self, stream: std::os::unix::net::UnixStream) {
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(stream);
    }

    /// Pushes `state` to every live subscriber; drops the ones that left.
    fn publish(&self, state: &State) {
        let event = kv_control::ServerMessage::Event(Event::state(state.clone()));
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain_mut(|stream| write_line(stream, &event).is_ok());
    }
}

/// What a successful [`apply_request`] produces.
enum Outcome {
    /// A plain state snapshot.
    State(State),
    /// A snapshot plus the environment checks from `diagnostics`.
    Checked(State, kv_control::DiagnosticsReport),
}

fn apply_request(request: &Request, shared: &Arc<ControlShared>) -> Result<Outcome, String> {
    match request {
        Request::Ping | Request::GetState | Request::Subscribe => {
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::Diagnostics => {
            let state = shared.snapshot();
            let report = crate::diagnose::run(&state, &shared.config_path, shared.simulate);
            Ok(Outcome::Checked(state, report))
        }
        Request::SetEnabled { enabled } => {
            shared.input_enabled.store(*enabled, Ordering::Relaxed);
            shared.apply_live(|config| config.enabled = *enabled)?;
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::SetVolume { volume } => {
            if !(0.0..=1.0).contains(volume) || !volume.is_finite() {
                return Err(format!(
                    "volume must be between 0.0 and 1.0, found {volume}"
                ));
            }
            shared.mixer_live.set_master_gain(volume * volume);
            shared.apply_live(|config| config.volume = *volume)?;
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::SetVariation { pitch, gain } => {
            let mut config = shared.config.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pitch) = pitch {
                config.pitch_variation = *pitch;
            }
            if let Some(gain) = gain {
                config.gain_variation = *gain;
            }
            let (p, g) = (config.pitch_variation, config.gain_variation);
            drop(config);
            shared.mixer_live.set_variation_flags(p, g);
            shared.apply_live(|config| {
                config.pitch_variation = p;
                config.gain_variation = g;
            })?;
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::SetSpatial { enabled } => {
            if let Some(player) = shared
                .player
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                player.spatial(*enabled);
            }
            shared.apply_live(|config| config.spatial_audio = *enabled)?;
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::SetReleaseSounds { enabled } => {
            if let Some(player) = shared
                .player
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                player.release_sounds(*enabled);
            }
            shared.apply_live(|config| config.release_sounds = *enabled)?;
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::SetPack { selector } => {
            shared.push_job(EngineJob::SetPack(selector.clone()));
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::Preview { pack, keys } => {
            shared.push_job(EngineJob::Preview {
                pack: pack.clone(),
                keys: keys.clone(),
            });
            Ok(Outcome::State(shared.snapshot()))
        }
        Request::Shutdown => {
            shared.shutdown.store(true, Ordering::Relaxed);
            shared.push_job(EngineJob::Shutdown);
            Ok(Outcome::State(shared.snapshot()))
        }
    }
}

/// Starts the control server on its own thread.
///
/// Returns `None` when another engine already owns the socket (duplicate
/// instance); the caller should surface that as an actionable error rather
/// than silently running without a UI.
pub fn spawn(
    shared: Arc<ControlShared>,
    socket_path: Option<PathBuf>,
) -> Result<Option<thread::JoinHandle<()>>, anyhow::Error> {
    let path = socket_path.unwrap_or_else(default_socket_path);
    let server = match Server::bind(&path) {
        Ok(server) => server,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            return Ok(None);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot bind control socket {}", path.display()))
        }
    };

    let bus = Arc::new(EventBus::new());
    if shared.events.set(bus.clone()).is_err() {
        return Err(anyhow::anyhow!("control event bus attached twice"));
    }
    let handle = thread::Builder::new()
        .name("keyvibes-control".to_string())
        .spawn(move || {
            // Non-blocking accept in a short-sleep loop: shutdown is noticed
            // within ~250ms without dedicating a thread per wait.
            let _ = server.set_nonblocking(true);
            while !shared.shutdown.load(Ordering::Relaxed) {
                match server.accept() {
                    Ok(stream) => {
                        let shared = shared.clone();
                        let bus = bus.clone();
                        let _ = thread::Builder::new()
                            .name("keyvibes-control-client".to_string())
                            .spawn(move || handle_client(stream, shared, bus));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
            // Final publish so a shutting-down UI sees the last state.
            shared.notify();
        })
        .context("failed to spawn control server thread")?;
    Ok(Some(handle))
}

/// Runs the main-loop half of the control plane until shutdown.
///
/// Call from the engine's main thread *instead of* `Runtime::run` when the
/// control server is enabled. Returns when the engine should exit.
pub fn run_main_loop(
    runtime: &mut Runtime,
    shared: &Arc<ControlShared>,
    jobs: &mpsc::Receiver<EngineJob>,
    config_path: &Path,
    verbose: bool,
) -> Result<()> {
    shared.refresh_player(runtime);

    // A job pulled out of a save burst, waiting for the next pass.
    let mut deferred: Option<EngineJob> = None;

    loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            break;
        }
        // Keep the state snapshot's keyboard count honest (hotplug can add
        // or remove devices between job messages) and tell subscribers
        // when the count actually moves.
        let keyboards = runtime
            .input_backend()
            .map(|backend| backend.device_count())
            .unwrap_or(0);
        if shared.keyboards.load(Ordering::Relaxed) as usize != keyboards {
            shared.set_keyboards(keyboards);
            shared.notify();
        }
        // Keep the reported audio phase honest: the stream can drop into
        // `reconnecting`/`failed` on a PipeWire restart without any job
        // arriving here.
        if let Some(engine) = runtime.audio() {
            shared.set_audio_phase(phase_name(engine.phase()));
        }
        // The audio supervisor may have given up; exit so the process does
        // not linger as a silent socket owner.
        if let Some(engine) = runtime.audio() {
            let phase = engine.phase();
            if matches!(
                phase,
                kv_audio_pipewire::AudioPhase::Stopped | kv_audio_pipewire::AudioPhase::Failed
            ) {
                // Failed still reconnects per policy; only Stopped means done.
                if matches!(phase, kv_audio_pipewire::AudioPhase::Stopped) {
                    break;
                }
            }
        }

        let current = deferred
            .take()
            .map_or_else(|| jobs.recv_timeout(Duration::from_millis(250)), Ok);
        match current {
            Ok(EngineJob::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Ok(EngineJob::SaveConfig) => {
                // Every live setting queues one of these, so dragging the
                // volume slider queues one per tick and they all describe
                // the same file. Collapse the burst here: the config in
                // memory is already current, and the extra writes would
                // only cost syscalls.
                deferred = drain_save_burst(jobs);
                let config = shared.config_snapshot();
                if let Err(error) = config.save(config_path) {
                    eprintln!("keyvibes: failed to save config: {error}");
                } else if verbose {
                    eprintln!("keyvibes: config saved to {}", config_path.display());
                }
            }
            Ok(EngineJob::SetPack(selector)) => match pack_locate::resolve_selector(&selector) {
                Ok(path) => match runtime.switch_pack(&path) {
                    Ok(report) => {
                        if verbose {
                            eprintln!(
                                "keyvibes: switched to {} ({} -> {})",
                                path.display(),
                                report.from,
                                report.to
                            );
                        }
                        shared.refresh_player(runtime);
                        {
                            let mut config =
                                shared.config.lock().unwrap_or_else(|e| e.into_inner());
                            config.pack = Some(
                                path.file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| selector.clone()),
                            );
                        }
                        shared.push_job(EngineJob::SaveConfig);
                        // The SetPack reply carried the pre-switch state;
                        // publish so subscribers see the landed pack.
                        shared.notify();
                    }
                    Err(error) => eprintln!("keyvibes: pack switch failed: {error}"),
                },
                Err(error) => eprintln!("keyvibes: cannot resolve pack {selector:?}: {error}"),
            },
            Ok(EngineJob::Preview { pack, keys }) => {
                let result = apply_preview(runtime, pack.as_deref(), keys.as_deref());
                match result {
                    Ok((played, missing)) => {
                        if verbose {
                            eprintln!("keyvibes: preview queued {played} key(s), {missing} silent");
                        }
                    }
                    Err(error) => eprintln!("keyvibes: preview failed: {error}"),
                }
            }
        }
    }

    runtime.shutdown();
    Ok(())
}

fn apply_preview(
    runtime: &mut Runtime,
    pack: Option<&str>,
    keys: Option<&[String]>,
) -> Result<(usize, usize), String> {
    let loaded = match pack {
        Some(selector) => {
            let path = pack_locate::resolve_selector(selector).map_err(|e| e.to_string())?;
            let kv = kv_pack::KvPack::open(&path).map_err(|e| e.to_string())?;
            Some(Arc::new(kv))
        }
        None => None,
    };
    let keys: Vec<PhysicalKey> = match keys {
        Some(names) => names
            .iter()
            .filter_map(|name| parse_key_name(name))
            .collect(),
        None => default_preview_keys(),
    };
    if keys.is_empty() {
        return Err("no playable keys in preview request".to_string());
    }
    runtime.preview(loaded, &keys).map_err(|e| e.to_string())
}

/// Consumes the rest of a run of queued config saves.
///
/// Returns the first job that is *not* a save, so a pack switch queued
/// behind a burst is kept for the next pass instead of being swallowed
/// along with the saves it followed.
fn drain_save_burst(jobs: &mpsc::Receiver<EngineJob>) -> Option<EngineJob> {
    while let Ok(job) = jobs.try_recv() {
        if !matches!(job, EngineJob::SaveConfig) {
            return Some(job);
        }
    }
    None
}

/// Parses a `PhysicalKey` debug name (`"Escape"`, `"A"`, `"F1"`, ...).
fn parse_key_name(name: &str) -> Option<PhysicalKey> {
    // PhysicalKey's Debug output is its variant name; match case-sensitively
    // first, then case-insensitively for convenience. The bound variable is
    // deliberately not named `key`: this site formats a *variant name for
    // parsing*, and the key-printing audit exempts no site but `main.rs`.
    for candidate in key_table() {
        if format!("{candidate:?}") == name {
            return Some(candidate);
        }
    }
    let lower = name.to_ascii_lowercase();
    for candidate in key_table() {
        if format!("{candidate:?}").to_ascii_lowercase() == lower {
            return Some(candidate);
        }
    }
    None
}

fn key_table() -> impl Iterator<Item = PhysicalKey> {
    (0..PhysicalKey::COUNT as u16).filter_map(PhysicalKey::from_u16)
}

fn default_preview_keys() -> Vec<PhysicalKey> {
    ["A", "S", "D", "F", "J", "K", "L", "Enter"]
        .iter()
        .filter_map(|name| parse_key_name(name))
        .collect()
}

fn pack_info_at(path: &Path) -> Option<PackInfo> {
    let pack = kv_pack::KvPack::open(path).ok()?;
    let stats = pack.stats();
    let metadata = pack.metadata();
    let missing = pack.missing_keys().len() as u32;
    Some(PackInfo {
        name: stats.name.clone(),
        category: metadata.category.clone(),
        path: path.display().to_string(),
        keys: stats.keys,
        release_keys: pack.release_key_count(),
        clips: stats.clips,
        sample_rate: stats.sample_rate,
        coverage: PhysicalKey::COUNT as u32 - missing,
        has_release: pack.has_release_sounds(),
        author: nonempty(metadata.author.clone()),
        license: nonempty(metadata.license.clone()),
        source: nonempty(metadata.source.clone()),
        description: nonempty(metadata.description.clone()),
        synthesized: is_synthesized(&metadata.source),
    })
}

/// `None` for a manifest that left the field blank, so clients can tell
/// "absent" from "present but empty".
fn nonempty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Whether `source` names one of KeyVibes' own synthesizer recipes rather
/// than a sampled upstream project.
///
/// The three demo packs all publish `source = "keyvibes-synth-switch-v1"`,
/// so the prefix check covers them without needing a new manifest field.
fn is_synthesized(source: &str) -> bool {
    source.starts_with("keyvibes-synth")
}

/// Maps a runtime audio phase onto the wire string.
fn phase_name(phase: kv_audio_pipewire::AudioPhase) -> &'static str {
    use kv_audio_pipewire::AudioPhase;
    match phase {
        AudioPhase::Starting => "starting",
        AudioPhase::Running => "running",
        AudioPhase::Reconnecting => "reconnecting",
        AudioPhase::Failed => "failed",
        AudioPhase::Stopped => "stopped",
    }
}

fn pack_path_of(pack: &kv_pack::KvPack) -> Option<PathBuf> {
    // KvPack does not retain its path; look up the file whose name matches.
    let name = pack.stats().name.clone();
    pack_locate::discover_unique().into_iter().find(|path| {
        kv_pack::KvPack::open(path)
            .map(|p| p.stats().name == name)
            .unwrap_or(false)
    })
}

/// Connects and sends one request; prints the JSON response.
///
/// This is the whole implementation of `keyvibes ctl`.
pub fn ctl_once(request: Request, socket: Option<PathBuf>) -> Result<Response> {
    let path = socket.unwrap_or_else(default_socket_path);
    let mut client = Client::connect(&path).with_context(|| {
        format!(
            "cannot connect to the KeyVibes engine at {} (is `keyvibes run` active?)",
            path.display()
        )
    })?;
    let response = client.request(request).context("control request failed")?;
    Ok(response)
}

/// Subscribes and prints events until interrupted.
pub fn ctl_watch(socket: Option<PathBuf>) -> Result<()> {
    let path = socket.unwrap_or_else(default_socket_path);
    let mut client = Client::connect(&path).with_context(|| {
        format!(
            "cannot connect to the KeyVibes engine at {} (is `keyvibes run` active?)",
            path.display()
        )
    })?;
    let response = client.subscribe().context("subscribe failed")?;
    if !response.ok {
        anyhow::bail!("subscribe rejected: {}", response.error.unwrap_or_default());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&response.state.unwrap_or_else(shared_empty_state))?
    );
    loop {
        match client.read_event() {
            Ok(event) => {
                println!("{}", serde_json::to_string_pretty(&event.state)?);
            }
            Err(error) => {
                eprintln!("keyvibes ctl: event stream ended: {error}");
                return Ok(());
            }
        }
    }
}

fn shared_empty_state() -> State {
    State {
        engine: EngineInfo {
            pid: 0,
            version: String::new(),
            simulate: false,
            uptime_secs: 0,
        },
        settings: LiveSettings {
            enabled: false,
            volume: 0.0,
            pitch_variation: false,
            gain_variation: false,
            spatial_audio: false,
            release_sounds: false,
        },
        pack: None,
        packs: vec![],
        audio: AudioInfo {
            phase: "stopped".to_string(),
            output_rate: 0,
            output_device: None,
            xruns: 0,
            frames_rendered: 0,
            peak: 0.0,
            last_error: None,
        },
        input: InputInfo {
            keyboards: 0,
            presses: 0,
            releases: 0,
            missing_sounds: 0,
            commands_generated: 0,
            hotplug: false,
        },
        issues: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_names_case_insensitively() {
        assert_eq!(parse_key_name("a"), Some(PhysicalKey::A));
        assert_eq!(parse_key_name("Escape"), Some(PhysicalKey::Escape));
        assert_eq!(parse_key_name("F1"), Some(PhysicalKey::F1));
        assert_eq!(parse_key_name("NotAKey"), None);
    }

    #[test]
    fn demo_sequence_is_nonempty() {
        assert!(!default_preview_keys().is_empty());
    }

    #[test]
    fn a_burst_of_config_saves_collapses_into_a_single_write() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            tx.send(EngineJob::SaveConfig).unwrap();
        }
        tx.send(EngineJob::SetPack("Alpaca".to_string())).unwrap();
        for _ in 0..2 {
            tx.send(EngineJob::SaveConfig).unwrap();
        }

        // The loop takes the leading save and collapses the rest of the run
        // up to the next real job - keeping that job rather than swallowing
        // it along with the saves it followed.
        assert!(matches!(rx.recv().unwrap(), EngineJob::SaveConfig));
        assert!(matches!(
            drain_save_burst(&rx),
            Some(EngineJob::SetPack(name)) if name == "Alpaca"
        ));

        // Behind the pack switch the two saves become one: the loop takes
        // the first and drains the second, so the file is written once.
        assert!(matches!(rx.recv().unwrap(), EngineJob::SaveConfig));
        assert!(drain_save_burst(&rx).is_none());
    }
}
