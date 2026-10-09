//! Phase 9: device hotplug and multi-keyboard robustness.
//!
//! These tests drive the **exact** state machine the live pipeline runs -
//! [`DeviceRegistry`] - through the lifecycle the acceptance test describes:
//! keyboard A attached, keyboard B attached, A removed, A reconnected, keys
//! pressed throughout.
//!
//! They are deterministic and need no hardware, which matters because a real
//! unplug cannot be scripted from inside the process. The live half of
//! `keyvibes hotplug-test` covers what only hardware can prove, and reports
//! it as "not run" when no keyboard is readable rather than claiming a pass.

use evdev::{EventType, InputEvent, KeyCode as Key};
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_input_linux::diagnostics::InputStats;
use kv_input_linux::discovery::KeyboardInfo;
use kv_input_linux::pipeline::{classify_read_error, DeviceRegistry, ReadFault};
use kv_ring::SpscRing;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A sound source that always answers with a command, so tests can count
/// how many keys actually produced audio work.
struct AlwaysSource {
    clip: Vec<i16>,
}

impl AlwaysSource {
    fn new() -> Self {
        Self {
            clip: (0..512).map(|i| (i % 64) as i16 - 32).collect(),
        }
    }
}

impl SoundSource for AlwaysSource {
    fn play(&self, _key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
        Some(unsafe {
            PlayCommand::new(
                self.clip.as_ptr(),
                self.clip.len() as u32,
                48000,
                1 << 32,
                1.0,
                1.0,
                false,
            )
        })
    }
}

fn info(path: &str, name: &str) -> KeyboardInfo {
    KeyboardInfo {
        path: PathBuf::from(path),
        name: name.to_string(),
        phys: None,
    }
}

fn key_event(code: Key, pressed: bool) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.code(), i32::from(pressed))
}

fn sync_report() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
}

/// Bundle of a registry plus handles to everything it shares.
struct Harness {
    registry: DeviceRegistry<AlwaysSource>,
    queue: Arc<SpscRing<PlayCommand>>,
    stats: Arc<InputStats>,
}

impl Harness {
    fn new() -> Self {
        let queue = Arc::new(SpscRing::with_capacity(64));
        let stats = Arc::new(InputStats::new());
        let source = Arc::new(AlwaysSource::new());
        let registry = DeviceRegistry::new(source, queue.clone(), stats.clone());
        Self {
            registry,
            queue,
            stats,
        }
    }

    fn press(&mut self, path: &Path, key: Key) {
        self.registry.handle_event(path, &key_event(key, true));
        self.registry.handle_event(path, &sync_report());
    }

    fn release(&mut self, path: &Path, key: Key) {
        self.registry.handle_event(path, &key_event(key, false));
        self.registry.handle_event(path, &sync_report());
    }

    fn commands_drained(&self) -> usize {
        let mut count = 0;
        while self.queue.pop().is_some() {
            count += 1;
        }
        count
    }
}

const A: &str = "/dev/input/event7";
const B: &str = "/dev/input/event9";

#[test]
fn two_keyboards_attach_and_are_counted() {
    let mut h = Harness::new();
    assert!(h.registry.attach(&info(A, "Holy Panda Board")));
    assert!(h.registry.attach(&info(B, "Linear Board")));
    assert!(
        !h.registry.attach(&info(A, "Holy Panda Board")),
        "duplicate attach is a no-op"
    );
    assert_eq!(h.registry.len(), 2);
    assert_eq!(h.stats.snapshot().devices_added, 2);
}

#[test]
fn pressed_state_is_tracked_per_device() {
    let mut h = Harness::new();
    h.registry.attach(&info(A, "A"));
    h.registry.attach(&info(B, "B"));

    h.press(Path::new(A), Key::KEY_S);
    h.press(Path::new(B), Key::KEY_L);

    let state_a = h.registry.state(Path::new(A)).expect("A attached");
    let state_b = h.registry.state(Path::new(B)).expect("B attached");
    assert!(state_a.is_pressed(PhysicalKey::S));
    assert!(state_b.is_pressed(PhysicalKey::L));
    assert!(!state_a.is_pressed(PhysicalKey::L));
    assert_eq!(h.registry.held_keys(), 2);
    assert_eq!(h.stats.snapshot().keys_held, 2);

    // A release on A must not disturb B.
    h.release(Path::new(A), Key::KEY_S);
    assert!(!h
        .registry
        .state(Path::new(A))
        .unwrap()
        .is_pressed(PhysicalKey::S));
    assert!(h
        .registry
        .state(Path::new(B))
        .unwrap()
        .is_pressed(PhysicalKey::L));
    assert_eq!(h.registry.held_keys(), 1);
}

#[test]
fn removing_a_keyboard_leaves_the_other_untouched() {
    let mut h = Harness::new();
    h.registry.attach(&info(A, "A"));
    h.registry.attach(&info(B, "B"));

    h.press(Path::new(A), Key::KEY_S);
    h.press(Path::new(B), Key::KEY_L);
    let before = h.commands_drained();
    assert_eq!(before, 2, "both presses should produce commands");

    assert!(h.registry.detach(Path::new(A)));
    assert_eq!(h.registry.len(), 1);
    assert!(!h.registry.contains(Path::new(A)));
    // Only A's held key is forgotten.
    assert_eq!(h.registry.held_keys(), 1);
    assert_eq!(h.stats.snapshot().keys_held, 1);
    assert_eq!(h.stats.snapshot().devices_removed, 1);

    // B keeps working after A vanished.
    h.press(Path::new(B), Key::KEY_K);
    assert_eq!(h.commands_drained(), 1);
}

#[test]
fn keyboard_works_after_reconnect() {
    let mut h = Harness::new();
    h.registry.attach(&info(A, "A"));
    h.press(Path::new(A), Key::KEY_S);
    assert_eq!(h.commands_drained(), 1);

    h.registry.detach(Path::new(A));
    assert_eq!(h.registry.len(), 0);

    // Reconnect: the same path comes back as a fresh device.
    assert!(h.registry.attach(&info(A, "A")));
    let state = h.registry.state(Path::new(A)).expect("reattached");
    assert!(
        !state.is_pressed(PhysicalKey::S),
        "held state must not survive a replug"
    );
    h.press(Path::new(A), Key::KEY_D);
    assert_eq!(
        h.commands_drained(),
        1,
        "input still functional after reconnect"
    );
}

#[test]
fn events_for_an_unknown_path_are_ignored() {
    let mut h = Harness::new();
    // A device can deliver a final batch in the same poll cycle in which it
    // was removed; that must be silently dropped, never panic.
    h.registry.handle_event(
        Path::new("/dev/input/event404"),
        &key_event(Key::KEY_A, true),
    );
    assert_eq!(h.commands_drained(), 0);
    assert_eq!(h.registry.held_keys(), 0);
}

#[test]
fn syn_dropped_resets_only_the_affected_keyboard() {
    let mut h = Harness::new();
    h.registry.attach(&info(A, "A"));
    h.registry.attach(&info(B, "B"));
    h.press(Path::new(A), Key::KEY_S);
    h.press(Path::new(B), Key::KEY_L);

    h.registry.handle_event(
        Path::new(A),
        &InputEvent::new(EventType::SYNCHRONIZATION.0, 1, 0),
    );

    assert_eq!(h.registry.held_keys(), 1, "only A was reset");
    assert_eq!(h.stats.snapshot().sync_dropped, 1);
    assert!(h
        .registry
        .state(Path::new(B))
        .unwrap()
        .is_pressed(PhysicalKey::L));

    // The kernel replays the true state as compensating events.
    h.press(Path::new(A), Key::KEY_S);
    assert_eq!(h.registry.held_keys(), 2);
}

#[test]
fn repeated_read_errors_detach_the_device() {
    let mut h = Harness::new();
    h.registry.attach(&info(A, "A"));

    let mut detached = false;
    for _ in 0..32 {
        if h.registry.record_read_error(Path::new(A)) {
            detached = true;
            break;
        }
    }
    assert!(detached, "the error budget must be bounded");

    h.registry.detach(Path::new(A));
    assert_eq!(h.registry.len(), 0);
    assert_eq!(h.stats.snapshot().devices_removed, 1);

    // A successful read clears the budget again.
    h.registry.attach(&info(A, "A"));
    h.registry.record_read_success(Path::new(A));
    assert!(!h.registry.record_read_error(Path::new(A)));
}

#[test]
fn read_errors_on_a_missing_entry_are_fatal() {
    let mut h = Harness::new();
    // Not attached: there is nothing to retry, so the caller must detach.
    assert!(h
        .registry
        .record_read_error(Path::new("/dev/input/event404")));
}

#[test]
fn read_faults_are_classified() {
    assert_eq!(
        classify_read_error(&io::Error::new(io::ErrorKind::WouldBlock, "x")),
        ReadFault::Nothing
    );
    assert_eq!(
        classify_read_error(&io::Error::new(io::ErrorKind::Interrupted, "x")),
        ReadFault::Nothing
    );
    assert_eq!(
        classify_read_error(&io::Error::from_raw_os_error(libc::ENODEV)),
        ReadFault::Removed
    );
    assert_eq!(
        classify_read_error(&io::Error::from_raw_os_error(libc::ENOENT)),
        ReadFault::Removed
    );
    assert_eq!(
        classify_read_error(&io::Error::from_raw_os_error(libc::EIO)),
        ReadFault::Removed
    );
    assert_eq!(
        classify_read_error(&io::Error::new(io::ErrorKind::ConnectionReset, "x")),
        ReadFault::Removed
    );
    assert_eq!(
        classify_read_error(&io::Error::from_raw_os_error(libc::EBUSY)),
        ReadFault::Transient
    );
}

#[test]
fn full_acceptance_lifecycle_keeps_working() {
    // A connected -> B connected -> A removed -> A reconnected, with keys
    // pressed throughout.
    let mut h = Harness::new();

    h.registry.attach(&info(A, "keyboard A"));
    h.press(Path::new(A), Key::KEY_S);

    h.registry.attach(&info(B, "keyboard B"));
    h.press(Path::new(B), Key::KEY_L);

    h.registry.detach(Path::new(A));
    h.press(Path::new(B), Key::KEY_K);

    h.registry.attach(&info(A, "keyboard A"));
    h.press(Path::new(A), Key::KEY_D);

    // Still alive, still accepting input on both.
    //   presses: A/S, B/L, (A gone), B/K, A/D
    assert_eq!(h.registry.len(), 2);
    assert_eq!(h.commands_drained(), 4, "every press produced a command");
    assert_eq!(
        h.stats.snapshot().commands_generated,
        4,
        "no command was dropped on the way to the queue"
    );
    assert_eq!(h.stats.snapshot().devices_added, 3);
    assert_eq!(h.stats.snapshot().devices_removed, 1);
    // A/S was forgotten when A was unplugged; B/L, B/K and A/D remain held.
    assert_eq!(h.registry.held_keys(), 3);
    assert_eq!(h.stats.snapshot().keys_held, 3);
}
