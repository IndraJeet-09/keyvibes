//! Individual keyboard device management.
//!
//! Each KeyboardDevice manages one physical keyboard, maintaining its
//! state and reading events from its evdev file descriptor.

use crate::diagnostics::InputStats;
use crate::error::InputError;
use crate::events::{process_event, EventResult};
use evdev::Device;
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
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
    /// The thread reads events, converts key presses into `PlayCommand`s via
    /// the shared [`SoundSource`], and pushes them to the queue. The thread
    /// exits when the device is disconnected or an unrecoverable error occurs.
    ///
    /// Each reader thread owns its own variant-rotation state, so no shared
    /// mutable state or locks are needed on the input path.
    pub fn spawn_reader<S>(
        mut self,
        queue: Arc<SpscRing<PlayCommand>>,
        source: Arc<S>,
    ) -> thread::JoinHandle<()>
    where
        S: SoundSource + Send + Sync + 'static,
    {
        thread::spawn(move || {
            // One rotation slot per physical key; owned by this thread.
            let mut variant_states: [VariantState; PhysicalKey::COUNT] =
                std::array::from_fn(|_| VariantState::default());

            loop {
                // Fetch events from the device
                match self.device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            Self::handle_event(
                                &self.stats,
                                &event,
                                &queue,
                                source.as_ref(),
                                &mut variant_states,
                            );
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
    fn handle_event<S: SoundSource + ?Sized>(
        stats: &Arc<InputStats>,
        event: &evdev::InputEvent,
        queue: &Arc<SpscRing<PlayCommand>>,
        source: &S,
        variant_states: &mut [VariantState; PhysicalKey::COUNT],
    ) {
        match process_event(event) {
            EventResult::Press(physical_key) => {
                stats.increment_press();

                // Ask the sound source (pack player) for this key's clip.
                // `None` means the loaded pack has no sound for this key.
                let state = &mut variant_states[physical_key.as_u16() as usize];
                if let Some(cmd) = source.play(physical_key, state) {
                    if queue.push(cmd).is_err() {
                        stats.increment_command_dropped();
                    } else {
                        stats.increment_command_generated();
                    }
                }
            }
            EventResult::Release(physical_key) => {
                stats.increment_release();
                // One-shot packs have no release sounds (Phase 5+).
                let _ = physical_key;
            }
            EventResult::Repeat => {
                stats.increment_repeat_ignored();
            }
            EventResult::Unknown => {
                stats.increment_unknown();
            }
            EventResult::Dropped => {
                stats.increment_dropped();
                // After SYN_DROPPED, device state is out of sync
                // The evdev crate handles resynchronization automatically
            }
            EventResult::Sync | EventResult::Other => {
                // Normal synchronization events, no action needed
            }
        }
    }
}
