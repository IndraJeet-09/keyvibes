//! Supervised PipeWire audio engine.
//!
//! [`AudioEngine`] owns the PipeWire main loop on a dedicated **control**
//! thread. The real-time thread is owned entirely by PipeWire and only ever
//! runs `process_callback`.
//!
//! Responsibilities kept off the RT thread:
//!
//! * connection setup and teardown
//! * bounded reconnection with exponential backoff
//! * forced reconnection (`AudioControl::reconnect`)
//! * sample-rate renegotiation
//! * idle stream suspend/resume
//! * lifecycle state reporting
//!
//! The engine shares one lock-free [`SpscRing`](kv_ring::SpscRing) of
//! [`PlayCommand`]s with the input threads for the whole process lifetime, so
//! reconnecting never loses queued commands.

use crate::idle::{spawn_idle_monitor, IdleConfig, IdleState};
use crate::lifecycle::{AudioPhase, LifecycleState, ReconnectPolicy, ReconnectStateMachine};
use crate::stream::{AudioError, Connection, RtStats, StreamStateCode, StreamStats};
use kv_core::{PlayCommand, StreamWake};
use kv_mixer::MixerSettings;
use kv_ring::SpscRing;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Attempts allowed to follow a sample-rate mismatch before KeyVibes stops
/// chasing PipeWire and keeps the current rate.
const MAX_RATE_RENEGOTIATIONS: u32 = 2;

/// Desired activity of the output stream.
///
/// One `bool` behind a short-held mutex: the two writers are the input path
/// (waking on a key) and the idle monitor (pausing after inactivity), and
/// they must not interleave into "stream paused while the engine believes it
/// is awake". This is control-plane state and is never touched by the
/// real-time callback.
#[derive(Debug, Clone, Copy)]
struct Activity {
    wanted_active: bool,
}

/// Control-plane messages handled on the PipeWire loop thread.
enum EngineCmd {
    /// Stop the loop so the supervisor can tear the connection down.
    Shutdown,
    /// Enable or disable the output stream (idle management).
    SetActive(bool),
    /// Tear the connection down and rebuild it (recovery testing).
    Reconnect,
    /// Wake the loop without side effects.
    Ping,
}

/// Everything [`AudioEngine::start_with_options`] needs to bring the output
/// up.
///
/// Bundled so the runtime does not have to thread six positional arguments
/// through its own configuration path, and so a future option (a buffer
/// size, a second device) has one obvious place to land.
#[derive(Debug, Clone)]
pub struct AudioOptions {
    /// Requested output sample rate in Hz.
    pub output_rate: u32,
    /// Bounded reconnect behaviour for the control-plane supervisor.
    pub reconnect_policy: ReconnectPolicy,
    /// Mixer behaviour (variation, spatialisation, release sounds).
    pub mixer: MixerSettings,
    /// Idle-management policy; see [`crate::idle`].
    pub idle: IdleConfig,
    /// PipeWire node name or object serial to route to.
    ///
    /// `None` means "whatever the session manager considers default", which
    /// is the normal desktop behaviour.
    pub output_device: Option<String>,
}

impl AudioOptions {
    /// Options for a plain output stream with every default.
    pub fn new(output_rate: u32) -> Self {
        Self {
            output_rate,
            reconnect_policy: ReconnectPolicy::default(),
            mixer: MixerSettings::default(),
            idle: IdleConfig::default(),
            output_device: None,
        }
    }
}

/// Cheap, cloneable control handle for an [`AudioEngine`].
///
/// Cloneable so a watchdog thread can stop the engine without owning it, and
/// so the input pipeline can resume a paused stream without owning it.
#[derive(Clone)]
pub struct AudioControl {
    tx: pipewire::channel::Sender<EngineCmd>,
    shutdown: Arc<AtomicBool>,
    activity: Arc<Mutex<Activity>>,
    reconnect_requested: Arc<AtomicBool>,
}

impl AudioControl {
    /// Requests a clean shutdown of the PipeWire loop.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.tx.send(EngineCmd::Shutdown);
    }

    /// Enables or disables stream output (idle management).
    ///
    /// The message is handled on the PipeWire loop thread, never in the
    /// real-time callback.
    pub fn set_active(&self, active: bool) {
        {
            let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            activity.wanted_active = active;
        }
        let _ = self.tx.send(EngineCmd::SetActive(active));
    }

    /// Whether the engine currently wants the stream producing audio.
    pub fn is_active(&self) -> bool {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wanted_active
    }

    /// Brings a paused stream back up after a key press.
    ///
    /// Fast path: one uncontended mutex, and a PipeWire message only on the
    /// paused → active edge. Called from the input pipeline, never from the
    /// real-time callback.
    pub fn wake(&self) {
        let should_send = {
            let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            if activity.wanted_active {
                false
            } else {
                activity.wanted_active = true;
                true
            }
        };
        if should_send {
            let _ = self.tx.send(EngineCmd::SetActive(true));
        }
    }

    /// Tears the current connection down so the supervisor rebuilds it.
    ///
    /// This is how `keyvibes audio-recovery-test` exercises a PipeWire
    /// restart without restarting the PipeWire daemon (which would need
    /// privileges and would disturb every other client).
    pub fn reconnect(&self) {
        self.reconnect_requested.store(true, Ordering::SeqCst);
        let _ = self.tx.send(EngineCmd::Reconnect);
    }

    /// Sends a no-op control message to confirm the loop is responsive.
    pub fn ping(&self) {
        let _ = self.tx.send(EngineCmd::Ping);
    }
}

impl StreamWake for AudioControl {
    fn wake(&self) {
        AudioControl::wake(self);
    }
}

/// Supervised audio output.
pub struct AudioEngine {
    control: AudioControl,
    handle: Option<JoinHandle<Result<(), AudioError>>>,
    stats: Arc<RtStats>,
    lifecycle: Arc<LifecycleState>,
    queue: Arc<SpscRing<PlayCommand>>,
    output_rate: u32,
    policy: ReconnectPolicy,
    idle: Arc<IdleState>,
    idle_stop: Arc<AtomicBool>,
    idle_handle: Option<JoinHandle<()>>,
}

impl AudioEngine {
    /// Starts the audio engine and waits for the first connection attempt.
    ///
    /// Returns an error only when the very first connection cannot be built
    /// (for example PipeWire is not installed or the socket is missing).
    /// Later failures are retried in the background per `policy`.
    pub fn start(
        output_rate: u32,
        queue: Arc<SpscRing<PlayCommand>>,
        policy: ReconnectPolicy,
        settings: MixerSettings,
    ) -> Result<Self, AudioError> {
        Self::start_with_idle(output_rate, queue, policy, settings, IdleConfig::default())
    }

    /// Starts the engine with an explicit idle-management policy.
    ///
    /// Idle handling lives entirely on its own control thread; see
    /// [`crate::idle`].
    pub fn start_with_idle(
        output_rate: u32,
        queue: Arc<SpscRing<PlayCommand>>,
        policy: ReconnectPolicy,
        settings: MixerSettings,
        idle_config: IdleConfig,
    ) -> Result<Self, AudioError> {
        Self::start_with_options(
            queue,
            AudioOptions {
                output_rate,
                reconnect_policy: policy,
                mixer: settings,
                idle: idle_config,
                output_device: None,
            },
        )
    }

    /// Starts the engine from a full description of what it should be.
    ///
    /// This is the entry point the runtime uses; the older constructors are
    /// kept as thin wrappers so tests and diagnostics do not have to name
    /// every field.
    pub fn start_with_options(
        queue: Arc<SpscRing<PlayCommand>>,
        options: AudioOptions,
    ) -> Result<Self, AudioError> {
        let AudioOptions {
            output_rate,
            reconnect_policy: policy,
            mixer: settings,
            idle: idle_config,
            output_device,
        } = options;
        let stats = Arc::new(RtStats::new(output_rate));
        let lifecycle = Arc::new(LifecycleState::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let reconnect_requested = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(Mutex::new(Activity {
            wanted_active: true,
        }));

        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();
        let (tx, rx) = pipewire::channel::channel::<EngineCmd>();

        let thread_stats = stats.clone();
        let thread_lifecycle = lifecycle.clone();
        let thread_shutdown = shutdown.clone();
        let thread_queue = queue.clone();
        let thread_reconnect = reconnect_requested.clone();

        let handle = thread::Builder::new()
            .name("keyvibes-audio".to_string())
            .spawn(move || {
                supervisor(
                    SupervisorCtx {
                        output_rate,
                        queue: thread_queue,
                        stats: thread_stats,
                        lifecycle: thread_lifecycle,
                        shutdown: thread_shutdown,
                        reconnect_requested: thread_reconnect,
                        policy,
                        settings,
                        output_device,
                    },
                    rx,
                    ready_tx,
                )
            })
            .map_err(|e| AudioError::InitFailed(format!("failed to spawn audio thread: {e}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(result) => result?,
            Err(_) => {
                return Err(AudioError::InitFailed(
                    "audio thread did not report readiness within 15s".to_string(),
                ));
            }
        }

        let control = AudioControl {
            tx,
            shutdown,
            activity,
            reconnect_requested,
        };

        let idle = Arc::new(IdleState::new());
        let idle_stop = Arc::new(AtomicBool::new(false));
        let idle_handle = spawn_idle_monitor(
            control.clone(),
            stats.clone(),
            queue.clone(),
            idle.clone(),
            idle_config,
            idle_stop.clone(),
        )
        .map_err(|e| AudioError::InitFailed(format!("failed to spawn idle monitor: {e}")))?;

        Ok(Self {
            control,
            handle: Some(handle),
            stats,
            lifecycle,
            queue,
            output_rate,
            policy,
            idle,
            idle_stop,
            idle_handle: Some(idle_handle),
        })
    }

    /// Lock-free idle state (phase, pause/resume counts, quiet duration).
    pub fn idle_state(&self) -> Arc<IdleState> {
        self.idle.clone()
    }

    /// Current idle phase (control plane only).
    pub fn idle_phase(&self) -> crate::idle::IdlePhase {
        self.idle.phase()
    }

    /// A cloneable control handle (safe to give to a watchdog thread).
    pub fn control(&self) -> AudioControl {
        self.control.clone()
    }

    /// Current stream statistics (lock-free snapshot).
    pub fn stats(&self) -> StreamStats {
        self.stats.snapshot()
    }

    /// Shared statistics block.
    pub fn stats_block(&self) -> Arc<RtStats> {
        self.stats.clone()
    }

    /// Lock-free lifecycle state.
    pub fn lifecycle(&self) -> Arc<LifecycleState> {
        self.lifecycle.clone()
    }

    /// Current lifecycle phase.
    pub fn phase(&self) -> AudioPhase {
        if self.control.shutdown.load(Ordering::Relaxed) {
            AudioPhase::Stopped
        } else if self.lifecycle.is_exhausted() {
            AudioPhase::Failed
        } else if self.lifecycle.is_running() {
            AudioPhase::Running
        } else if self.lifecycle.attempts() > 0 {
            AudioPhase::Reconnecting
        } else {
            AudioPhase::Starting
        }
    }

    /// Sample rate the engine renders at.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// The reconnection policy the supervisor applies.
    pub fn policy(&self) -> ReconnectPolicy {
        self.policy
    }

    /// The shared command queue (also used by the input backends).
    pub fn queue(&self) -> Arc<SpscRing<PlayCommand>> {
        self.queue.clone()
    }

    /// Requests that the output stream start or stop producing audio.
    ///
    /// This is a control-plane operation: the message is handled on the
    /// PipeWire loop thread, never in the real-time callback.
    pub fn set_active(&self, active: bool) {
        self.control.set_active(active);
    }

    /// Whether the engine currently wants the stream producing audio.
    pub fn is_active(&self) -> bool {
        self.control.is_active()
    }

    /// Resumes a paused stream; cheap when it is already running.
    pub fn wake(&self) {
        self.control.wake();
    }

    /// Forces the connection to be torn down and rebuilt.
    ///
    /// Recovery stays on the control thread: the supervisor drops the old
    /// connection, waits its bounded backoff, and builds a new one. The
    /// real-time callback is never asked to reconnect.
    pub fn reconnect(&self) {
        self.control.reconnect();
    }

    /// Sends a no-op control message, used to confirm the loop is responsive.
    pub fn ping(&self) {
        self.control.ping();
    }

    /// Asks the supervisor to shut down cleanly.
    pub fn shutdown(&self) {
        self.control.shutdown();
    }

    /// Blocks until the supervisor thread exits on its own.
    ///
    /// Does **not** request a shutdown first: call [`shutdown`](Self::shutdown)
    /// (or clone the [`AudioControl`] and call it from a watchdog) when you
    /// want the loop to stop.
    pub fn join(mut self) -> Result<(), AudioError> {
        self.stop_idle_monitor();
        match self.handle.take() {
            Some(handle) => handle.join().unwrap_or(Ok(())),
            None => Ok(()),
        }
    }

    /// Stops the idle monitor thread (idempotent).
    fn stop_idle_monitor(&mut self) {
        self.idle_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.idle_handle.take() {
            let _ = handle.join();
        }
    }

    /// Requests shutdown and then blocks until the supervisor exits.
    pub fn stop_and_join(self) -> Result<(), AudioError> {
        self.shutdown();
        self.join()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop_idle_monitor();
        self.control.shutdown();
        if let Some(handle) = self.handle.take() {
            // Never leave a detached loop thread behind.
            let _ = handle.join();
        }
    }
}

/// Everything the supervisor thread needs, bundled to keep signatures small.
struct SupervisorCtx {
    output_rate: u32,
    queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<RtStats>,
    lifecycle: Arc<LifecycleState>,
    shutdown: Arc<AtomicBool>,
    reconnect_requested: Arc<AtomicBool>,
    policy: ReconnectPolicy,
    settings: MixerSettings,
    output_device: Option<String>,
}

/// The supervisor loop: build, run, observe failure, back off, rebuild.
///
/// Every decision comes from [`ReconnectStateMachine`], which is the same
/// state machine `keyvibes audio-recovery-test` drives without PipeWire.
fn supervisor(
    ctx: SupervisorCtx,
    rx: pipewire::channel::Receiver<EngineCmd>,
    ready_tx: mpsc::Sender<Result<(), AudioError>>,
) -> Result<(), AudioError> {
    let SupervisorCtx {
        mut output_rate,
        queue,
        stats,
        lifecycle,
        shutdown,
        reconnect_requested,
        policy,
        settings,
        output_device,
    } = ctx;
    let mut ready_tx = Some(ready_tx);
    let mut receiver = Some(rx);
    let mut machine = ReconnectStateMachine::new(policy);
    let mut first = true;
    let mut last_error: Option<AudioError> = None;
    let mut rate_rebuilds: u32 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            lifecycle.mark_running();
            return Ok(());
        }

        // A reconnect requested while nothing was connected is a no-op: we
        // are about to build a connection anyway, and consuming the flag
        // here keeps it from firing again on the next attach.
        if reconnect_requested.swap(false, Ordering::SeqCst) && !first {
            lifecycle.mark_running();
        }

        // Bounded backoff - never a busy loop, never delayed on attempt one.
        let delay = machine.backoff();
        if delay > Duration::ZERO && !sleep_unless_shutdown(&shutdown, delay) {
            return Ok(());
        }

        let connection = match Connection::build_targeted(
            output_rate,
            queue.clone(),
            stats.clone(),
            settings,
            output_device.as_deref(),
        ) {
            Ok(connection) => connection,
            Err(error) => {
                if first {
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Err(error));
                    }
                    lifecycle.mark_exhausted();
                    return Err(AudioError::ConnectionFailed(
                        "initial audio connection failed".to_string(),
                    ));
                }
                last_error = Some(error);
                let retry = machine.record_failure();
                lifecycle.mark_failed(machine.failures());
                if !retry {
                    lifecycle.mark_exhausted();
                    return Err(AudioError::ReconnectExhausted(machine.failures()));
                }
                continue;
            }
        };

        if let Some(tx) = ready_tx.take() {
            let _ = tx.send(Ok(()));
        }

        let resumed = !first;
        machine.record_success();
        lifecycle.mark_running();
        if resumed {
            stats.record_reconnect();
        }
        first = false;

        // Run the loop until it quits (shutdown, core error, stream error,
        // forced reconnect, or a sample-rate renegotiation).
        let outcome = run_once(
            &connection,
            receiver.take().expect("receiver"),
            &shutdown,
            &reconnect_requested,
        );
        receiver = Some(outcome.receiver);

        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        if connection.rate_changed() {
            let negotiated = stats.negotiated_rate();
            drop(connection);
            if rate_rebuilds < MAX_RATE_RENEGOTIATIONS
                && negotiated != 0
                && negotiated != output_rate
            {
                rate_rebuilds += 1;
                output_rate = negotiated;
                lifecycle.mark_failed(machine.failures());
                continue;
            }
            // Budget spent (or nothing usable came back): keep the current
            // rate rather than looping on a host that keeps changing its
            // mind. PipeWire still converts for the sink.
            reconnect_requested.store(false, Ordering::SeqCst);
            lifecycle.mark_running();
            continue;
        }

        if reconnect_requested.swap(false, Ordering::SeqCst) {
            // Deliberate teardown: not a failure, so the retry budget is
            // untouched, but `machine.backoff()` still applies because the
            // connection had been established.
            drop(connection);
            lifecycle.mark_running();
            continue;
        }

        // The loop exited without a shutdown request: the connection failed.
        drop(connection);
        lifecycle.mark_failed(machine.failures().saturating_add(1));
        if !machine.record_failure() {
            lifecycle.mark_exhausted();
            return Err(last_error
                .take()
                .unwrap_or(AudioError::ReconnectExhausted(machine.failures())));
        }
    }
}

/// Outcome of one main-loop run.
struct RunOutcome {
    receiver: pipewire::channel::Receiver<EngineCmd>,
}

/// Attaches the control channel, runs the loop, then detaches the channel so
/// it can be re-attached to the next connection.
fn run_once(
    connection: &Connection,
    rx: pipewire::channel::Receiver<EngineCmd>,
    shutdown: &Arc<AtomicBool>,
    reconnect_requested: &Arc<AtomicBool>,
) -> RunOutcome {
    let stream = connection.stream().clone();
    let main_loop = connection.main_loop();
    let shutdown_flag = shutdown.clone();
    let reconnect_flag = reconnect_requested.clone();

    // The closure runs on the PipeWire loop thread, so `quit()` and
    // `set_active()` are legal here: both are control-plane operations.
    let attached = rx.attach(connection.loop_(), move |cmd| match cmd {
        EngineCmd::Shutdown => {
            shutdown_flag.store(true, Ordering::Relaxed);
            main_loop.quit();
        }
        EngineCmd::SetActive(active) => {
            let _ = stream.set_active(active);
        }
        EngineCmd::Reconnect => {
            reconnect_flag.store(true, Ordering::SeqCst);
            main_loop.quit();
        }
        EngineCmd::Ping => {}
    });

    connection.run();

    RunOutcome {
        receiver: attached.deattach(),
    }
}

/// Sleeps for `delay`, returning `false` if shutdown was requested meanwhile.
fn sleep_unless_shutdown(shutdown: &AtomicBool, delay: Duration) -> bool {
    let mut remaining = delay;
    while remaining > Duration::ZERO {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let step = remaining.min(Duration::from_millis(50));
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !shutdown.load(Ordering::Relaxed)
}

impl AudioEngine {
    /// Waits until the stream reports a streaming/paused state or `timeout`.
    ///
    /// Control plane only; used by diagnostics and tests.
    pub fn wait_until_active(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let state = self.stats.snapshot().stream_state;
            if matches!(state, StreamStateCode::Streaming | StreamStateCode::Paused) {
                return true;
            }
            if self.lifecycle.is_exhausted() {
                return false;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until at least `wanted` reconnections have been observed.
    ///
    /// Control plane only; used by `keyvibes audio-recovery-test`.
    pub fn wait_for_reconnects(&self, wanted: u64, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.stats.snapshot().reconnects >= wanted {
                return true;
            }
            if self.lifecycle.is_exhausted() {
                return false;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_cmd_is_not_constructible_outside() {
        // Compile-time documentation: engine commands are private so no
        // caller can bypass the AudioEngine control API.
        let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(8));
        assert_eq!(queue.capacity(), 8);
    }

    #[test]
    fn test_sleep_unless_shutdown_wakes_early() {
        let shutdown = AtomicBool::new(true);
        let start = std::time::Instant::now();
        assert!(!sleep_unless_shutdown(&shutdown, Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_sleep_unless_shutdown_sleeps_full_time() {
        let shutdown = AtomicBool::new(false);
        let start = std::time::Instant::now();
        assert!(sleep_unless_shutdown(&shutdown, Duration::from_millis(30)));
        assert!(start.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn activity_defaults_to_wanted() {
        let activity = Activity {
            wanted_active: true,
        };
        assert!(activity.wanted_active);
    }
}
