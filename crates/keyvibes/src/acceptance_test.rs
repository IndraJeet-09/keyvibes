//! Phase 25 acceptance: `keyvibes acceptance-test`.
//!
//! The whole product, one process, one pass: launch, discover, load a pack,
//! start PipeWire, feed it keys, and observe that sounds come out - then the
//! lifecycle edges that make a difference in daily use: variants, polyphony,
//! pack switching, audio reconnect, configuration.
//!
//! This is deliberately *not* a summary of the other acceptance commands. It
//! drives the real `Runtime` the way `keyvibes run` does and checks the
//! observable contract at each step of the product flow, so a regression that
//! only shows up when the pieces are wired together still fails here.
//!
//! Checks are reported in the order the flow executes:

//! 1. KVPack loading, 2. input discovery, 3. audio backend,
//! 4. event to command, 5. variant selection, 6. voice allocation,
//! 7. audio rendering, 8. configuration, 9. pack switching,
//! 10. recovery, 11. RT safety, 12. privacy and security.
//!
//! A check that needs hardware this host does not have reports `NOT RUN` with
//! the reason. It is never reported as a pass.

use crate::accept::{Report, Status};
use crate::{alloc_probe, config, pack_locate};
use anyhow::Result;
use kv_core::{PhysicalKey, SoundSource, VariantState};
use kv_input_linux::discovery::discover_keyboards;
use kv_mixer::{Mixer, MAX_VOICES};
use kv_pack::KvPack;
use kv_runtime::{Runtime, SimInputOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Options for one acceptance run.
pub struct AcceptanceOptions {
    /// `--pack`, already resolved, or `None` to take the first installed one.
    pub pack: Option<PathBuf>,
    /// Audio output sample rate.
    pub rate: u32,
    /// Mixer settings from the configuration file.
    pub settings: kv_core::Settings,
    /// PipeWire node from `output_device` in the configuration file.
    pub output_device: Option<String>,
}

/// Runs the acceptance test.
pub fn run(options: AcceptanceOptions) -> Result<()> {
    let mut report = Report::new("acceptance-test");
    let scratch = scratch_root();
    let _ = std::fs::create_dir_all(&scratch);

    // ------------------------------------------------------------ launching
    // 1. KVPack loading - the pack is the first thing the product needs and
    //    the thing it cannot invent.
    let pack_path = match &options.pack {
        Some(path) => path.clone(),
        None => match pack_locate::resolve(None) {
            Ok(path) => path,
            Err(error) => {
                report.fail(
                    "KVPack loading",
                    format!("no sound pack could be located: {error:#}"),
                );
                return report.finish();
            }
        },
    };
    let loaded = match KvPack::open(&pack_path) {
        Ok(loaded) => loaded,
        Err(error) => {
            report.fail(
                "KVPack loading",
                format!("cannot open {}: {error}", pack_path.display()),
            );
            return report.finish();
        }
    };
    let stats = loaded.stats();
    if stats.name.is_empty() || stats.keys == 0 || stats.clips == 0 {
        report.fail(
            "KVPack loading",
            format!(
                "{} opened but looks empty: name {:?}, {} keys, {} clips",
                pack_path.display(),
                stats.name,
                stats.keys,
                stats.clips
            ),
        );
        return report.finish();
    }
    let keys: Vec<PhysicalKey> = loaded
        .keys()
        .iter()
        .map(|entry| entry.physical_key)
        .collect();
    let best_variants = loaded
        .keys()
        .iter()
        .map(|entry| entry.variant_count)
        .max()
        .unwrap_or(0);
    report.pass(
        "KVPack loading",
        format!(
            "\"{}\": {} keys, {} clips, {} Hz, best key offers {} variant(s), from {}",
            stats.name,
            stats.keys,
            stats.clips,
            stats.sample_rate,
            best_variants,
            pack_path.display()
        ),
    );

    // ------------------------------------------------------- input discovery
    // 2. The enumeration must work whether or not a keyboard is attached;
    //    what it finds is the host's business, reported in the detail.
    match discover_keyboards() {
        Ok(found) => report.pass(
            "Input discovery",
            format!(
                "evdev enumeration completed; {} keyboard(s) readable by this user",
                found.len()
            ),
        ),
        Err(error) => report.fail("Input discovery", format!("enumeration failed: {error:#}")),
    }

    // -------------------------------------------------------- audio backend
    // 3. Start the real engine. Everything after this point runs against a
    //    live PipeWire stream.
    let mut runtime = Runtime::new(options.rate);
    runtime.set_settings(options.settings);
    runtime.set_output_device(options.output_device.clone());
    if let Err(error) = runtime.load_pack(&pack_path) {
        report.fail(
            "Audio backend",
            format!(
                "failed to load {} into the engine: {error}",
                pack_path.display()
            ),
        );
        return report.finish();
    }
    let audio_started = match runtime.start_audio() {
        Ok(()) => runtime.wait_for_audio(Duration::from_secs(10)),
        Err(error) => {
            report.fail(
                "Audio backend",
                format!("PipeWire would not start: {error}"),
            );
            return report.finish();
        }
    };
    if !audio_started {
        report.fail(
            "Audio backend",
            "the stream never reached the streaming state within 10s",
        );
        return report.finish();
    }
    let negotiated = runtime.audio_stats();
    report.pass(
        "Audio backend",
        format!(
            "PipeWire stream streaming at {} Hz, {} ns quantum",
            negotiated.negotiated_rate, negotiated.quantum_ns
        ),
    );

    // ------------------------------------------------------ event -> command
    // 4. The product flow: a key press must become a queued command and then
    //    a rendered block. Simulated input is the same code path a real
    //    keyboard takes after evdev hands over a keycode.
    if keys.is_empty() {
        report.not_run(
            "Event to command",
            "the selected pack contains no keys to press",
        );
    } else {
        let before = runtime.audio_stats();
        let _ = runtime.start_simulated(SimInputOptions::from_keys(
            &keys,
            Duration::from_millis(3),
            6,
        ));
        std::thread::sleep(Duration::from_secs(2));
        let input = runtime.input_stats();
        let after = runtime.audio_stats();
        runtime.stop_simulated();
        let drained = after
            .drained_commands
            .saturating_sub(before.drained_commands);
        if input.commands_generated > 0 && input.commands_dropped == 0 && drained > 0 {
            report.pass(
                "Event to command",
                format!(
                    "{} press(es) -> {} queued command(s), 0 dropped, {} drained by the callback",
                    input.key_presses, input.commands_generated, drained
                ),
            );
        } else {
            report.fail(
                "Event to command",
                format!(
                    "{} press(es), {} command(s), {} dropped, {} drained",
                    input.key_presses, input.commands_generated, input.commands_dropped, drained
                ),
            );
        }
    }

    // ------------------------------------------------------ variant selection
    // 5. Pressing the same key repeatedly has to reach more than one clip, or
    //    the "mechanical keyboard" part of the product is a lie. Checked at
    //    the pack player, which is exactly where the product decides.
    match runtime.player() {
        Some(player) => match best_variant_key(&loaded).or_else(|| keys.first().copied()) {
            Some(key) => {
                let mut state = VariantState::default();
                let mut distinct = std::collections::HashSet::new();
                let presses = 400usize;
                for _ in 0..presses {
                    if let Some(command) = player.play(key, &mut state) {
                        distinct.insert(command.sample_ptr as usize);
                    }
                }
                let expected = loaded
                    .lookup(key)
                    .map(|sounds| sounds.variant_count)
                    .unwrap_or(0);
                let want = expected.clamp(1, 2) as usize;
                if distinct.len() >= want && distinct.len() <= expected as usize {
                    report.pass(
                        "Variant selection",
                        format!(
                            "{presses} press(es) of the pack's best-variant key reached {} of {expected} variant(s)",
                            distinct.len()
                        ),
                    );
                } else {
                    report.fail(
                        "Variant selection",
                        format!(
                            "{presses} press(es) of the pack's best-variant key reached {} variant(s); pack offers {expected}, expected at least {want}",
                            distinct.len()
                        ),
                    );
                }
            }
            None => report.not_run(
                "Variant selection",
                "the selected pack contains no keys to vary",
            ),
        },
        None => report.not_run("Variant selection", "the engine holds no pack player"),
    }

    // ------------------------------------------------------- voice allocation
    // 6. 32 voices, and not 33. Checked against the mixer directly so the
    //    answer is exact rather than a sample of a moving counter.
    {
        let mut mixer = Mixer::new(options.rate);
        let silence: [i16; 64] = [0; 64];
        for _ in 0..(MAX_VOICES + 8) {
            mixer.trigger(silence_command(&silence));
        }
        let active = mixer.active_voice_count();
        let mut buffer = vec![0.0f32; 256 * 2];
        // SAFETY: the buffer is interleaved stereo with whole frames, and the
        // silence it mixes from lives on this stack frame.
        unsafe { mixer.render_block(&mut buffer) };
        let after = mixer.active_voice_count();
        if active <= MAX_VOICES && after <= MAX_VOICES && active >= 1 {
            report.pass(
                "Voice allocation",
                format!(
                    "{active} voice(s) admitted from {} triggers, cap {MAX_VOICES}, {after} still sounding after one block",
                    MAX_VOICES + 8
                ),
            );
        } else {
            report.fail(
                "Voice allocation",
                format!("admitted {active} voices, cap {MAX_VOICES}, {after} after a block"),
            );
        }
    }

    // --------------------------------------------------------- audio rendering
    // 7. Something has to have come out of the speakers' end.
    let rendered = runtime.audio_stats();
    if rendered.frames_rendered > 0 && rendered.peak > 0.0 {
        report.pass(
            "Audio rendering",
            format!(
                "{} frame(s) rendered across {} callback(s), peak {:.4}",
                rendered.frames_rendered, rendered.callbacks, rendered.peak
            ),
        );
    } else {
        report.fail(
            "Audio rendering",
            format!(
                "{} frame(s), {} callback(s), peak {:.4} - nothing audible was produced",
                rendered.frames_rendered, rendered.callbacks, rendered.peak
            ),
        );
    }

    // ------------------------------------------------------------ configuration
    // 8. The file has to survive a round trip and reject what it cannot run.
    let (config_status, config_detail) = check_configuration(&scratch);
    report.add(config_status, "Configuration", config_detail);

    // ----------------------------------------------------------- pack switching
    // 9. Replace the pack under a live stream.
    let other = pack_locate::discover_unique()
        .into_iter()
        .find(|path| path != &pack_path);
    match other {
        None => report.not_run(
            "Pack switching",
            "only one pack is installed, so there is nothing to switch to",
        ),
        Some(other) => match runtime.switch_pack(&other) {
            Ok(swap) => {
                let keys_after: Vec<PhysicalKey> = KvPack::open(&other)
                    .map(|pack| pack.keys().iter().map(|entry| entry.physical_key).collect())
                    .unwrap_or_default();
                let player_still_there = runtime.player().is_some();
                let stream_still_up = runtime.audio_stats().saw_streaming;
                if player_still_there && stream_still_up {
                    report.pass(
                        "Pack switching",
                        format!(
                            "swapped to \"{}\" under a running stream ({} retired pack(s) pending)",
                            other.display(),
                            swap.retired
                        ),
                    );
                } else {
                    report.fail(
                        "Pack switching",
                        format!(
                            "swap returned but player present={player_still_there}, streaming={stream_still_up}; {} keys in the new pack",
                            keys_after.len()
                        ),
                    );
                }
            }
            Err(error) => report.fail(
                "Pack switching",
                format!("switching to {} failed: {error}", other.display()),
            ),
        },
    }

    // ---------------------------------------------------------------- recovery
    // 10. Audio comes back on its own after a disconnect. `reconnect_audio`
    //     is what the engine's supervisor calls; going quiet and returning is
    //     the whole contract.
    let before_reconnect = runtime.audio_stats();
    runtime.reconnect_audio();
    let reconnected = wait_for_frames(
        &runtime,
        before_reconnect.frames_rendered,
        Duration::from_secs(10),
    );
    if reconnected {
        let after = runtime.audio_stats();
        report.pass(
            "Recovery",
            format!(
                "engine dropped and re-acquired the stream; {} frame(s) rendered after the reconnect",
                after
                    .frames_rendered
                    .saturating_sub(before_reconnect.frames_rendered)
            ),
        );
    } else {
        report.fail(
            "Recovery",
            "the stream did not resume rendering within 10s of a reconnect",
        );
    }

    // ---------------------------------------------------------------- RT safety
    // 11. With the engine running and nothing else happening, the real-time
    //     callback must be the only thing running - and it must not allocate.
    let callbacks_before = runtime.audio_stats().callbacks;
    let alloc_before = alloc_probe::Snapshot::take();
    std::thread::sleep(Duration::from_millis(500));
    let alloc_after = alloc_probe::Snapshot::take();
    let callbacks_after = runtime.audio_stats().callbacks;
    let allocations = alloc_before.allocations_since(alloc_after);
    let idle_callbacks = callbacks_after.saturating_sub(callbacks_before);
    let final_stats = runtime.audio_stats();
    let overruns = final_stats.deadline_misses + final_stats.budget_overruns;
    if allocations == 0 && overruns == 0 && idle_callbacks > 0 {
        report.pass(
            "RT safety",
            format!(
                "{idle_callbacks} callback(s) ran in a 500 ms idle window with 0 heap allocation(s), 0 deadline miss(es), 0 budget overrun(s)",
            ),
        );
    } else {
        report.fail(
            "RT safety",
            format!(
                "{allocations} allocation(s), {overruns} deadline/budget failure(s) across {idle_callbacks} idle callback(s)",
            ),
        );
    }

    // ------------------------------------------------------ privacy / security
    // 12. The audit is the same one `security-test` runs, plus a look at this
    //     very process: no socket should have appeared while the engine ran.
    let (privacy_status, privacy_detail) = check_privacy();
    report.add(privacy_status, "Privacy/security", privacy_detail);

    runtime.shutdown();
    report.finish()
}

// ----------------------------------------------------------------- checks ---

/// Configuration: round-trips, and rejects what it cannot run.
fn check_configuration(scratch: &Path) -> (Status, String) {
    let path = scratch.join("acceptance-config.toml");
    let config = config::Config {
        volume: 0.42,
        pack: Some("Acceptance".to_string()),
        pitch_variation: false,
        ..config::Config::default()
    };
    if let Err(error) = config.save(&path) {
        return (
            Status::Fail,
            format!("cannot write {}: {error}", path.display()),
        );
    }
    let loaded = match config::Config::load(&path) {
        Ok(loaded) => loaded,
        Err(error) => {
            return (
                Status::Fail,
                format!("cannot read back {}: {error}", path.display()),
            )
        }
    };
    let round_trips = (loaded.volume - 0.42).abs() < 0.001
        && loaded.pack.as_deref() == Some("Acceptance")
        && !loaded.pitch_variation;

    let broken = scratch.join("acceptance-broken.toml");
    let _ = std::fs::write(&broken, "version = 1\nvolume = 9\n");
    let rejected = match config::Config::load(&broken) {
        Ok(_) => false,
        Err(error) => error.to_string().contains("volume"),
    };

    match (round_trips, rejected) {
        (true, true) => (
            Status::Pass,
            "volume 0.42, pack name and pitch_variation round-tripped; volume = 9 rejected by name".to_string(),
        ),
        (false, _) => (
            Status::Fail,
            format!("read back volume {}, pack {:?}, pitch_variation {} - expected 0.42 / \"Acceptance\" / false",
                loaded.volume, loaded.pack, loaded.pitch_variation),
        ),
        (_, false) => (
            Status::Fail,
            "volume = 9 was accepted, but it is outside 0.0 ..= 1.0".to_string(),
        ),
    }
}

/// Privacy: this process opened no outbound network connection, and the
/// source audit still holds.
///
/// The engine is already running when this is called, so a connection made
/// by the program would be visible. PipeWire is reached over a Unix socket
/// in the user's runtime directory, which is not a network connection and is
/// not counted here.
fn check_privacy() -> (Status, String) {
    match outbound_connections() {
        Ok(0) => {}
        Ok(count) => {
            return (
                Status::Fail,
                format!("{count} outbound network connection(s) while the engine was running"),
            )
        }
        Err(detail) => {
            return (
                Status::Fail,
                format!("cannot read this process's network state: {detail}"),
            )
        }
    }

    match security_audit_in_process() {
        Ok(()) => (
            Status::Pass,
            "0 outbound network connection(s); source audit found no logging, network, grab, or injection".to_string(),
        ),
        Err(detail) => (Status::Fail, format!("security audit: {detail}")),
    }
}

/// TCP and UDP sockets this process owns that have a remote peer.
///
/// `/proc/net/{tcp,tcp6,udp,udp6}` list the whole network namespace, so each
/// entry is matched back to this process by inode through `/proc/self/fd`.
/// A socket with a non-zero remote address is one we connected to; a listener
/// or an unconnected Unix socket has none.
fn outbound_connections() -> Result<usize, String> {
    let mut mine = std::collections::HashSet::new();
    let fds = std::fs::read_dir("/proc/self/fd").map_err(|error| error.to_string())?;
    for entry in fds.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let text = target.to_string_lossy();
            if let Some(rest) = text.strip_prefix("socket:[") {
                if let Some(inode) = rest.strip_suffix(']') {
                    mine.insert(inode.to_string());
                }
            }
        }
    }

    let mut connected = 0usize;
    for table in [
        "/proc/net/tcp",
        "/proc/net/tcp6",
        "/proc/net/udp",
        "/proc/net/udp6",
    ] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl local rem st ... inode, where inode is the 10th field.
            if fields.len() < 10 {
                continue;
            }
            let remote = fields[3];
            let inode = fields[9];
            let is_connected = !remote.ends_with(":0000");
            if is_connected && mine.contains(inode) {
                connected += 1;
            }
        }
    }
    Ok(connected)
}

/// Re-runs the source audit by asking this binary to do it.
///
/// Invoking `security-test` rather than re-implementing it keeps one audit in
/// the tree: if the policy changes, both commands change together.
fn security_audit_in_process() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let output = Command::new(exe)
        .arg("security-test")
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&output.stdout);
        let failures: Vec<&str> = text.lines().filter(|line| line.contains("[FAIL")).collect();
        Err(if failures.is_empty() {
            format!("security-test exited {}", output.status)
        } else {
            failures.join("; ")
        })
    }
}

// -------------------------------------------------------------- helpers ---

/// Waits until the engine has rendered past `previous` frames.
fn wait_for_frames(runtime: &Runtime, previous: u64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if runtime.audio_stats().frames_rendered > previous {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// The key in the pack with the most variants, if any.
fn best_variant_key(pack: &KvPack) -> Option<PhysicalKey> {
    pack.keys()
        .iter()
        .max_by_key(|entry| entry.variant_count)
        .map(|entry| entry.physical_key)
}

/// A command pointing at `samples`, with no variation applied.
///
/// # Safety
///
/// `samples` must outlive the command and any voice built from it. Both the
/// caller's buffer and the mixer are local to one block.
fn silence_command(samples: &[i16]) -> kv_core::PlayCommand {
    kv_core::PlayCommand {
        sample_ptr: samples.as_ptr(),
        sample_len: samples.len() as u32,
        source_rate: 48_000,
        pitch_step: 1 << 32,
        left_gain: 0.25,
        right_gain: 0.25,
        release: false,
        enqueued_ns: 0,
    }
}

/// A scratch directory private to this run.
fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("keyvibes-acceptance-{}", std::process::id()))
}
