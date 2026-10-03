//! Runtime coordination for KeyVibes.
//!
//! The runtime wires the three subsystems together:
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
use kv_audio_pipewire::stream::{AudioError, PipeWireStream};
use kv_core::PlayCommand;
use kv_input_linux::diagnostics::InputStatsSnapshot;
use kv_input_linux::error::InputError;
use kv_input_linux::LinuxInputBackend;
use kv_pack::{KvPack, PackError};
use kv_ring::SpscRing;
use std::path::Path;
use std::sync::Arc;
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

/// Main runtime coordinating all subsystems.
pub struct Runtime {
    stream: Option<PipeWireStream>,
    command_queue: Arc<SpscRing<PlayCommand>>,
    output_rate: u32,
    pack: Option<Arc<KvPack>>,
    player: Option<Arc<PackPlayer>>,
    stats: Arc<kv_input_linux::InputStats>,
    input: Option<LinuxInputBackend>,
}

impl Runtime {
    /// Creates a runtime targeting the given output sample rate.
    ///
    /// Nothing is opened yet: call [`load_pack`](Self::load_pack),
    /// [`start_audio`](Self::start_audio), and [`start_input`](Self::start_input).
    pub fn new(output_rate: u32) -> Self {
        Self {
            stream: None,
            command_queue: Arc::new(SpscRing::with_capacity(256)),
            output_rate,
            pack: None,
            player: None,
            stats: Arc::new(kv_input_linux::InputStats::new()),
            input: None,
        }
    }

    /// Opens and validates a `.kvpack`, then builds the pack player.
    ///
    /// Performs filesystem I/O and full pack validation; must not be called
    /// from a real-time thread.
    pub fn load_pack<P: AsRef<Path>>(&mut self, path: P) -> Result<(), RuntimeError> {
        let pack = Arc::new(KvPack::open(path.as_ref())?);
        self.player = Some(Arc::new(PackPlayer::new(pack.clone(), self.output_rate)));
        self.pack = Some(pack);
        Ok(())
    }

    /// The loaded pack, if any.
    pub fn pack(&self) -> Option<&KvPack> {
        self.pack.as_deref()
    }

    /// The pack player shared with input threads, if a pack is loaded.
    pub fn player(&self) -> Option<&Arc<PackPlayer>> {
        self.player.as_ref()
    }

    /// Audio output sample rate.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Lock-free command queue shared by input and audio threads.
    pub fn command_queue(&self) -> &Arc<SpscRing<PlayCommand>> {
        &self.command_queue
    }

    /// Starts the PipeWire output stream (commands begin rendering immediately).
    pub fn start_audio(&mut self) -> Result<(), AudioError> {
        let stream = PipeWireStream::new(self.output_rate, self.command_queue.clone())?;
        self.stream = Some(stream);
        Ok(())
    }

    /// The PipeWire stream, once started.
    pub fn audio(&self) -> Option<&PipeWireStream> {
        self.stream.as_ref()
    }

    /// Discovers keyboards and starts reader threads that play through the
    /// loaded pack.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::NoPack`] when no pack is loaded, or the
    /// underlying input error (for example no keyboards found).
    pub fn start_input(&mut self) -> Result<(), RuntimeError> {
        let player = self.player.clone().ok_or(RuntimeError::NoPack)?;

        let mut backend = LinuxInputBackend::new(
            self.command_queue.clone(),
            self.stats.clone(),
            player.clone(),
        )?;

        // Hotplug is best-effort: losing it should not stop playback.
        if let Err(e) = backend.enable_hotplug(player) {
            eprintln!("hotplug monitoring unavailable: {e}");
        }

        self.input = Some(backend);
        Ok(())
    }

    /// The input backend, once started.
    pub fn input_backend(&self) -> Option<&LinuxInputBackend> {
        self.input.as_ref()
    }

    /// Snapshot of input statistics.
    pub fn input_stats(&self) -> InputStatsSnapshot {
        self.stats.snapshot()
    }

    /// Runs the PipeWire main loop (blocks until the process exits).
    pub fn run(&self) -> Result<(), AudioError> {
        match &self.stream {
            Some(stream) => stream.run(),
            None => Err(AudioError::InitFailed(
                "audio not started; call start_audio() first".to_string(),
            )),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Order matters: stop readers first so nothing pushes to a torn-down
        // queue, then drop the audio stream (which unregisters the RT listener).
        self.input = None;
        self.stream = None;
        self.player = None;
        self.pack = None;
    }
}
