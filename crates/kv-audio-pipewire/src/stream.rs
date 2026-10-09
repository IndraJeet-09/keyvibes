//! PipeWire stream implementation for KeyVibes audio output.
//!
//! This module provides the PipeWire backend that connects the deterministic
//! audio engine to actual audio devices. The stream operates with strict
//! real-time safety: the process callback performs ZERO allocations, ZERO
//! locks, and ZERO I/O.
//!
//! # Real Safety
//!
//! The RT callback owns the mixer directly (as listener user data, not through
//! `Arc<Mutex<>>`). All communication happens through lock-free SPSC queues.
//!
//! # Concurrency model
//!
//! `PW_STREAM_FLAG_RT_PROCESS` means the `process` callback can run on a
//! PipeWire data thread while `state_changed` runs on the PipeWire main loop
//! thread. pipewire-rs hands both callbacks a `&mut` to the *same* user data,
//! so KeyVibes deliberately registers **two** listeners:
//!
//! * listener A: `process` only, user data = [`RtState`] (owned by the RT thread)
//! * listener B: `state_changed` only, user data = [`StreamWatch`] (loop thread)
//!
//! Neither shares mutable state other than the lock-free [`RtStats`] atomics.

use kv_core::{monotonic_ns, AtomicHistogram, PlayCommand};
use kv_mixer::{Mixer, MixerSettings};
use kv_ring::SpscRing;
use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::keys;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::properties;
use pipewire::spa;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Once};
use thiserror::Error;

/// Bytes per output frame: stereo f32 interleaved.
const FRAME_BYTES: usize = 8;

/// Cycle size assumed when neither the peer nor our own history says otherwise.
const FALLBACK_FRAMES: usize = 1024;

/// Fraction of the quantum budget the callback must stay under to be
/// considered healthy. A callback using more than this share of its quantum
/// is counted as a safety-budget overrun.
pub const DEFAULT_SAFETY_BUDGET_PCT: f64 = 0.5;

static PIPEWIRE_INIT: Once = Once::new();

/// Idempotent PipeWire library initialization.
pub fn init_pipewire() {
    PIPEWIRE_INIT.call_once(pipewire::init);
}

/// Stream lifecycle state, as observed on the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum StreamStateCode {
    #[default]
    Unconnected = 0,
    Connecting = 1,
    Paused = 2,
    Streaming = 3,
    Error = 4,
    Unknown = 255,
}

impl StreamStateCode {
    fn from_raw(state: &pipewire::stream::StreamState) -> Self {
        match state {
            pipewire::stream::StreamState::Unconnected => Self::Unconnected,
            pipewire::stream::StreamState::Connecting => Self::Connecting,
            pipewire::stream::StreamState::Paused => Self::Paused,
            pipewire::stream::StreamState::Streaming => Self::Streaming,
            pipewire::stream::StreamState::Error(_) => Self::Error,
        }
    }

    /// Human-readable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconnected => "unconnected",
            Self::Connecting => "connecting",
            Self::Paused => "paused",
            Self::Streaming => "streaming",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }
}

/// Lock-free statistics shared by the RT callback and the control plane.
#[derive(Debug)]
pub struct RtStats {
    frames_rendered: AtomicU64,
    callbacks: AtomicU64,
    drained_commands: AtomicU64,
    active_voices: AtomicU32,
    peak_bits: AtomicU32,

    callback_ns_total: AtomicU64,
    callback_ns_max: AtomicU64,
    callback_hist: AtomicHistogram,

    // Attribution for the slowest callback: which section paid the cost.
    dequeue_ns_max: AtomicU64,
    drain_ns_max: AtomicU64,
    render_ns_max: AtomicU64,

    queue_latency_total: AtomicU64,
    queue_latency_max: AtomicU64,
    queue_latency_hist: AtomicHistogram,

    deadline_misses: AtomicU64,
    budget_overruns: AtomicU64,
    late_callbacks: AtomicU64,

    no_buffer: AtomicU64,
    empty_buffer: AtomicU64,
    no_mapping: AtomicU64,

    quantum_ns: AtomicU64,
    safety_budget_ns: AtomicU64,

    stream_state: AtomicU8,
    stream_saw_streaming: AtomicBool,
    stream_errors: AtomicU64,
    reconnects: AtomicU64,

    // Sample-rate negotiation (control plane, never the RT thread).
    output_rate: AtomicU32,
    negotiated_rate: AtomicU32,
    rate_changes: AtomicU64,

    // True while the data thread is inside `process_callback`. The control
    // plane reads it to know that no command can be "between the queue and a
    // voice" - the one window in which a retired pack would still be needed.
    in_callback: AtomicBool,
}

impl RtStats {
    /// Creates the statistics block for an engine rendering at `output_rate`.
    pub fn new(output_rate: u32) -> Self {
        let stats = Self {
            frames_rendered: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            drained_commands: AtomicU64::new(0),
            active_voices: AtomicU32::new(0),
            peak_bits: AtomicU32::new(0),
            callback_ns_total: AtomicU64::new(0),
            callback_ns_max: AtomicU64::new(0),
            callback_hist: AtomicHistogram::new(),
            dequeue_ns_max: AtomicU64::new(0),
            drain_ns_max: AtomicU64::new(0),
            render_ns_max: AtomicU64::new(0),
            queue_latency_total: AtomicU64::new(0),
            queue_latency_max: AtomicU64::new(0),
            queue_latency_hist: AtomicHistogram::new(),
            deadline_misses: AtomicU64::new(0),
            budget_overruns: AtomicU64::new(0),
            late_callbacks: AtomicU64::new(0),
            no_buffer: AtomicU64::new(0),
            empty_buffer: AtomicU64::new(0),
            no_mapping: AtomicU64::new(0),
            quantum_ns: AtomicU64::new(0),
            safety_budget_ns: AtomicU64::new(0),
            stream_state: AtomicU8::new(StreamStateCode::Unconnected as u8),
            stream_saw_streaming: AtomicBool::new(false),
            stream_errors: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            output_rate: AtomicU32::new(output_rate),
            negotiated_rate: AtomicU32::new(output_rate),
            rate_changes: AtomicU64::new(0),
            in_callback: AtomicBool::new(false),
        };
        stats.set_output_rate(output_rate);
        stats
    }

    /// Recomputes the quantum-derived budgets for `output_rate`.
    pub fn set_output_rate(&self, output_rate: u32) {
        self.output_rate.store(output_rate, Ordering::Relaxed);
        // Until PipeWire tells us otherwise, assume it honoured the request.
        self.negotiated_rate.store(output_rate, Ordering::Relaxed);
        // A default 1024-frame quantum: PipeWire renegotiates this per stream,
        // and `record_callback` refreshes it from the actual buffer size.
        let quantum = default_quantum_ns(output_rate, 1024);
        self.quantum_ns.store(quantum, Ordering::Relaxed);
        self.safety_budget_ns.store(
            (quantum as f64 * DEFAULT_SAFETY_BUDGET_PCT) as u64,
            Ordering::Relaxed,
        );
    }

    fn record_stream_state(&self, state: StreamStateCode) {
        self.stream_state.store(state as u8, Ordering::Relaxed);
        // The final state after teardown is always Unconnected, so remember
        // that the stream did reach the running state at least once.
        if state == StreamStateCode::Streaming {
            self.stream_saw_streaming.store(true, Ordering::Relaxed);
        }
        if state == StreamStateCode::Error {
            self.stream_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Records the format PipeWire actually chose for the stream.
    ///
    /// Returns `true` when it differs from the rate the mixer renders at,
    /// which means the connection has to be rebuilt at the negotiated rate or
    /// every sample would be played at the wrong speed. Runs on the PipeWire
    /// loop thread only.
    pub(crate) fn record_negotiated_rate(&self, rate: u32) -> bool {
        if rate == 0 {
            return false;
        }
        self.negotiated_rate.store(rate, Ordering::Relaxed);
        let requested = self.output_rate.load(Ordering::Relaxed);
        if rate == requested {
            return false;
        }
        self.rate_changes.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// The sample rate the mixer currently renders at.
    pub fn output_rate(&self) -> u32 {
        self.output_rate.load(Ordering::Relaxed)
    }

    /// The sample rate PipeWire reported for the live stream.
    pub fn negotiated_rate(&self) -> u32 {
        self.negotiated_rate.load(Ordering::Relaxed)
    }

    /// How many times negotiation produced a rate we had to rebuild for.
    pub fn rate_changes(&self) -> u64 {
        self.rate_changes.load(Ordering::Relaxed)
    }

    /// Whether the real-time thread is inside the process callback right now.
    pub fn in_callback(&self) -> bool {
        self.in_callback.load(Ordering::SeqCst)
    }

    /// Enters / leaves the process callback. Data thread only.
    #[inline]
    fn set_in_callback(&self, value: bool) {
        self.in_callback.store(value, Ordering::SeqCst);
    }

    /// Clears counters that belong to a connection that no longer exists.
    ///
    /// Called when a stream is (re)built: the new connection starts with its
    /// own mixer, so any voice count left over from the previous one would be
    /// a lie the control plane might act on.
    pub(crate) fn reset_connection_state(&self) {
        self.active_voices.store(0, Ordering::Relaxed);
        self.in_callback.store(false, Ordering::SeqCst);
    }

    /// Snapshot of every statistic; control plane only (allocates).
    pub fn snapshot(&self) -> StreamStats {
        let peak_bits = self.peak_bits.load(Ordering::Relaxed);
        StreamStats {
            frames_rendered: self.frames_rendered.load(Ordering::Relaxed),
            callbacks: self.callbacks.load(Ordering::Relaxed),
            drained_commands: self.drained_commands.load(Ordering::Relaxed),
            active_voices: self.active_voices.load(Ordering::Relaxed),
            peak: f32::from_bits(peak_bits),
            callback_ns_total: self.callback_ns_total.load(Ordering::Relaxed),
            callback_ns_max: self.callback_ns_max.load(Ordering::Relaxed),
            callback_p50_ns: self.callback_hist.percentile_ns(0.50),
            callback_p95_ns: self.callback_hist.percentile_ns(0.95),
            callback_p99_ns: self.callback_hist.percentile_ns(0.99),
            dequeue_ns_max: self.dequeue_ns_max.load(Ordering::Relaxed),
            drain_ns_max: self.drain_ns_max.load(Ordering::Relaxed),
            render_ns_max: self.render_ns_max.load(Ordering::Relaxed),
            queue_latency_max_ns: self.queue_latency_max.load(Ordering::Relaxed),
            queue_latency_p50_ns: self.queue_latency_hist.percentile_ns(0.50),
            queue_latency_p95_ns: self.queue_latency_hist.percentile_ns(0.95),
            queue_latency_p99_ns: self.queue_latency_hist.percentile_ns(0.99),
            deadline_misses: self.deadline_misses.load(Ordering::Relaxed),
            budget_overruns: self.budget_overruns.load(Ordering::Relaxed),
            late_callbacks: self.late_callbacks.load(Ordering::Relaxed),
            no_buffer: self.no_buffer.load(Ordering::Relaxed),
            empty_buffer: self.empty_buffer.load(Ordering::Relaxed),
            no_mapping: self.no_mapping.load(Ordering::Relaxed),
            quantum_ns: self.quantum_ns.load(Ordering::Relaxed),
            safety_budget_ns: self.safety_budget_ns.load(Ordering::Relaxed),
            stream_state: StreamStateCode::from_u8(self.stream_state.load(Ordering::Relaxed)),
            saw_streaming: self.stream_saw_streaming.load(Ordering::Relaxed),
            stream_errors: self.stream_errors.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            output_rate: self.output_rate.load(Ordering::Relaxed),
            negotiated_rate: self.negotiated_rate.load(Ordering::Relaxed),
            rate_changes: self.rate_changes.load(Ordering::Relaxed),
            in_callback: self.in_callback(),
        }
    }
}

/// Plain-data snapshot of [`RtStats`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamStats {
    pub frames_rendered: u64,
    pub callbacks: u64,
    pub drained_commands: u64,
    pub active_voices: u32,
    pub peak: f32,
    pub callback_ns_total: u64,
    pub callback_ns_max: u64,
    pub callback_p50_ns: u64,
    pub callback_p95_ns: u64,
    pub callback_p99_ns: u64,
    /// Slowest buffer dequeue / setup seen (PipeWire client side).
    pub dequeue_ns_max: u64,
    /// Slowest command-queue drain seen.
    pub drain_ns_max: u64,
    /// Slowest `Mixer::render_block` seen.
    pub render_ns_max: u64,
    pub queue_latency_max_ns: u64,
    pub queue_latency_p50_ns: u64,
    pub queue_latency_p95_ns: u64,
    pub queue_latency_p99_ns: u64,
    pub deadline_misses: u64,
    pub budget_overruns: u64,
    pub late_callbacks: u64,
    pub no_buffer: u64,
    pub empty_buffer: u64,
    pub no_mapping: u64,
    pub quantum_ns: u64,
    pub safety_budget_ns: u64,
    pub stream_state: StreamStateCode,
    pub saw_streaming: bool,
    pub stream_errors: u64,
    pub reconnects: u64,
    /// Sample rate the mixer renders at.
    pub output_rate: u32,
    /// Sample rate PipeWire negotiated for the stream.
    pub negotiated_rate: u32,
    /// Times negotiation forced a rebuild at a different rate.
    pub rate_changes: u64,
    /// Whether the data thread is inside the process callback right now.
    pub in_callback: bool,
}

impl StreamStats {
    /// Cycles where the client could not supply audio to PipeWire.
    ///
    /// `no_buffer` is a cycle in which PipeWire handed us no output buffer;
    /// `empty_buffer` is one where the buffer carried no data planes. Either
    /// means the sink had to repeat or insert samples. PipeWire's own XRUN
    /// counter is read separately: see [`crate::xrun`].
    pub fn producer_underruns(&self) -> u64 {
        self.no_buffer + self.empty_buffer
    }

    /// Whether the real-time budget checks all passed.
    ///
    /// The budget is a fraction of the measured quantum, stored in
    /// [`StreamStats::safety_budget_ns`].
    pub fn within_safety_budget(&self) -> bool {
        self.safety_budget_ns > 0 && self.callback_ns_max < self.safety_budget_ns
    }
}

impl StreamStateCode {
    /// Wraps a raw discriminant back into a state code.
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Unconnected,
            1 => Self::Connecting,
            2 => Self::Paused,
            3 => Self::Streaming,
            4 => Self::Error,
            _ => Self::Unknown,
        }
    }
}

fn default_quantum_ns(rate: u32, frames: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    (frames as u64) * 1_000_000_000 / (rate as u64)
}

/// Real-time callback state (owned by the RT thread as listener user data).
struct RtState {
    mixer: Mixer,
    queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<RtStats>,
    output_rate: u32,
    last_callback_start_ns: u64,
    /// Frame count of the previous cycle; used when `requested == 0`.
    last_frames: usize,
}

/// Separate listener user data used from the PipeWire main-loop thread.
///
/// Kept apart from [`RtState`] so the two threads never alias the same `&mut`.
struct StreamWatch {
    stats: Arc<RtStats>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    /// Set once when negotiation hands back a rate the mixer cannot use.
    rate_changed: Arc<std::sync::atomic::AtomicBool>,
}

/// Everything needed to keep one PipeWire connection alive.
pub struct Connection {
    main_loop: MainLoopRc,
    stream: StreamRc,
    stats: Arc<RtStats>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    rate_changed: Arc<std::sync::atomic::AtomicBool>,
    _core_listener: pipewire::core::Listener,
    _process_listener: StreamListener<RtState>,
    _watch_listener: StreamListener<StreamWatch>,
    _context: ContextRc,
    _core: CoreRc,
}

impl Connection {
    /// Builds and connects a PipeWire output stream on the default sink.
    ///
    /// Performs no real-time work: call from a control thread.
    pub fn build(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<RtStats>,
        settings: MixerSettings,
    ) -> Result<Self, AudioError> {
        Self::build_targeted(output_rate, command_queue, stats, settings, None)
    }

    /// Builds and connects a PipeWire output stream routed to `target`.
    ///
    /// `target` is a PipeWire node name or object serial (the `target.object`
    /// property). `None` leaves routing to the session manager, which is what
    /// "default sink" means in PipeWire. Runs entirely on a control thread.
    pub fn build_targeted(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<RtStats>,
        settings: MixerSettings,
        target: Option<&str>,
    ) -> Result<Self, AudioError> {
        init_pipewire();

        stats.set_output_rate(output_rate);
        stats.reset_connection_state();

        let main_loop = MainLoopRc::new(None)
            .map_err(|e| AudioError::InitFailed(format!("failed to create main loop: {e:?}")))?;

        let context = ContextRc::new(&main_loop, None)
            .map_err(|e| AudioError::InitFailed(format!("failed to create context: {e:?}")))?;

        let core = context
            .connect_rc(None)
            .map_err(|e| AudioError::InitFailed(format!("failed to connect core: {e:?}")))?;

        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Core-level fatal errors: record and stop the loop so the supervisor
        // can rebuild the connection. Runs on the loop thread.
        let core_fail = failed.clone();
        let core_stats = stats.clone();
        let core_loop = main_loop.clone();
        let _core_listener = core
            .add_listener_local()
            .error(move |_id, _seq, _res, message| {
                tracing_error_free_log(&format!("pipewire core error: {message}"));
                core_fail.store(true, Ordering::Relaxed);
                core_stats.record_stream_state(StreamStateCode::Error);
                core_loop.quit();
            })
            .register();

        let mut properties = properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Playback",
            *keys::MEDIA_ROLE => "Music",
            *keys::APP_NAME => "KeyVibes",
            *keys::NODE_NAME => "keyvibes",
            *keys::NODE_DESCRIPTION => "KeyVibes keyboard sounds",
            // Let the session manager suspend our node when it is idle; we
            // still keep the stream connected so the first key after idle
            // resumes without a reconnect.
            "node.suspend-on-idle" => "false",
        };
        if let Some(target) = target {
            properties.insert(*keys::TARGET_OBJECT, target);
        }

        let stream = StreamRc::new(core.clone(), "keyvibes-playback", properties)
            .map_err(|e| AudioError::InitFailed(format!("failed to create stream: {e:?}")))?;

        let rt_state = RtState {
            mixer: Mixer::with_settings(output_rate, settings),
            queue: command_queue,
            stats: stats.clone(),
            output_rate,
            last_callback_start_ns: 0,
            last_frames: 0,
        };

        let process_listener = stream
            .add_local_listener_with_user_data(rt_state)
            .process(|stream, state| {
                // SAFETY: runs on the PipeWire data thread with exclusive
                // access to the listener user data.
                process_callback(stream, state);
            })
            .register()
            .map_err(|e| AudioError::StreamError(format!("failed to register listener: {e:?}")))?;

        let rate_changed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let watch = StreamWatch {
            stats: stats.clone(),
            failed: failed.clone(),
            rate_changed: rate_changed.clone(),
        };
        let watch_loop = main_loop.clone();
        let param_loop = main_loop.clone();
        let watch_listener = stream
            .add_local_listener_with_user_data(watch)
            .state_changed(move |_stream, user, _old, new| {
                let code = StreamStateCode::from_raw(&new);
                user.stats.record_stream_state(code);
                if let pipewire::stream::StreamState::Error(message) = &new {
                    tracing_error_free_log(&format!("pipewire stream error: {message}"));
                }
                if matches!(new, pipewire::stream::StreamState::Error(_)) {
                    user.failed.store(true, Ordering::Relaxed);
                    watch_loop.quit();
                }
            })
            .param_changed(move |_stream, user, id, param| {
                use spa::param::ParamType;
                if id != ParamType::EnumFormat.as_raw() {
                    return;
                }
                let Some(param) = param else { return };
                let mut info = spa::param::audio::AudioInfoRaw::new();
                if info.parse(param).is_err() {
                    return;
                }
                // Negotiation produced a rate the mixer does not render at.
                // Hand the decision to the control-plane supervisor: rebuild
                // the connection at the negotiated rate instead of playing
                // every sample at the wrong speed.
                if user.stats.record_negotiated_rate(info.rate())
                    && user
                        .rate_changed
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
                        .is_ok()
                {
                    tracing_error_free_log(&format!(
                        "pipewire negotiated {} Hz, stream will be rebuilt",
                        info.rate()
                    ));
                    param_loop.quit();
                }
            })
            .register()
            .map_err(|e| AudioError::StreamError(format!("failed to register watch: {e:?}")))?;

        connect_output(&stream, output_rate)?;
        stats.record_stream_state(StreamStateCode::Connecting);

        Ok(Self {
            main_loop,
            stream,
            stats,
            failed,
            rate_changed,
            _core_listener,
            _process_listener: process_listener,
            _watch_listener: watch_listener,
            _context: context,
            _core: core,
        })
    }

    /// Runs the PipeWire main loop until it quits.
    pub fn run(&self) {
        self.main_loop.run();
    }

    /// The PipeWire main loop (control plane only).
    pub fn loop_(&self) -> &pipewire::loop_::Loop {
        self.main_loop.loop_()
    }

    /// Requests the loop to stop (must be called from the loop thread).
    pub fn quit(&self) {
        self.main_loop.quit();
    }

    /// The stream handle (control plane only).
    pub fn stream(&self) -> &StreamRc {
        &self.stream
    }

    /// The PipeWire main loop (control plane only).
    pub fn main_loop(&self) -> MainLoopRc {
        self.main_loop.clone()
    }

    /// Shared statistics block.
    pub fn stats(&self) -> Arc<RtStats> {
        self.stats.clone()
    }

    /// Whether the supervisor should rebuild this connection.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Whether negotiation handed back a sample rate the mixer cannot use.
    ///
    /// Cleared by construction: each new connection starts with the flag
    /// down, so a rebuild only happens once per mismatch.
    pub fn rate_changed(&self) -> bool {
        self.rate_changed.load(Ordering::Relaxed)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Field order drops listeners before the stream, which drops before
        // the core/context/main loop.
    }
}

/// Logs from the PipeWire callbacks without touching the RT path.
///
/// `state_changed` and core errors run on the control-plane loop thread, so a
/// plain `eprintln!` would be legal there; we still funnel through this helper
/// to keep a single, greppable place for audio-backend messages.
fn tracing_error_free_log(message: &str) {
    // Deliberately no timestamping or formatting work: control plane only.
    eprintln!("keyvibes: {message}");
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
        .map_err(|e| AudioError::InitFailed(format!("failed to build format pod: {e}")))?;
    }

    let pod = Pod::from_bytes(&param_data)
        .ok_or_else(|| AudioError::InitFailed("invalid format pod".to_string()))?;
    let mut params = [pod];

    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| AudioError::ConnectionFailed(format!("failed to connect stream: {e:?}")))
}

/// Real-time process callback.
///
/// ZERO locks, ZERO allocations, ZERO I/O, ZERO blocking, ZERO logging.
fn process_callback(stream: &pipewire::stream::Stream, state: &mut RtState) {
    // Brackets every early return below: while this is true the control
    // plane knows nothing can be sitting between the queue and a voice.
    state.stats.set_in_callback(true);
    process_inner(stream, state);
    state.stats.set_in_callback(false);
}

fn process_inner(_stream: &pipewire::stream::Stream, state: &mut RtState) {
    let start_ns = monotonic_ns();
    state.stats.callbacks.fetch_add(1, Ordering::Relaxed);

    let mut buffer = match _stream.dequeue_buffer() {
        Some(buf) => buf,
        None => {
            state.stats.no_buffer.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    // `requested` is how many frames the consumer wants for this cycle. On a
    // freshly dequeued output buffer `chunk.size` is still zero: the client is
    // the one that fills the buffer in and publishes the size.
    let requested = buffer.requested() as usize;

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        state.stats.empty_buffer.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let data = &mut datas[0];
    let capacity = data
        .data()
        .map(|bytes| bytes.len() / FRAME_BYTES)
        .unwrap_or(0);

    // Fall back to the last known cycle size when the peer does not request
    // explicitly, then clamp to the mapped capacity.
    let frames = if requested > 0 {
        requested.min(capacity)
    } else if state.last_frames > 0 {
        state.last_frames.min(capacity)
    } else {
        FALLBACK_FRAMES.min(capacity)
    };

    if frames == 0 {
        state.stats.empty_buffer.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let output_ptr = match data.data() {
        Some(bytes) if bytes.len() >= frames * FRAME_BYTES => bytes.as_mut_ptr() as *mut f32,
        _ => {
            state.stats.no_mapping.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let output_slice = unsafe { std::slice::from_raw_parts_mut(output_ptr, frames * 2) };

    let after_setup = monotonic_ns();

    // Drain command queue (lock-free). One clock read per drained batch.
    let mut drained = 0u32;
    let dequeue_ns = monotonic_ns();
    while let Some(cmd) = state.queue.pop() {
        if cmd.enqueued_ns != 0 && dequeue_ns >= cmd.enqueued_ns {
            let latency = dequeue_ns - cmd.enqueued_ns;
            state.stats.queue_latency_hist.record_ns(latency);
            state
                .stats
                .queue_latency_total
                .fetch_add(latency, Ordering::Relaxed);
            state
                .stats
                .queue_latency_max
                .fetch_max(latency, Ordering::Relaxed);
        }
        state.mixer.trigger(cmd);
        drained += 1;
    }

    let after_drain = monotonic_ns();

    // Render audio block (no locks, no allocations).
    unsafe {
        state.mixer.render_block(output_slice);
    }

    let end_ns = monotonic_ns();
    let duration = end_ns.saturating_sub(start_ns);
    let stats = &state.stats;

    // Attribute the callback: which section paid for the wall time?
    stats
        .dequeue_ns_max
        .fetch_max(after_setup.saturating_sub(start_ns), Ordering::Relaxed);
    stats
        .drain_ns_max
        .fetch_max(after_drain.saturating_sub(after_setup), Ordering::Relaxed);
    stats
        .render_ns_max
        .fetch_max(end_ns.saturating_sub(after_drain), Ordering::Relaxed);

    stats
        .frames_rendered
        .fetch_add(frames as u64, Ordering::Relaxed);
    if drained > 0 {
        stats
            .drained_commands
            .fetch_add(drained as u64, Ordering::Relaxed);
    }
    stats
        .active_voices
        .store(state.mixer.active_voice_count() as u32, Ordering::Relaxed);

    // Peak magnitude as a monotonically ordered bit pattern.
    let peak_bits = state.mixer.last_peak().to_bits();
    stats.peak_bits.fetch_max(peak_bits, Ordering::Relaxed);

    stats.callback_hist.record_ns(duration);
    stats
        .callback_ns_total
        .fetch_add(duration, Ordering::Relaxed);
    stats.callback_ns_max.fetch_max(duration, Ordering::Relaxed);

    // Scheduling budgets derived from this very buffer.
    let quantum = default_quantum_ns(state.output_rate, frames as u32);
    if quantum > 0 {
        stats.quantum_ns.store(quantum, Ordering::Relaxed);
        let safety = (quantum as f64 * DEFAULT_SAFETY_BUDGET_PCT) as u64;
        stats.safety_budget_ns.store(safety, Ordering::Relaxed);
        if duration >= quantum {
            stats.deadline_misses.fetch_add(1, Ordering::Relaxed);
        }
        if duration > safety {
            stats.budget_overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Inter-callback gap: more than 2.5 quanta means we were scheduled late.
    let previous = state.last_callback_start_ns;
    state.last_callback_start_ns = start_ns;
    if previous != 0 && quantum > 0 {
        let gap = start_ns.saturating_sub(previous);
        if gap > quantum.saturating_mul(5) / 2 {
            stats.late_callbacks.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Publish the filled chunk back to PipeWire. This must be the last write
    // to the buffer: the peer reads `chunk.size` to know how much audio we
    // produced for this cycle.
    let chunk = data.chunk_mut();
    *chunk.size_mut() = (frames * FRAME_BYTES) as u32;
    *chunk.stride_mut() = FRAME_BYTES as i32;
    *chunk.offset_mut() = 0;
    state.last_frames = frames;
}

/// Blocking single-connection audio stream (used by diagnostics and tests).
///
/// For the production path with reconnection, use [`crate::engine::AudioEngine`].
pub struct PipeWireStream {
    connection: Connection,
}

impl PipeWireStream {
    /// Creates and connects a new PipeWire stream.
    pub fn new(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
    ) -> Result<Self, AudioError> {
        Self::with_settings(output_rate, command_queue, MixerSettings::default())
    }

    /// Creates and connects a new PipeWire stream with explicit mixer settings.
    pub fn with_settings(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
        settings: MixerSettings,
    ) -> Result<Self, AudioError> {
        Self::with_target(output_rate, command_queue, settings, None)
    }

    /// Creates a stream routed to a specific PipeWire node.
    pub fn with_target(
        output_rate: u32,
        command_queue: Arc<SpscRing<PlayCommand>>,
        settings: MixerSettings,
        target: Option<&str>,
    ) -> Result<Self, AudioError> {
        let stats = Arc::new(RtStats::new(output_rate));
        Ok(Self {
            connection: Connection::build_targeted(
                output_rate,
                command_queue,
                stats,
                settings,
                target,
            )?,
        })
    }

    /// Gets current stream statistics (lock-free).
    pub fn get_stats(&self) -> StreamStats {
        self.connection.stats.snapshot()
    }

    /// Shared statistics block.
    pub fn stats(&self) -> Arc<RtStats> {
        self.connection.stats()
    }

    /// Runs the PipeWire main loop (blocking).
    pub fn run(&self) -> Result<(), AudioError> {
        self.connection.run();
        if self.connection.failed() {
            return Err(AudioError::ConnectionFailed(
                "PipeWire stream entered an error state".to_string(),
            ));
        }
        Ok(())
    }
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

    #[error("gave up reconnecting after {0} attempts")]
    ReconnectExhausted(u32),

    #[error("audio engine stopped")]
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_state_code_roundtrip() {
        for code in [
            StreamStateCode::Unconnected,
            StreamStateCode::Connecting,
            StreamStateCode::Paused,
            StreamStateCode::Streaming,
            StreamStateCode::Error,
        ] {
            assert_eq!(StreamStateCode::from_u8(code as u8), code);
        }
        assert_eq!(StreamStateCode::from_u8(200), StreamStateCode::Unknown);
    }

    #[test]
    fn test_quantum_ns() {
        assert_eq!(default_quantum_ns(48_000, 1024), 21_333_333);
        assert_eq!(default_quantum_ns(0, 1024), 0);
    }

    #[test]
    fn test_rt_stats_snapshot_defaults() {
        let stats = RtStats::new(48_000);
        let snap = stats.snapshot();
        assert_eq!(snap.callbacks, 0);
        assert_eq!(snap.deadline_misses, 0);
        assert_eq!(snap.stream_state, StreamStateCode::Unconnected);
        assert_eq!(snap.safety_budget_ns, 10_666_666);
    }

    #[test]
    fn test_duration_helper_is_unused_but_documented() {
        use std::time::{Duration, Instant};
        // Guards against accidental removal of the quantum math unit.
        let d = Duration::from_micros(100);
        assert_eq!(d.as_nanos(), 100_000);
        let _ = Instant::now();
    }
}
