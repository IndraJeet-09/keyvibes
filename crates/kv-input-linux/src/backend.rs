//! Linux input backend coordinator.
//!
//! Manages keyboard discovery, hotplug, and the central input pipeline.

use crate::device::KeyboardDevice;
use crate::diagnostics::InputStats;
use crate::discovery::discover_keyboards;
use crate::error::InputError;
use crate::hotplug::{HotplugEvent, HotplugMonitor};
use kv_core::{PlayCommand, SoundSource};
use kv_ring::SpscRing;
use std::sync::Arc;
use std::thread;

/// The Linux input backend.
///
/// Manages:
/// - Keyboard discovery (no hardcoded /dev/input/event0)
/// - Multiple simultaneous keyboards
/// - Hotplug detection
/// - Event-to-PlayCommand conversion via the shared [`SoundSource`]
pub struct LinuxInputBackend {
    command_queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<InputStats>,
    device_handles: Vec<thread::JoinHandle<()>>,
    hotplug_handle: Option<thread::JoinHandle<()>>,
}

impl LinuxInputBackend {
    /// Creates a new Linux input backend.
    ///
    /// # Arguments
    ///
    /// * `command_queue` - Lock-free queue for PlayCommands
    /// * `stats` - Statistics sink
    /// * `source` - Shared sound source (pack player) used to build commands
    ///
    /// # Errors
    ///
    /// Returns `InputError` if no keyboards are found or initialization fails.
    pub fn new<S>(
        command_queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<InputStats>,
        source: Arc<S>,
    ) -> Result<Self, InputError>
    where
        S: SoundSource + Send + Sync + 'static,
    {
        // Discover initial keyboards
        let keyboards = discover_keyboards()?;

        if keyboards.is_empty() {
            return Err(InputError::NoKeyboardsFound);
        }

        let mut device_handles = Vec::new();

        // Open and spawn readers for each keyboard
        for keyboard_info in keyboards {
            match KeyboardDevice::open(keyboard_info.path, stats.clone()) {
                Ok(device) => {
                    let handle = device.spawn_reader(command_queue.clone(), source.clone());
                    device_handles.push(handle);
                    stats.increment_device_added();
                }
                Err(e) => {
                    eprintln!("Failed to open keyboard: {}", e);
                }
            }
        }

        Ok(Self {
            command_queue,
            stats,
            device_handles,
            hotplug_handle: None,
        })
    }

    /// Enables hotplug detection.
    pub fn enable_hotplug<S>(&mut self, source: Arc<S>) -> Result<(), InputError>
    where
        S: SoundSource + Send + Sync + 'static,
    {
        let queue = self.command_queue.clone();
        let stats = self.stats.clone();

        let mut monitor = HotplugMonitor::default();
        monitor.initialize()?;

        let handle = monitor.spawn_monitor(move |event| {
            match event {
                HotplugEvent::Added(info) => {
                    eprintln!("Keyboard added: {} ({})", info.name, info.path.display());
                    match KeyboardDevice::open(info.path, stats.clone()) {
                        Ok(device) => {
                            let _handle = device.spawn_reader(queue.clone(), source.clone());
                            stats.increment_device_added();
                            // Note: Handle is dropped, which stops the thread
                            // In production, we'd track these handles
                        }
                        Err(e) => {
                            eprintln!("Failed to open new keyboard: {}", e);
                        }
                    }
                }
                HotplugEvent::Removed(path) => {
                    eprintln!("Keyboard removed: {}", path.display());
                    stats.increment_device_removed();
                }
            }
        });

        self.hotplug_handle = Some(handle);
        Ok(())
    }

    /// Number of keyboard reader threads currently tracked.
    pub fn device_count(&self) -> usize {
        self.device_handles.len()
    }

    /// Gets statistics snapshot.
    pub fn get_stats(&self) -> crate::diagnostics::InputStatsSnapshot {
        self.stats.snapshot()
    }
}

impl Drop for LinuxInputBackend {
    fn drop(&mut self) {
        // In a real implementation, we'd signal threads to stop gracefully
        // For now, threads will be terminated when handles drop
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
    fn test_backend_creation_no_keyboards() {
        let queue = Arc::new(SpscRing::with_capacity(256));
        let stats = Arc::new(InputStats::new());
        let source = Arc::new(SilentSource);

        // This will fail if no keyboards are present (expected in most test environments)
        match LinuxInputBackend::new(queue, stats, source) {
            Ok(backend) => {
                // On a system with keyboards, this succeeds
                assert!(backend.device_count() > 0);
            }
            Err(InputError::NoKeyboardsFound) => {
                // Expected in test environments
            }
            Err(e) => {
                eprintln!("Unexpected error: {}", e);
            }
        }
    }
}
