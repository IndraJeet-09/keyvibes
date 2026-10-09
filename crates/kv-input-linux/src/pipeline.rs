//! The single-thread input pipeline.
//!
//! One thread owns every keyboard and is the **only** producer into the
//! lock-free command queue the audio callback consumes. That matters because
//! [`kv_ring::SpscRing`] is strictly single-producer: the previous design
//! spawned one reader thread per keyboard, which let several threads race on
//! the same `push`.
//!
//! The pipeline multiplexes all device descriptors with [`libc::poll`] (never
//! a busy loop), so adding or removing a keyboard means adding or removing a
//! file descriptor rather than a thread.
//!
//! What it guarantees, and what the tests in `tests/hotplug.rs` pin down:
//!
//! * keyboards appear and disappear without restarting the process,
//! * each keyboard keeps its own pressed-key and variant-rotation state,
//! * a vanished keyboard is detached instead of crashing the pipeline,
//! * `SYN_DROPPED` resets only the affected keyboard's pressed state,
//! * transient read errors are retried a bounded number of times, then the
//!   device is dropped.

use crate::diagnostics::InputStats;
use crate::discovery::KeyboardInfo;
use crate::events::{process_event, EventResult};
use evdev::{Device, InputEvent};
use kv_core::{PhysicalKey, PlayCommand, SoundSource, StreamWake, VariantState};
use kv_ring::SpscRing;
use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How long [`libc::poll`] sleeps before the pipeline re-checks for commands.
///
/// Long enough that an idle system does no work, short enough that a command
/// sent by the hotplug monitor (attach, detach, shutdown) is picked up
/// promptly. The pipeline blocks inside `poll` for this whole window, so it
/// never spins.
pub const POLL_TIMEOUT_MS: libc::c_int = 200;

/// Transient read errors tolerated before a device is given up on.
const MAX_TRANSIENT_READ_ERRORS: u32 = 8;

/// Commands the hotplug monitor hands to the pipeline thread.
#[derive(Debug, Clone)]
pub enum PipelineCommand {
    /// A keyboard appeared (or a previous attempt should be retried).
    Attach(KeyboardInfo),
    /// A keyboard path vanished from the system.
    Detach(PathBuf),
}

/// Per-keyboard state owned entirely by the pipeline thread.
///
/// Pressed-key tracking is per device on purpose: unplugging keyboard A must
/// not clear keyboard B's held keys.
#[derive(Debug)]
pub struct DeviceState {
    /// Variant rotation for each physical key, owned by this device.
    variants: [VariantState; PhysicalKey::COUNT],
    pressed: [bool; PhysicalKey::COUNT],
    held: usize,
    read_errors: u32,
}

impl DeviceState {
    /// Creates an idle device state.
    pub fn new() -> Self {
        Self {
            variants: std::array::from_fn(|_| VariantState::default()),
            pressed: [false; PhysicalKey::COUNT],
            held: 0,
            read_errors: 0,
        }
    }

    /// Whether `key` is currently held on this device.
    pub fn is_pressed(&self, key: PhysicalKey) -> bool {
        self.pressed[key.as_u16() as usize]
    }

    /// Number of keys currently held on this device.
    pub fn held_keys(&self) -> usize {
        self.held
    }

    /// Variant rotator for `key`, used to build a play command.
    pub fn variant_mut(&mut self, key: PhysicalKey) -> &mut VariantState {
        &mut self.variants[key.as_u16() as usize]
    }

    fn set_pressed(&mut self, key: PhysicalKey, value: bool) -> bool {
        let index = key.as_u16() as usize;
        if self.pressed[index] == value {
            return false;
        }
        self.pressed[index] = value;
        self.held = if value {
            self.held + 1
        } else {
            self.held.saturating_sub(1)
        };
        true
    }

    /// Forgets every held key (device removal or `SYN_DROPPED`).
    ///
    /// Returns how many keys were released, so the caller can keep the
    /// process-wide gauge in step.
    fn forget_pressed(&mut self) -> usize {
        let released = self.held;
        self.pressed = [false; PhysicalKey::COUNT];
        self.held = 0;
        released
    }

    fn record_read_error(&mut self) -> u32 {
        self.read_errors = self.read_errors.saturating_add(1);
        self.read_errors
    }

    fn reset_read_errors(&mut self) {
        self.read_errors = 0;
    }
}

impl Default for DeviceState {
    fn default() -> Self {
        Self::new()
    }
}

/// Which keyboards are currently known to the pipeline, and how they behave.
///
/// Deliberately free of file descriptors and syscalls: the exact same state
/// machine runs against real evdev devices and against the scripted
/// lifecycle in `keyvibes hotplug-test`.
pub struct DeviceRegistry<S: ?Sized> {
    entries: HashMap<PathBuf, DeviceState>,
    held_total: usize,
    source: Arc<S>,
    queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<InputStats>,
    wake: Option<Arc<dyn StreamWake>>,
}

impl<S> DeviceRegistry<S>
where
    S: SoundSource + ?Sized,
{
    /// Creates an empty registry feeding `queue` through `source`.
    pub fn new(source: Arc<S>, queue: Arc<SpscRing<PlayCommand>>, stats: Arc<InputStats>) -> Self {
        Self {
            entries: HashMap::new(),
            held_total: 0,
            source,
            queue,
            stats,
            wake: None,
        }
    }

    /// Installs the hook that resumes a paused output stream on a key press.
    ///
    /// Called from the input thread just before a command is queued, so the
    /// first key after idle does not wait for the idle monitor's poll. Never
    /// runs on the real-time thread.
    pub fn with_wake(mut self, wake: Option<Arc<dyn StreamWake>>) -> Self {
        self.wake = wake;
        self
    }

    /// Adds a keyboard. Returns `true` when it was newly attached.
    ///
    /// Re-attaching a path that is already tracked is a no-op, which makes
    /// duplicate hotplug notifications harmless.
    pub fn attach(&mut self, info: &KeyboardInfo) -> bool {
        if self.entries.contains_key(&info.path) {
            return false;
        }
        self.entries.insert(info.path.clone(), DeviceState::new());
        self.stats.increment_device_added();
        true
    }

    /// Removes a keyboard and forgets its held keys.
    ///
    /// Returns `true` when the path was actually tracked. Other keyboards'
    /// pressed state is untouched.
    pub fn detach(&mut self, path: &Path) -> bool {
        let Some(state) = self.entries.remove(path) else {
            return false;
        };
        self.held_total = self.held_total.saturating_sub(state.held);
        self.stats.set_keys_held(self.held_total as u64);
        self.stats.increment_device_removed();
        true
    }

    /// Whether `path` is currently attached.
    pub fn contains(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    /// Number of attached keyboards.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any keyboard is attached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Attached keyboard paths, in arbitrary order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.entries.keys().cloned().collect()
    }

    /// Per-device pressed state, for diagnostics and tests.
    pub fn state(&self, path: &Path) -> Option<&DeviceState> {
        self.entries.get(path)
    }

    /// Total keys held across every attached keyboard.
    pub fn held_keys(&self) -> usize {
        self.held_total
    }

    /// Records a recoverable read error.
    ///
    /// Returns `true` once the device has failed too many times in a row and
    /// the caller should detach it.
    pub fn record_read_error(&mut self, path: &Path) -> bool {
        let Some(state) = self.entries.get_mut(path) else {
            // Already detached while this read was in flight.
            return true;
        };
        state.record_read_error() >= MAX_TRANSIENT_READ_ERRORS
    }

    /// Clears the transient error counter after a successful read.
    pub fn record_read_success(&mut self, path: &Path) {
        if let Some(state) = self.entries.get_mut(path) {
            state.reset_read_errors();
        }
    }

    /// Handles one evdev event for `path`.
    ///
    /// Events for a path that is no longer attached are dropped silently:
    /// they can arrive in the same `poll` cycle in which the device was
    /// removed, and dropping them is exactly what "never crash because one
    /// keyboard disappears" requires.
    pub fn handle_event(&mut self, path: &Path, event: &InputEvent) {
        let result = process_event(event);

        // Split the struct so the queue/source borrows do not overlap the
        // entry lookup: this function pushes onto the RT queue while mutably
        // borrowing the device state.
        let Self {
            entries,
            held_total,
            source,
            queue,
            stats,
            wake,
        } = self;

        let Some(state) = entries.get_mut(path) else {
            return;
        };

        match result {
            EventResult::Press(physical_key) => {
                if state.set_pressed(physical_key, true) {
                    *held_total += 1;
                }
                stats.increment_press();
                let started_ns = kv_core::monotonic_ns();

                let variant = state.variant_mut(physical_key);
                if let Some(cmd) = source.play(physical_key, variant) {
                    // Resume a paused stream *before* the command lands, so
                    // the callback that drains it is already scheduled.
                    if let Some(wake) = wake.as_ref() {
                        wake.wake();
                    }
                    let outcome = queue.push(cmd.stamped(kv_core::monotonic_ns()));
                    // Release the source's hold only after the push has
                    // finished: a swappable pack must stay mapped until no
                    // queued command can point into it.
                    source.queued(cmd);
                    if outcome.is_err() {
                        stats.increment_command_dropped();
                    } else {
                        stats.increment_command_generated();
                        stats.record_command_latency(kv_core::monotonic_ns() - started_ns);
                    }
                }
            }
            EventResult::Release(physical_key) => {
                if state.set_pressed(physical_key, false) {
                    *held_total = held_total.saturating_sub(1);
                }
                stats.increment_release();
            }
            EventResult::Repeat => stats.increment_repeat_ignored(),
            EventResult::Unknown => stats.increment_unknown(),
            EventResult::Sync | EventResult::Other => {}
            EventResult::Dropped => {
                // The kernel dropped events: our view of this keyboard is
                // stale. Forget it; evdev replays the true state as
                // compensating events right after, which repopulate it.
                let released = state.forget_pressed();
                *held_total = held_total.saturating_sub(released);
                stats.increment_sync_dropped();
            }
        }

        stats.set_keys_held(*held_total as u64);
    }
}

/// Why a read from a device failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFault {
    /// Nothing to read right now.
    Nothing,
    /// The device is gone.
    Removed,
    /// The device is still there but complained.
    Transient,
}

/// Classifies an error from `Device::fetch_events`.
pub fn classify_read_error(error: &io::Error) -> ReadFault {
    if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::Interrupted {
        return ReadFault::Nothing;
    }
    if matches!(
        error.raw_os_error(),
        Some(libc::ENODEV | libc::ENXIO | libc::EBADF | libc::EIO | libc::ENOENT)
    ) {
        return ReadFault::Removed;
    }
    match error.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset => ReadFault::Removed,
        _ => ReadFault::Transient,
    }
}

/// Cloneable handle used to hand commands to the pipeline thread.
///
/// Cheap to clone: it is only an `mpsc` sender, and the hotplug monitor keeps
/// one for the lifetime of the process.
#[derive(Clone)]
pub struct CommandPort {
    sender: Sender<PipelineCommand>,
}

impl CommandPort {
    /// Queues a command. Fails silently once the pipeline has shut down.
    pub fn send(&self, command: PipelineCommand) {
        let _ = self.sender.send(command);
    }
}

/// Handle to the running pipeline thread.
pub struct InputPipeline {
    port: CommandPort,
    stop: Arc<AtomicBool>,
    attached: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl InputPipeline {
    /// Opens `keyboards` and spawns the pipeline thread.
    ///
    /// Fails only when the backend cannot be built at all; an individual
    /// keyboard that cannot be opened is recorded in `decisions` and skipped
    /// so one bad device never stops the others.
    pub fn start<S>(
        queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<InputStats>,
        source: Arc<S>,
        keyboards: Vec<KeyboardInfo>,
        decisions: Arc<Mutex<HashSet<PathBuf>>>,
        wake: Option<Arc<dyn StreamWake>>,
    ) -> Result<Self, crate::error::InputError>
    where
        S: SoundSource + Send + Sync + 'static,
    {
        let mut devices = HashMap::new();
        let mut registry = DeviceRegistry::new(source, queue, stats.clone()).with_wake(wake);

        {
            let mut decided = decisions.lock().expect("decisions lock poisoned");
            for info in keyboards {
                decided.insert(info.path.clone());
                match open_device(&info.path) {
                    Ok(device) => {
                        registry.attach(&info);
                        devices.insert(info.path.clone(), device);
                    }
                    Err(error) => {
                        stats.increment_read_error();
                        if error.raw_os_error() == Some(libc::ENOENT)
                            || error.raw_os_error() == Some(libc::ENODEV)
                        {
                            // Vanished between enumeration and open: let the
                            // monitor offer it again instead of giving up.
                            decided.remove(&info.path);
                        }
                        eprintln!(
                            "keyvibes: skipping keyboard {}: {error}",
                            info.path.display()
                        );
                    }
                }
            }
        }

        let attached = Arc::new(AtomicUsize::new(devices.len()));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, receiver) = std::sync::mpsc::channel::<PipelineCommand>();

        let thread_stop = stop.clone();
        let thread_attached = attached.clone();
        let thread_decisions = decisions;
        let handle = thread::Builder::new()
            .name("keyvibes-input".to_string())
            .spawn(move || {
                run_pipeline(PipelineContext {
                    receiver,
                    stop: thread_stop,
                    attached: thread_attached,
                    decisions: thread_decisions,
                    devices,
                    registry,
                });
            })
            .map_err(|e| crate::error::InputError::HotplugFailed(e.to_string()))?;

        Ok(Self {
            port: CommandPort { sender: commands },
            stop,
            attached,
            handle: Some(handle),
        })
    }

    /// Sends a command to the pipeline thread.
    pub fn send(&self, command: PipelineCommand) {
        self.port.send(command);
    }

    /// A cloneable handle for other threads (the hotplug monitor).
    pub fn command_port(&self) -> CommandPort {
        self.port.clone()
    }

    /// Number of keyboards currently attached.
    pub fn attached_count(&self) -> usize {
        self.attached.load(Ordering::Relaxed)
    }

    /// Whether the pipeline is running.
    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for InputPipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The thread wakes at the latest one poll-timeout from now.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Opens a keyboard and puts its descriptor into non-blocking mode.
///
/// Non-blocking means the pipeline thread can never be wedged inside `read`
/// by a misbehaving device: a spurious wakeup surfaces as `WouldBlock` and
/// the next `poll` simply waits again.
fn open_device(path: &Path) -> io::Result<Device> {
    let device = Device::open(path)?;
    device.set_nonblocking(true)?;
    Ok(device)
}

struct PipelineContext<S: ?Sized> {
    receiver: Receiver<PipelineCommand>,
    stop: Arc<AtomicBool>,
    attached: Arc<AtomicUsize>,
    decisions: Arc<Mutex<HashSet<PathBuf>>>,
    devices: HashMap<PathBuf, Device>,
    registry: DeviceRegistry<S>,
}

fn run_pipeline<S>(mut context: PipelineContext<S>)
where
    S: SoundSource + ?Sized,
{
    let PipelineContext {
        receiver,
        stop,
        attached,
        decisions,
        devices,
        registry,
    } = &mut context;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 1. Apply everything the monitor asked for since the last wakeup.
        while let Ok(command) = receiver.try_recv() {
            apply_command(command, devices, registry, decisions, attached);
        }

        // 2. Sleep until a device has data, or the timeout elapses so any
        //    queued command (including shutdown) is picked up.
        if devices.is_empty() {
            thread::sleep(Duration::from_millis(POLL_TIMEOUT_MS as u64));
            continue;
        }

        let mut paths: Vec<PathBuf> = Vec::with_capacity(devices.len());
        let mut pollfds: Vec<libc::pollfd> = Vec::with_capacity(devices.len());
        for (path, device) in devices.iter() {
            paths.push(path.clone());
            pollfds.push(libc::pollfd {
                fd: device.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }

        let ready = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                POLL_TIMEOUT_MS,
            )
        };

        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                // Back off rather than spinning on a broken descriptor.
                eprintln!("keyvibes: input poll failed: {error}");
                thread::sleep(Duration::from_millis(POLL_TIMEOUT_MS as u64));
            }
            continue;
        }
        if ready == 0 {
            continue;
        }

        // 3. Split the results into "device vanished" and "events waiting"
        //    before mutating anything, so we never hold a borrow across a
        //    removal.
        let mut vanished: Vec<PathBuf> = Vec::new();
        let mut readable: Vec<PathBuf> = Vec::new();
        for (index, pollfd) in pollfds.iter().enumerate() {
            if pollfd.revents == 0 {
                continue;
            }
            let path = &paths[index];
            if pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                vanished.push(path.clone());
            } else if pollfd.revents & libc::POLLIN != 0 {
                readable.push(path.clone());
            }
        }

        for path in vanished {
            eprintln!("keyvibes: keyboard removed: {}", path.display());
            registry.detach(&path);
            devices.remove(&path);
            decisions
                .lock()
                .expect("decisions lock poisoned")
                .remove(&path);
            attached.store(registry.len(), Ordering::Relaxed);
        }

        for path in readable {
            drain_device(&path, devices, registry, attached);
        }
    }
}

/// Reads every pending event from one device and dispatches it.
fn drain_device<S>(
    path: &Path,
    devices: &mut HashMap<PathBuf, Device>,
    registry: &mut DeviceRegistry<S>,
    attached: &Arc<AtomicUsize>,
) where
    S: SoundSource + ?Sized,
{
    // Scope the descriptor borrow so the detach below can mutate `devices`.
    // The `Result` returned by `fetch_events` carries a lifetime tied to the
    // device, so it must be fully consumed before we remove anything.
    let outcome = {
        let device = match devices.get_mut(path) {
            Some(device) => device,
            None => return,
        };

        match device.fetch_events() {
            Ok(events) => {
                let mut saw_any = false;
                for event in events {
                    saw_any = true;
                    registry.handle_event(path, &event);
                }
                if saw_any {
                    registry.record_read_success(path);
                }
                DrainOutcome::Keep
            }
            Err(error) => match classify_read_error(&error) {
                ReadFault::Nothing => DrainOutcome::Keep,
                ReadFault::Removed => DrainOutcome::Remove("disappeared"),
                ReadFault::Transient => {
                    if registry.record_read_error(path) {
                        DrainOutcome::Remove("giving up after repeated read errors")
                    } else {
                        DrainOutcome::Keep
                    }
                }
            },
        }
    };

    if let DrainOutcome::Remove(reason) = outcome {
        eprintln!("keyvibes: keyboard {reason}: {}", path.display());
        registry.detach(path);
        devices.remove(path);
        attached.store(registry.len(), Ordering::Relaxed);
    }
}

/// What `drain_device` decided to do with a descriptor afterwards.
enum DrainOutcome {
    Keep,
    Remove(&'static str),
}

/// Applies one hotplug command.
fn apply_command<S>(
    command: PipelineCommand,
    devices: &mut HashMap<PathBuf, Device>,
    registry: &mut DeviceRegistry<S>,
    decisions: &Arc<Mutex<HashSet<PathBuf>>>,
    attached: &Arc<AtomicUsize>,
) where
    S: SoundSource + ?Sized,
{
    match command {
        PipelineCommand::Attach(info) => {
            let mut decided = decisions.lock().expect("decisions lock poisoned");
            if decided.contains(&info.path) {
                return;
            }
            decided.insert(info.path.clone());
            drop(decided);

            match open_device(&info.path) {
                Ok(device) => {
                    registry.attach(&info);
                    devices.insert(info.path.clone(), device);
                    eprintln!(
                        "keyvibes: keyboard attached: {} ({})",
                        info.name,
                        info.path.display()
                    );
                }
                Err(error) => {
                    if error.raw_os_error() == Some(libc::ENOENT)
                        || error.raw_os_error() == Some(libc::ENODEV)
                    {
                        // Not actually here; let the monitor try again.
                        decisions
                            .lock()
                            .expect("decisions lock poisoned")
                            .remove(&info.path);
                    }
                    eprintln!(
                        "keyvibes: cannot use keyboard {}: {error}",
                        info.path.display()
                    );
                }
            }
            attached.store(registry.len(), Ordering::Relaxed);
        }
        PipelineCommand::Detach(path) => {
            decisions
                .lock()
                .expect("decisions lock poisoned")
                .remove(&path);
            if registry.detach(&path) {
                eprintln!("keyvibes: keyboard detached: {}", path.display());
            }
            devices.remove(&path);
            attached.store(registry.len(), Ordering::Relaxed);
        }
    }
}
