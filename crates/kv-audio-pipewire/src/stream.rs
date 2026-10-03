//! PipeWire stream implementation for KeyVibes audio output.
//!
//! This module provides the PipeWire backend that connects the deterministic
//! audio engine to actual audio devices. The stream operates with strict
//! real-time safety: the process callback performs ZERO allocations, ZERO
//! locks, and ZERO I/O.
//!
//! # Real-time Safety
//!
//! The RT callback owns the mixer directly (as listener user data, not through
//! `Arc<Mutex<>>`). All communication happens through lock-free SPSC queues.

use kv_core::PlayCommand;
use kv_mixer::Mixer;
use kv_ring::SpscRing;
use pipewire::context::ContextRc;
use pipewire::keys;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::properties;
use pipewire::spa;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;

/// Bytes per output frame: stereo f32 interleaved.
const FRAME_BYTES: usize = 8;

/// PipeWire stream for audio playback.
///
/// Field order matters: fields are dropped top-to-bottom, so the listener is
/// unregistered before the stream is destroyed and the main loop is torn down
/// last.
pub struct PipeWireStream {
    _listener: StreamListener<RtState>,
    _stream: StreamRc,
    _main_loop: MainLoopRc,
    stats: Arc<RtStats>,
}

/// Lock-free statistics using atomics.
#[derive(Debug)]
pub struct RtStats {
    frames_rendered: AtomicU64,
    callbacks: AtomicU64,
}

impl RtStats {
    fn new() -> Self {
        Self {
            frames_rendered: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
        }
    }

    fn increment_frames(&self, frames: u64) {
        self.frames_rendered.fetch_add(frames, Ordering::Relaxed);
    }

    fn increment_callbacks(&self) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_frames(&self) -> u64 {
        self.frames_rendered.load(Ordering::Relaxed)
    }

    pub fn get_callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }
}

/// Real-time callback state (owned by the RT thread as listener user data).
struct RtState {
    mixer: Mixer,
    queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<RtStats>,
}

impl PipeWireStream {
    /// Creates and connects a new PipeWire stream.
    ///
    /// # Arguments
    ///
    /// * `output_rate` - Target sample rate for the mixer and stream format
    /// * `command_queue` - SPSC queue for receiving play commands
    ///
    /// # Errors
    ///
    /// Returns `AudioError` if PipeWire initialization fails.
    pub fn new(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
    ) -> Result<Self, AudioError> {
        // Initialize PipeWire
        pipewire::init();

        let main_loop = MainLoopRc::new(None)
            .map_err(|e| AudioError::InitFailed(format!("Failed to create main loop: {e:?}")))?;

        let context = ContextRc::new(&main_loop, None)
            .map_err(|e| AudioError::InitFailed(format!("Failed to create context: {e:?}")))?;

        let core = context
            .connect_rc(None)
            .map_err(|e| AudioError::InitFailed(format!("Failed to connect core: {e:?}")))?;

        let stream = StreamRc::new(
            core,
            "keyvibes-playback",
            properties! {
                *keys::MEDIA_TYPE => "Audio",
                *keys::MEDIA_CATEGORY => "Playback",
                *keys::MEDIA_ROLE => "Music",
                *keys::APP_NAME => "KeyVibes",
            },
        )
        .map_err(|e| AudioError::InitFailed(format!("Failed to create stream: {e:?}")))?;

        let stats = Arc::new(RtStats::new());
        let rt_state = RtState {
            mixer: Mixer::new(output_rate),
            queue: command_queue,
            stats: stats.clone(),
        };

        let listener = stream
            .add_local_listener_with_user_data(rt_state)
            .process(|stream, state| {
                // SAFETY: runs on the PipeWire thread with exclusive access to
                // the listener user data. RefCell-free by construction.
                process_callback(stream, state);
            })
            .register()
            .map_err(|e| AudioError::StreamError(format!("Failed to register listener: {e:?}")))?;

        connect_output(&stream, output_rate)?;

        Ok(Self {
            _listener: listener,
            _stream: stream,
            _main_loop: main_loop,
            stats,
        })
    }

    /// Gets current stream statistics (lock-free).
    pub fn get_stats(&self) -> StreamStats {
        StreamStats {
            frames_rendered: self.stats.get_frames(),
            callbacks: self.stats.get_callbacks(),
        }
    }

    /// Runs the PipeWire main loop (blocking).
    pub fn run(&self) -> Result<(), AudioError> {
        self._main_loop.run();
        Ok(())
    }
}

/// Negotiates an output format (f32 stereo at `output_rate`) and connects.
fn connect_output(stream: &StreamRc, output_rate: u32) -> Result<(), AudioError> {
    use spa::param::audio::AudioFormat;
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::ParamType;
    use spa::pod::builder::builder_add;
    use spa::pod::builder::Builder;
    use spa::pod::Pod;

    let mut param_data = Vec::new();
    {
        let mut builder = Builder::new(&mut param_data);
        builder_add!(
            &mut builder,
            Object(
                spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                ParamType::EnumFormat.as_raw(),
            ) {
                FormatProperties::MediaType.as_raw() =>
                    Id(spa::utils::Id(MediaType::Audio.as_raw())),
                FormatProperties::MediaSubtype.as_raw() =>
                    Id(spa::utils::Id(MediaSubtype::Raw.as_raw())),
                FormatProperties::AudioFormat.as_raw() =>
                    Id(spa::utils::Id(AudioFormat::F32LE.as_raw())),
                FormatProperties::AudioRate.as_raw() => Int(output_rate as i32),
                FormatProperties::AudioChannels.as_raw() => Int(2),
            }
        )
        .map_err(|e| AudioError::InitFailed(format!("Failed to build format pod: {e}")))?;
    }

    let pod = Pod::from_bytes(&param_data)
        .ok_or_else(|| AudioError::InitFailed("Invalid format pod".to_string()))?;
    let mut params = [pod];

    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| AudioError::ConnectionFailed(format!("Failed to connect stream: {e:?}")))
}

/// Real-time process callback.
///
/// ZERO locks, ZERO allocations, ZERO I/O, ZERO blocking.
///
/// This function runs in PipeWire's real-time thread and must maintain strict
/// real-time safety guarantees.
fn process_callback(stream: &pipewire::stream::Stream, state: &mut RtState) {
    // Dequeue buffer from PipeWire
    let mut buffer = match stream.dequeue_buffer() {
        Some(buf) => buf,
        None => return,
    };

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    let frames = {
        let chunk = data.chunk();
        (chunk.size() / FRAME_BYTES as u32) as usize
    };

    if frames == 0 {
        return;
    }

    // Get output slice (f32 interleaved stereo)
    let output_ptr = match data.data() {
        Some(bytes) if bytes.len() >= frames * FRAME_BYTES => bytes.as_mut_ptr() as *mut f32,
        _ => return,
    };
    let output_slice = unsafe { std::slice::from_raw_parts_mut(output_ptr, frames * 2) };

    // Drain command queue (LOCK-FREE)
    while let Some(cmd) = state.queue.pop() {
        state.mixer.trigger(cmd);
    }

    // Render audio block (NO LOCKS, NO ALLOCATIONS)
    unsafe {
        state.mixer.render_block(output_slice);
    }

    // Update statistics (LOCK-FREE ATOMICS)
    state.stats.increment_frames(frames as u64);
    state.stats.increment_callbacks();

    // Mark chunk as filled
    let chunk = data.chunk_mut();
    *chunk.size_mut() = (frames * FRAME_BYTES) as u32;
    *chunk.stride_mut() = FRAME_BYTES as i32;
}

/// Stream statistics.
#[derive(Debug, Clone)]
pub struct StreamStats {
    pub frames_rendered: u64,
    pub callbacks: u64,
}

/// Audio backend errors.
#[derive(Error, Debug)]
pub enum AudioError {
    #[error("PipeWire initialization failed: {0}")]
    InitFailed(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Stream error: {0}")]
    StreamError(String),
}
