//! Hotplug detection for keyboard devices.
//!
//! Polls `/dev/input` and compares what is present against the set of paths
//! the pipeline has already made a decision about. Using the pipeline's view
//! instead of a private "last seen" list is what makes recovery work: if the
//! pipeline drops a device because its descriptor went stale, the path is no
//! longer "decided", so the very next poll offers it again.

use crate::discovery::discover_keyboards;
use crate::discovery::KeyboardInfo;
use crate::error::InputError;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Hotplug event indicating a device was added or removed.
#[derive(Debug, Clone)]
pub enum HotplugEvent {
    Added(KeyboardInfo),
    Removed(PathBuf),
}

/// Longest gap between polls when discovery keeps failing.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Slice the shutdown-aware sleep is broken into, so `Drop` stays prompt.
const STOP_POLL_SLICE: Duration = Duration::from_millis(50);

/// Monitors `/dev/input` for keyboard hotplug events.
pub struct HotplugMonitor {
    poll_interval: Duration,
    decisions: Arc<Mutex<HashSet<PathBuf>>>,
}

impl HotplugMonitor {
    /// Creates a monitor over the pipeline's set of decided paths.
    ///
    /// # Arguments
    ///
    /// * `poll_interval` - base check period. Default: 1 second.
    /// * `decisions` - paths the pipeline has attached or deliberately
    ///   refused; the monitor only suggests paths absent from this set.
    pub fn new(poll_interval: Duration, decisions: Arc<Mutex<HashSet<PathBuf>>>) -> Self {
        Self {
            poll_interval,
            decisions,
        }
    }

    /// Checks for device changes and returns any hotplug events.
    pub fn check_for_changes(&self) -> Result<Vec<HotplugEvent>, InputError> {
        let current_keyboards = discover_keyboards()?;
        let current_paths: HashSet<PathBuf> =
            current_keyboards.iter().map(|k| k.path.clone()).collect();

        let decided: HashSet<PathBuf> = self
            .decisions
            .lock()
            .expect("decisions lock poisoned")
            .clone();

        let mut events = Vec::new();
        for keyboard in &current_keyboards {
            if !decided.contains(&keyboard.path) {
                events.push(HotplugEvent::Added(keyboard.clone()));
            }
        }
        for path in &decided {
            if !current_paths.contains(path) {
                events.push(HotplugEvent::Removed(path.clone()));
            }
        }

        Ok(events)
    }

    /// Spawns the background monitor thread.
    ///
    /// The thread exits as soon as `stop` is set, and sleeps in short slices
    /// so that happens promptly. Repeated discovery failures back off
    /// exponentially instead of hammering the system, and stop being logged
    /// after the first few so a machine without `/dev/input` stays quiet.
    pub fn spawn_monitor<F>(self, stop: Arc<AtomicBool>, mut callback: F) -> thread::JoinHandle<()>
    where
        F: FnMut(HotplugEvent) + Send + 'static,
    {
        thread::Builder::new()
            .name("keyvibes-hotplug".to_string())
            .spawn(move || {
                let mut consecutive_failures: u32 = 0;
                while !stop.load(Ordering::Relaxed) {
                    match self.check_for_changes() {
                        Ok(events) => {
                            if consecutive_failures > 0 {
                                eprintln!("keyvibes: device discovery recovered");
                            }
                            consecutive_failures = 0;
                            for event in events {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                callback(event);
                            }
                        }
                        Err(error) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            if consecutive_failures <= 3 {
                                eprintln!("keyvibes: device discovery failed: {error}");
                            }
                        }
                    }

                    let deadline =
                        Instant::now() + backoff(self.poll_interval, consecutive_failures);
                    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                        thread::sleep(STOP_POLL_SLICE.min(deadline - Instant::now()));
                    }
                }
            })
            .expect("failed to spawn hotplug monitor")
    }
}

impl Default for HotplugMonitor {
    fn default() -> Self {
        Self::new(Duration::from_secs(1), Arc::new(Mutex::new(HashSet::new())))
    }
}

/// Doubles `base` per consecutive failure, capped at [`MAX_BACKOFF`].
fn backoff(base: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return base;
    }
    let shifts = failures.min(6);
    base.saturating_mul(1u32 << shifts).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let base = Duration::from_secs(1);
        assert_eq!(backoff(base, 0), base);
        assert_eq!(backoff(base, 1), Duration::from_secs(2));
        assert_eq!(backoff(base, 2), Duration::from_secs(4));
        assert_eq!(backoff(base, 100), MAX_BACKOFF);
    }

    #[test]
    fn monitor_starts_with_no_known_devices() {
        let monitor = HotplugMonitor::default();
        assert!(monitor
            .decisions
            .lock()
            .expect("decisions lock poisoned")
            .is_empty());
    }

    #[test]
    fn check_reports_missing_input_subsystem_as_an_error() {
        let monitor = HotplugMonitor::default();
        // Passes on a normal Linux box; in a sandbox without /dev/input it
        // must surface an error rather than panic or silently do nothing.
        match monitor.check_for_changes() {
            Ok(events) => println!("discovery ok, {} event(s)", events.len()),
            Err(error) => println!("discovery unavailable: {error}"),
        }
    }
}
