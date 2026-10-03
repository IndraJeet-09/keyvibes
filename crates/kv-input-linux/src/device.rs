//! Individual keyboard device management.
//!
//! Each KeyboardDevice manages one physical keyboard, maintaining its
//! state and reading events from its evdev file descriptor.

use crate::diagnostics::InputStats;
use crate::error::InputError;
use crate::events::{process_event, EventResult};
use evdev::Device;
use kv_core::PlayCommand;
use kv_ring::SpscRing;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

/// Represents a single keyboard device.
pub struct KeyboardDevice {
    pub path: PathBuf,
    pub name: String,
    device: Device,
    stats: Arc<InputStats>,
}

impl KeyboardDevice {
    /// Opens a keyboard device.
    pub fn open(path: PathBuf, stats: Arc<InputStats>) -> Result<Self, InputError> {
        let device = Device::open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                InputError::PermissionDenied(format!("{}: {}", path.display(), e))
            } else {
                InputError::DeviceOpenFailed(format!("{}: {}", path.display(), e))
            }
        })?;

        let name = device.name().unwrap_or("Unknown").to_string();

        Ok(Self {
            path,
            name,
            device,
            stats,
        })
    }

    /// Spawns a reader thread for this keyboard.
    ///
    /// The thread reads events, converts them to PlayCommands, and pushes
    /// them to the queue. The thread exits when the device is disconnected
    /// or an unrecoverable error occurs.
    pub fn spawn_reader(
        mut self,
        queue: Arc<SpscRing<PlayCommand>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            loop {
                // Fetch events from the device
                match self.device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            self.handle_event(&event, &queue);
                        }
                    }
                    Err(e) => {
                        // Device disconnected or error
                        if e.kind() == std::io::ErrorKind::Other
                            || e.raw_os_error() == Some(libc::ENODEV)
                        {
                            // Device removed
                            self.stats.increment_device_removed();
                            break;
                        } else {
                            eprintln!("Error reading from {}: {}", self.name, e);
                            self.stats.increment_device_removed();
                            break;
                        }
                    }
                }
            }
        })
    }

    /// Handles a single input event.
    fn handle_event(&self, event: &evdev::InputEvent, queue: &Arc<SpscRing<PlayCommand>>) {
        let result = process_event(event);

        match result {
            EventResult::Press(physical_key) => {
                self.stats.increment_press();

                // Create PlayCommand - stub for now since we don't have sound packs yet
                // In Phase 4, this will load actual samples
                let cmd = unsafe {
                    // Dummy values - will be replaced in Phase 4
                    PlayCommand::new(
                        std::ptr::null(), // No sample data yet
                        0,                // No length
                        48000,            // Sample rate
                        1u64 << 32,       // 1.0 playback rate
                        0.5,              // Left gain
                        0.5,              // Right gain
                        false,            // Not a release
                    )
                };

                // Try to push to queue
                if queue.push(cmd).is_err() {
                    self.stats.increment_command_dropped();
                } else {
                    self.stats.increment_command_generated();
                }
            }
            EventResult::Release(physical_key) => {
                self.stats.increment_release();
                // Release handling can be added later if needed
            }
            EventResult::Repeat => {
                self.stats.increment_repeat_ignored();
            }
            EventResult::Unknown => {
                self.stats.increment_unknown();
            }
            EventResult::Dropped => {
                self.stats.increment_dropped();
                // After SYN_DROPPED, device state is out of sync
                // The evdev crate handles resynchronization automatically
            }
            EventResult::Sync | EventResult::Other => {
                // Normal synchronization events, no action needed
            }
        }
    }
}
