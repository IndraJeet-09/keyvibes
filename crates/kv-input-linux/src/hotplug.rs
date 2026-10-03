//! Hotplug detection for keyboard devices.
//!
//! Monitors /dev/input for new and removed keyboard devices.

use crate::discovery::{discover_keyboards, KeyboardInfo};
use crate::error::InputError;
use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Hotplug event indicating a device was added or removed.
#[derive(Debug, Clone)]
pub enum HotplugEvent {
    Added(KeyboardInfo),
    Removed(PathBuf),
}

/// Monitors for keyboard hotplug events.
pub struct HotplugMonitor {
    known_devices: HashSet<PathBuf>,
    poll_interval: Duration,
}

impl HotplugMonitor {
    /// Creates a new hotplug monitor.
    ///
    /// # Arguments
    ///
    /// * `poll_interval` - How often to check for device changes.
    ///   Default: 1 second (reasonable fallback when udev monitoring unavailable).
    pub fn new(poll_interval: Duration) -> Self {
        Self {
            known_devices: HashSet::new(),
            poll_interval,
        }
    }

    /// Initializes the monitor with currently connected keyboards.
    pub fn initialize(&mut self) -> Result<Vec<KeyboardInfo>, InputError> {
        let keyboards = discover_keyboards()?;
        self.known_devices = keyboards.iter().map(|k| k.path.clone()).collect();
        Ok(keyboards)
    }

    /// Checks for device changes and returns any hotplug events.
    pub fn check_for_changes(&mut self) -> Result<Vec<HotplugEvent>, InputError> {
        let current_keyboards = discover_keyboards()?;
        let current_paths: HashSet<PathBuf> =
            current_keyboards.iter().map(|k| k.path.clone()).collect();

        let mut events = Vec::new();

        // Detect added devices
        for keyboard in &current_keyboards {
            if !self.known_devices.contains(&keyboard.path) {
                events.push(HotplugEvent::Added(keyboard.clone()));
            }
        }

        // Detect removed devices
        for path in &self.known_devices {
            if !current_paths.contains(path) {
                events.push(HotplugEvent::Removed(path.clone()));
            }
        }

        // Update known devices
        self.known_devices = current_paths;

        Ok(events)
    }

    /// Spawns a background thread that monitors for hotplug events.
    ///
    /// The callback is invoked for each hotplug event.
    pub fn spawn_monitor<F>(mut self, mut callback: F) -> thread::JoinHandle<()>
    where
        F: FnMut(HotplugEvent) + Send + 'static,
    {
        thread::spawn(move || loop {
            match self.check_for_changes() {
                Ok(events) => {
                    for event in events {
                        callback(event);
                    }
                }
                Err(e) => {
                    eprintln!("Hotplug check failed: {}", e);
                }
            }

            thread::sleep(self.poll_interval);
        })
    }
}

impl Default for HotplugMonitor {
    fn default() -> Self {
        Self::new(Duration::from_secs(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotplug_monitor_creation() {
        let monitor = HotplugMonitor::new(Duration::from_secs(1));
        assert_eq!(monitor.known_devices.len(), 0);
    }

    #[test]
    fn test_initialize() {
        let mut monitor = HotplugMonitor::default();
        // This will succeed or fail depending on the test environment
        match monitor.initialize() {
            Ok(keyboards) => {
                assert_eq!(monitor.known_devices.len(), keyboards.len());
            }
            Err(_) => {
                // Expected in some test environments
            }
        }
    }
}
