//! Runtime coordination for KeyVibes.
//!
//! The runtime wires the subsystems together into one production path:
//!
//! ```text
//! evdev reader threads ──SoundSource──▶ SPSC queue ──▶ PipeWire RT callback
//!        (kv-input-linux)                 (kv-ring)         (kv-audio-pipewire)
//!                │                                                  ▲
//!                └──────────── PackPlayer (immutable KvPack) ───────┘
//! ```
//!
//! Everything sample-related is validated and mapped before any thread starts,
//! so the input and audio threads only ever touch immutable data and the
//! lock-free queue.

use crate::player::PackPlayer;
use crate::simulate::{SimInputOptions, SimulatedInput};
use crate::swappable::{SwapReport, SwappableSource};
use kv_audio_pipewire::engine::{AudioEngine, AudioOptions};
use kv_audio_pipewire::idle::{IdleConfig, IdlePhase, IdleState};
use kv_audio_pipewire::lifecycle::ReconnectPolicy;
use kv_audio_pipewire::stream::{AudioError, StreamStats};
use kv_core::{
    monotonic_ns, PhysicalKey, PlayCommand, Settings, SoundSource, StreamWake, VariantState,
};
use kv_input_linux::diagnostics::InputStatsSnapshot;
use kv_input_linux::error::InputError;
use kv_input_linux::LinuxInputBackend;
use kv_mixer::MixerSettings;
use kv_pack::{KvPack, PackError};
use kv_ring::SpscRing;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Errors raised while starting or wiring the runtime.
#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("no pack loaded; call load_pack() first")]
    NoPack,

    #[error(transparent)]
    Pack(#[from] PackError),

    #[error(transparent)]
    Audio(#[from] AudioError),

    #[error(transparent)]
    Input(#[from] InputError),
}

/// What happened when input was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStart {
    /// At least one keyboard is being read.
    Keyboards(usize),
    /// No readable keyboard right now; hotplug will attach one later.
    WaitingForKeyboard,
}

/// Main runtime coordinating all subsystems.
pub struct Runtime {
    engine: Option<AudioEngine>,
    audio_stats: Option<Arc<kv_audio_pipewire::RtStats>>,
    command_queue: Arc<SpscRing<PlayCommand>>,
    output_rate: u32,
    pack: Option<Arc<KvPack>>,
    source: Option<Arc<SwappableSource>>,
    stats: Arc<kv_input_linux::InputStats>,
    input: Option<LinuxInputBackend>,
    simulated: Option<SimulatedInput>,
    settings: Settings,
    reconnect_policy: ReconnectPolicy,
    idle_config: IdleConfig,
    output_device: Option<String>,
    /// Resumes a paused output stream; installed by `start_audio`.
    wake: Option<Arc<dyn StreamWake>>,
    /// Per-key variant rotation used by `trigger_key`.
    ///
    /// Control plane only (`keyvibes idle-test`); the live input pipeline
    /// keeps its own state on its own thread.
    trigger_variants: std::sync::Mutex<[VariantState; PhysicalKey::COUNT]>,
}

impl Runtime {
    /// Creates a runtime targeting the given output sample rate.
    ///
    /// Nothing is opened yet: call [`load_pack`](Self::load_pack),
    /// [`start_audio`](Self::start_audio), and [`start_input`](Self::start_input).
    pub fn new(output_rate: u32) -> Self {
        Self {
            engine: None,
            audio_stats: None,
            command_queue: Arc::new(SpscRing::with_capacity(256)),
            output_rate,
            pack: None,
            source: None,
            stats: Arc::new(kv_input_linux::InputStats::new()),
            input: None,
            simulated: None,
            settings: Settings::default(),
            reconnect_policy: ReconnectPolicy::default(),
            idle_config: IdleConfig::default(),
            output_device: None,
            wake: None,
            trigger_variants: std::sync::Mutex::new(std::array::from_fn(|_| {
                VariantState::default()
            })),
        }
    }

    /// Overrides the idle-management policy (must be set before `start_audio`).
    pub fn set_idle_config(&mut self, config: IdleConfig) {
        self.idle_config = config;
    }

    /// The idle-management policy in force.
    pub fn idle_config(&self) -> IdleConfig {
        self.idle_config
    }

    /// Routes output to a specific PipeWire node (must be set before
    /// `start_audio`).
    ///
    /// `None` leaves routing to the session manager.
    pub fn set_output_device(&mut self, device: Option<String>) {
        self.output_device = device;
    }

    /// The PipeWire node output is routed to, if one was requested.
    pub fn output_device(&self) -> Option<&str> {
        self.output_device.as_deref()
    }

    /// Overrides the audio reconnection policy (must be set before `start_audio`).
    pub fn set_reconnect_policy(&mut self, policy: ReconnectPolicy) {
        self.reconnect_policy = policy;
    }

    /// The active settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replaces the user settings.
    ///
    /// Must be called before [`load_pack`](Self::load_pack) and
    /// [`start_audio`](Self::start_audio): the mixer lives on the real-time
    /// thread and the player is shared with the input threads, so neither can
    /// be reconfigured once started.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// Opens and validates a `.kvpack`, then makes it the playing pack.
    ///
    /// Performs filesystem I/O and full pack validation; must not be called
    /// from a real-time thread.
    ///
    /// When input threads are already running this swaps rather than
    /// replaces: they keep the same [`SwappableSource`] handle and pick the
    /// new pack up on their very next key press.
    pub fn load_pack<P: AsRef<Path>>(&mut self, path: P) -> Result<(), RuntimeError> {
        self.load_pack_inner(path).map(|_| ())
    }

    /// Replaces the playing pack **while the engine keeps running**.
    ///
    /// The input side switches immediately; the previous pack is parked and
    /// released once nothing can still reference it - see
    /// [`retire_packs`](Self::retire_packs). The audio stream is never
    /// stopped, so there is no gap in which presses go unheard.
    pub fn switch_pack<P: AsRef<Path>>(&mut self, path: P) -> Result<SwapReport, RuntimeError> {
        let report = self.load_pack_inner(path)?;
        self.retire_packs();
        Ok(report)
    }

    /// How many packs are parked waiting to be released.
    pub fn retired_packs(&self) -> usize {
        self.source.as_ref().map(|s| s.retired()).unwrap_or(0)
    }

    /// Names of the packs parked for release, oldest first.
    pub fn retired_pack_names(&self) -> Vec<String> {
        self.source
            .as_ref()
            .map(|s| s.retired_names())
            .unwrap_or_default()
    }

    /// Releases parked packs if - and only if - nothing can reference them.
    ///
    /// Safe to call at any time from the control plane; it returns how many
    /// packs were released and simply does nothing when the stream is still
    /// using them. Call it repeatedly (after a switch, while idle, at
    /// shutdown) rather than waiting for a specific moment.
    pub fn retire_packs(&self) -> usize {
        let Some(source) = self.source.as_ref() else {
            return 0;
        };
        if source.retired() == 0 {
            return 0;
        }

        let stats = self.audio_stats();
        let producers_clear = source.producers() == 0;
        let queue_clear = self.command_queue.is_empty();
        // The one window in which a command has left the queue but has not
        // become a voice yet: only observable while the data thread is in
        // its callback.
        let not_mid_callback = !stats.in_callback;

        if !(producers_clear && queue_clear && not_mid_callback) {
            return 0;
        }

        // Either nothing is sounding, or the safety deadline proves every
        // voice that started before the swap has finished by now.
        let voices_clear = stats.active_voices == 0;
        let deadline_passed = source
            .safety_deadline()
            .is_some_and(|deadline| std::time::Instant::now() >= deadline);

        source.retire(voices_clear || deadline_passed)
    }

    fn load_pack_inner<P: AsRef<Path>>(&mut self, path: P) -> Result<SwapReport, RuntimeError> {
        let pack = Arc::new(KvPack::open(path.as_ref())?);
        let mut player = PackPlayer::new(pack.clone(), self.output_rate);
        player.spatial(self.settings.spatial_audio_enabled);
        let player = Arc::new(player);

        let report = match self.source.clone() {
            Some(source) => source.swap(player),
            None => {
                self.source = Some(Arc::new(SwappableSource::new(player)));
                SwapReport {
                    from: "<none>".to_string(),
                    to: pack.stats().name.clone(),
                    retired: 0,
                    safe_after: std::time::Instant::now(),
                    elapsed: std::time::Duration::ZERO,
                }
            }
        };
        self.pack = Some(pack);
        Ok(report)
    }

    /// The loaded pack, if any.
    pub fn pack(&self) -> Option<&KvPack> {
        self.pack.as_deref()
    }

    /// The pack player input threads are playing through, if one is loaded.
    ///
    /// This is a snapshot: after [`switch_pack`](Self::switch_pack) the
    /// handle still works but the pack underneath it may already be parked.
    pub fn player(&self) -> Option<Arc<PackPlayer>> {
        self.source.as_ref().map(|source| source.current())
    }

    /// Audio output sample rate.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Lock-free command queue shared by input and audio threads.
    pub fn command_queue(&self) -> &Arc<SpscRing<PlayCommand>> {
        &self.command_queue
    }

    /// Starts the supervised PipeWire output stream.
    pub fn start_audio(&mut self) -> Result<(), AudioError> {
        let engine = AudioEngine::start_with_options(
            self.command_queue.clone(),
            AudioOptions {
                output_rate: self.output_rate,
                reconnect_policy: self.reconnect_policy,
                mixer: MixerSettings::from_settings(&self.settings),
                idle: self.idle_config,
                output_device: self.output_device.clone(),
            },
        )?;
        self.audio_stats = Some(engine.stats_block());
        // Input threads resume a paused stream with the same keypress that
        // produced the sound - never via the idle monitor's poll.
        self.wake = Some(Arc::new(engine.control()));
        self.engine = Some(engine);
        Ok(())
    }

    /// Lock-free idle state of the output stream.
    pub fn idle_state(&self) -> Option<Arc<IdleState>> {
        self.engine.as_ref().map(|engine| engine.idle_state())
    }

    /// Current idle phase (`None` when audio has not started).
    pub fn idle_phase(&self) -> Option<IdlePhase> {
        self.engine.as_ref().map(|engine| engine.idle_phase())
    }

    /// Forces the audio connection to be torn down and rebuilt.
    ///
    /// Recovery happens on the control plane with bounded backoff; the
    /// real-time callback is never involved.
    pub fn reconnect_audio(&self) {
        if let Some(engine) = &self.engine {
            engine.reconnect();
        }
    }

    /// Plays one key through the loaded pack and queues it for the mixer.
    ///
    /// Control-plane helper used by `keyvibes idle-test` (there is no
    /// readable keyboard on the machines that run the automated gates). It
    /// performs exactly the two production operations an input thread
    /// performs: wake the stream, then push the command.
    ///
    /// Returns `false` when no pack is loaded or the key has no sound.
    pub fn trigger_key(&self, key: PhysicalKey) -> bool {
        let Some(source) = self.source.clone() else {
            return false;
        };
        if let Some(wake) = self.wake.as_ref() {
            wake.wake();
        }
        let mut variants = self
            .trigger_variants
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = &mut variants[key.as_u16() as usize];
        let Some(command) = source.play(key, state) else {
            return false;
        };
        let outcome = self
            .command_queue
            .push(command.stamped(monotonic_ns()))
            .is_ok();
        source.queued(command);
        outcome
    }

    /// The audio engine, once started.
    pub fn audio(&self) -> Option<&AudioEngine> {
        self.engine.as_ref()
    }

    /// Snapshot of audio statistics (empty when audio has not started).
    ///
    /// Remains readable after the engine has been joined, because the
    /// statistics block is owned separately from the supervisor thread.
    pub fn audio_stats(&self) -> StreamStats {
        self.audio_stats
            .as_ref()
            .map(|stats| stats.snapshot())
            .unwrap_or_default()
    }

    /// Discovers keyboards and starts reader threads that play through the
    /// loaded pack.
    ///
    /// Zero readable keyboards is **not** an error: the hotplug monitor keeps
    /// running so a keyboard plugged in later is picked up without a restart.
    pub fn start_input(&mut self) -> Result<InputStart, RuntimeError> {
        let source = self.source.clone().ok_or(RuntimeError::NoPack)?;

        // The command queue is single-producer. Real keyboards and the
        // scripted source must never run at the same time, or two threads
        // would race on the same `push`.
        self.stop_simulated();

        let mut backend = LinuxInputBackend::new(
            self.command_queue.clone(),
            self.stats.clone(),
            source.clone(),
            self.wake.clone(),
        )?;

        // Hotplug is best-effort: losing it should not stop playback.
        if let Err(e) = backend.enable_hotplug() {
            eprintln!("hotplug monitoring unavailable: {e}");
        }

        let count = backend.device_count();
        self.input = Some(backend);
        Ok(if count > 0 {
            InputStart::Keyboards(count)
        } else {
            InputStart::WaitingForKeyboard
        })
    }

    /// Starts the scripted input source used when no keyboard is readable.
    pub fn start_simulated(&mut self, options: SimInputOptions) -> Result<(), RuntimeError> {
        let source = self.source.clone().ok_or(RuntimeError::NoPack)?;

        // See `start_input`: exactly one producer may own the queue.
        self.input = None;
        self.simulated = Some(SimulatedInput::start(
            self.command_queue.clone(),
            self.stats.clone(),
            source,
            options.script,
            options.repeats,
            self.wake.clone(),
        ));
        Ok(())
    }

    /// Stops the scripted input source, if running.
    pub fn stop_simulated(&mut self) {
        self.simulated = None;
    }

    /// The audio engine's control handle, once started.
    ///
    /// Lets a watchdog stop the engine without owning it, which is how
    /// [`run_for`](Self::run_for) and `keyvibes stress` terminate cleanly.
    pub fn audio_control(&self) -> Option<kv_audio_pipewire::AudioControl> {
        self.engine.as_ref().map(|engine| engine.control())
    }

    /// The input backend, once started.
    pub fn input_backend(&self) -> Option<&LinuxInputBackend> {
        self.input.as_ref()
    }

    /// Snapshot of input statistics.
    pub fn input_stats(&self) -> InputStatsSnapshot {
        self.stats.snapshot()
    }

    /// Waits until the audio stream reports an active state.
    pub fn wait_for_audio(&self, timeout: Duration) -> bool {
        match &self.engine {
            Some(engine) => engine.wait_until_active(timeout),
            None => false,
        }
    }

    /// Asks the audio engine to start or stop producing output.
    pub fn set_audio_active(&self, active: bool) {
        if let Some(engine) = &self.engine {
            engine.set_active(active);
        }
    }

    /// Runs the audio supervisor until it stops (blocks).
    pub fn run(&mut self) -> Result<(), AudioError> {
        match self.engine.take() {
            Some(engine) => engine.join(),
            None => Err(AudioError::InitFailed(
                "audio not started; call start_audio() first".to_string(),
            )),
        }
    }

    /// Runs the audio supervisor for at most `duration`, then shuts down.
    ///
    /// Used by the automated checks (`stress`, `soak-test`, `acceptance-test`)
    /// so they always terminate.
    pub fn run_for(&mut self, duration: Duration) -> Result<(), AudioError> {
        let control = match &self.engine {
            Some(engine) => engine.control(),
            None => {
                return Err(AudioError::InitFailed(
                    "audio not started; call start_audio() first".to_string(),
                ));
            }
        };

        let watchdog = std::thread::Builder::new()
            .name("keyvibes-run-timer".to_string())
            .spawn(move || {
                std::thread::sleep(duration);
                control.shutdown();
            })
            .map_err(|e| AudioError::InitFailed(format!("failed to spawn timer: {e}")))?;

        let result = self.run();
        let _ = watchdog.join();
        result
    }

    /// Requests a clean shutdown of every subsystem.
    pub fn shutdown(&mut self) {
        self.simulated = None;
        self.input = None;
        self.wake = None;
        if let Some(engine) = self.engine.take() {
            let _ = engine.stop_and_join();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Order matters: stop producers first so nothing pushes to a torn-down
        // queue, then stop the audio engine (which owns the RT callback).
        self.simulated = None;
        self.input = None;
        self.engine = None;
        self.audio_stats = None;
        self.wake = None;
        self.source = None;
        self.pack = None;
    }
}

/// Convenience helper used by diagnostics: keys the default script presses.
pub fn default_sim_keys() -> Vec<kv_core::PhysicalKey> {
    use kv_core::PhysicalKey::*;
    vec![A, S, D, F, J, K, L, Space]
}
