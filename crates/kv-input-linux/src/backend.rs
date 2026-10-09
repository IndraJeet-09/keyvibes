//! Linux input backend coordinator.
//!
//! Owns exactly two threads:
//!
//! * the **pipeline**, which reads every keyboard through `poll` and is the
//!   single producer into the audio command queue,
//! * the **hotplug monitor**, which tells the pipeline about devices that
//!   appeared or vanished.
//!
//! There is deliberately no reader thread per keyboard. `SpscRing` is
//! single-producer, so a thread-per-device design would let several threads
//! race on the same `push`; multiplexing descriptors in one thread removes
//! the violation and makes attach/detach a map operation instead of a thread
//! lifecycle.

use crate::diagnostics::InputStats;
use crate::discovery::discover_keyboards;
use crate::error::InputError;
use crate::hotplug::{HotplugEvent, HotplugMonitor};
use crate::pipeline::{InputPipeline, PipelineCommand};
use kv_core::{PlayCommand, SoundSource, StreamWake};
use kv_ring::SpscRing;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// The Linux input backend.
pub struct LinuxInputBackend {
    pipeline: InputPipeline,
    decisions: Arc<Mutex<HashSet<PathBuf>>>,
    monitor_stop: Arc<AtomicBool>,
    monitor_handle: Option<thread::JoinHandle<()>>,
    stats: Arc<InputStats>,
}

impl LinuxInputBackend {
    /// Creates a new Linux input backend and attaches every keyboard found.
    ///
    /// # Arguments
    ///
    /// * `command_queue` - lock-free queue for `PlayCommand`s; this backend
    ///   is its only producer
    /// * `stats` - statistics sink
    /// * `source` - shared sound source used to build commands
    /// * `wake` - optional hook that resumes a paused output stream just
    ///   before a command is queued (idle power management)
    ///
    /// Zero readable keyboards is a valid state: the backend still comes up
    /// so the hotplug monitor can attach a keyboard when one appears.
    ///
    /// # Errors
    ///
    /// Returns `InputError` only when device enumeration itself fails. An
    /// individual keyboard that cannot be opened is skipped and logged.
    pub fn new<S>(
        command_queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<InputStats>,
        source: Arc<S>,
        wake: Option<Arc<dyn StreamWake>>,
    ) -> Result<Self, InputError>
    where
        S: SoundSource + Send + Sync + 'static,
    {
        // Enumeration failure is fatal; an empty list is not.
        let keyboards = discover_keyboards()?;
        let decisions = Arc::new(Mutex::new(HashSet::new()));

        let pipeline = InputPipeline::start(
            command_queue,
            stats.clone(),
            source,
            keyboards,
            decisions.clone(),
            wake,
        )?;

        Ok(Self {
            pipeline,
            decisions,
            monitor_stop: Arc::new(AtomicBool::new(false)),
            monitor_handle: None,
            stats,
        })
    }

    /// Starts watching for keyboards appearing and disappearing.
    ///
    /// Best effort: losing hotplug must not stop playback, so a failure here
    /// leaves the already-attached keyboards working.
    pub fn enable_hotplug(&mut self) -> Result<(), InputError> {
        if self.monitor_handle.is_some() {
            return Ok(());
        }

        let monitor = HotplugMonitor::new(Duration::from_secs(1), self.decisions.clone());
        let commands = self.pipeline.command_port();
        let handle = monitor.spawn_monitor(self.monitor_stop.clone(), move |event| match event {
            HotplugEvent::Added(info) => commands.send(PipelineCommand::Attach(info)),
            HotplugEvent::Removed(path) => commands.send(PipelineCommand::Detach(path)),
        });

        self.monitor_handle = Some(handle);
        Ok(())
    }

    /// Number of keyboards the pipeline currently has attached.
    pub fn device_count(&self) -> usize {
        self.pipeline.attached_count()
    }

    /// Whether the pipeline thread is still running.
    pub fn is_running(&self) -> bool {
        self.pipeline.is_running()
    }

    /// Gets a statistics snapshot.
    pub fn get_stats(&self) -> crate::diagnostics::InputStatsSnapshot {
        self.stats.snapshot()
    }
}

impl Drop for LinuxInputBackend {
    fn drop(&mut self) {
        self.monitor_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.monitor_handle.take() {
            let _ = handle.join();
        }
        // `pipeline` stops itself when its own `Drop` runs, immediately after
        // this body: it wakes within one poll timeout and exits cleanly.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_core::{PhysicalKey, VariantState};

    /// Sound source that never produces a command (for headless tests).
    struct SilentSource;

    impl SoundSource for SilentSource {
        fn play(&self, _key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
            None
        }
    }

    #[test]
    fn test_backend_creation_allows_zero_keyboards() {
        let queue = Arc::new(SpscRing::with_capacity(256));
        let stats = Arc::new(InputStats::new());
        let source = Arc::new(SilentSource);

        // Enumeration failure is the only error; an empty system is fine
        // because the hotplug monitor will attach keyboards later.
        match LinuxInputBackend::new(queue, stats, source, None) {
            Ok(backend) => {
                println!("keyboards visible: {}", backend.device_count());
                // Dropping must tear the threads down without hanging.
                drop(backend);
            }
            Err(InputError::NoKeyboardsFound) => {
                panic!("NoKeyboardsFound must no longer be returned by new()");
            }
            Err(e) => {
                // Enumeration can legitimately fail in sandboxed test runners.
                println!("enumeration failed (expected in sandbox): {e}");
            }
        }
    }
}
