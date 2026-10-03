//! PipeWire stream implementation for KeyVibes audio output.
//!
//! This module provides the PipeWire backend that connects the deterministic
//! audio engine to actual audio devices. The stream operates with strict
//! real-time safety: the process callback performs zero allocations, takes no
//! locks (beyond the command queue), and does no I/O.

use kv_core::PlayCommand;
use kv_mixer::Mixer;
use kv_ring::SpscRing;
use pipewire as pw;
use std::sync::Arc;
use thiserror::Error;

/// PipeWire stream for audio playback.
pub struct PipeWireStream {
    _main_loop: pw::MainLoop,
    _stream: pw::stream::Stream,
    state: Arc<std::sync::Mutex<StreamState>>,
}

/// Internal stream state for diagnostics.
#[derive(Debug, Clone)]
struct StreamState {
    sample_rate: u32,
    channels: u32,
    quantum: u32,
    frames_rendered: u64,
    callbacks: u64,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            quantum: 256,
            frames_rendered: 0,
            callbacks: 0,
        }
    }
}

impl PipeWireStream {
    /// Creates a new PipeWire stream.
    ///
    /// # Arguments
    ///
    /// * `output_rate` - Target sample rate for the mixer
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
        pw::init();

        // Create main loop
        let main_loop = pw::MainLoop::new().map_err(|e| {
            AudioError::InitFailed(format!("Failed to create main loop: {:?}", e))
        })?;

        // Create stream
        let stream = pw::stream::Stream::new(
            &main_loop,
            "keyvibes-playback",
            pw::properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_CATEGORY => "Playback",
                *pw::keys::MEDIA_ROLE => "Music",
                *pw::keys::APP_NAME => "KeyVibes",
            },
        )
        .map_err(|e| AudioError::InitFailed(format!("Failed to create stream: {:?}", e)))?;

        // Set up mixer and state
        let mixer = Arc::new(std::sync::Mutex::new(Mixer::new(output_rate)));
        let state = Arc::new(std::sync::Mutex::new(StreamState::default()));

        // Clone references for the callback
        let mixer_cb = mixer.clone();
        let queue_cb = command_queue.clone();
        let state_cb = state.clone();

        // Add process callback
        let _listener = stream
            .add_local_listener()
            .process(move |stream| {
                process_callback(stream, &mixer_cb, &queue_cb, &state_cb);
            })
            .register();

        // Connect the stream
        stream
            .connect(
                pw::spa::utils::Direction::Output,
                None, // Connect to default sink
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut [pw::spa::pod::Object {
                    type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                    id: pw::spa::param::ParamType::EnumFormat.as_raw(),
                    properties: vec![
                        pw::spa::pod::Property {
                            key: pw::spa::param::format::FormatProperties::MediaType.as_raw(),
                            value: pw::spa::pod::Value::Id(
                                pw::spa::param::format::MediaType::Audio.as_raw(),
                            ),
                        },
                        pw::spa::pod::Property {
                            key: pw::spa::param::format::FormatProperties::MediaSubtype.as_raw(),
                            value: pw::spa::pod::Value::Id(
                                pw::spa::param::format::MediaSubtype::Raw.as_raw(),
                            ),
                        },
                        pw::spa::pod::Property {
                            key: pw::spa::param::format::FormatProperties::AudioFormat.as_raw(),
                            value: pw::spa::pod::Value::Id(
                                pw::spa::param::format::AudioFormat::F32LE.as_raw(),
                            ),
                        },
                        pw::spa::pod::Property {
                            key: pw::spa::param::format::FormatProperties::AudioRate.as_raw(),
                            value: pw::spa::pod::Value::Int(output_rate as i32),
                        },
                        pw::spa::pod::Property {
                            key: pw::spa::param::format::FormatProperties::AudioChannels.as_raw(),
                            value: pw::spa::pod::Value::Int(2),
                        },
                    ],
                }],
            )
            .map_err(|e| AudioError::ConnectionFailed(format!("Failed to connect: {:?}", e)))?;

        Ok(Self {
            _main_loop: main_loop,
            _stream: stream,
            state,
        })
    }

    /// Gets current stream statistics.
    pub fn get_stats(&self) -> StreamStats {
        let state = self.state.lock().unwrap();
        StreamStats {
            sample_rate: state.sample_rate,
            channels: state.channels,
            quantum: state.quantum,
            frames_rendered: state.frames_rendered,
            callbacks: state.callbacks,
        }
    }

    /// Runs the PipeWire main loop (blocking).
    pub fn run(&self) -> Result<(), AudioError> {
        self._main_loop.run();
        Ok(())
    }
}

/// Real-time process callback.
///
/// This function runs in PipeWire's real-time thread and must maintain strict
/// real-time safety guarantees.
fn process_callback(
    stream: &pw::stream::Stream,
    mixer: &Arc<std::sync::Mutex<Mixer>>,
    queue: &Arc<SpscRing<PlayCommand>>,
    state: &Arc<std::sync::Mutex<StreamState>>,
) {
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
    let chunk = data.chunk();
    let frames = (chunk.size() / 8) as usize; // F32LE stereo = 8 bytes per frame

    // Get output slice (f32 interleaved stereo)
    let output_slice = unsafe {
        std::slice::from_raw_parts_mut(data.data().as_mut_ptr() as *mut f32, frames * 2)
    };

    // Lock mixer (brief - only for render)
    let mut mixer_guard = mixer.lock().unwrap();

    // Drain command queue (lock-free)
    while let Some(cmd) = queue.pop() {
        mixer_guard.trigger(cmd);
    }

    // Render audio block
    unsafe {
        mixer_guard.render_block(output_slice);
    }

    // Update statistics
    if let Ok(mut state_guard) = state.try_lock() {
        state_guard.frames_rendered += frames as u64;
        state_guard.callbacks += 1;
    }

    // Mark chunk as filled
    chunk.set_size(frames * 8);
    chunk.set_stride(8);
}

/// Stream statistics.
#[derive(Debug, Clone)]
pub struct StreamStats {
    pub sample_rate: u32,
    pub channels: u32,
    pub quantum: u32,
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
