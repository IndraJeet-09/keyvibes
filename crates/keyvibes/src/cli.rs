//! Command-line interface.
//!
//! The surface is two things in one list, kept apart by order rather than by
//! a flag:
//!
//! * **product commands** come first - `run`, `doctor`, `config`,
//!   `input-test`, `analyze`, `process`, `stress`, `benchmark`,
//!   `pack ...`. These are what a KeyVibes user reaches for, and their
//!   wording stays in the user's vocabulary.
//! * **verification commands** come after, and all of them are named
//!   `*-test`. They are the per-phase acceptance checks, and they are the
//!   ones that talk about XRUNs and device state machines.
//!
//! A bare `keyvibes` is `keyvibes run`: the program's whole job is to make
//! sound, so the no-argument case does that rather than printing a page of
//! text.
//!
//! `keyvibes cli-test` walks this interface and fails if any advertised
//! command stops answering its own `--help`.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "keyvibes")]
#[command(about = "Make your keyboard sound like a mechanical keyboard", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Sound pack to use: a name from `pack list`, or a path to a .kvpack
    ///
    /// Overrides the `pack` key in the configuration file, which in turn
    /// overrides the first installed pack.
    #[arg(long = "pack", global = true)]
    pub sound_pack: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Play keyboard sounds (what a bare `keyvibes` does)
    ///
    /// Loads a pack, reads the keyboard, and renders a voice per press until
    /// interrupted. Needs a readable keyboard and a running PipeWire session;
    /// `keyvibes doctor` reports either of those before you try.
    Run {
        /// Audio output sample rate
        #[arg(long, default_value_t = 48000)]
        rate: u32,

        /// Drive the engine with scripted key events instead of a keyboard
        ///
        /// Diagnostic only: exercises the full event-to-speaker pipeline when
        /// no readable keyboard is available.
        #[arg(long)]
        simulate: bool,

        /// Stop automatically after this many seconds (0 = run until Ctrl+C)
        #[arg(long, default_value_t = 0)]
        duration: u64,
    },

    /// Check this machine is ready, and say what to do about it if not
    ///
    /// Prints version, OS, kernel, input, audio, packs and runtime state as
    /// one fixed block of fields, so two runs can be compared. Every problem
    /// is listed with what is wrong, why KeyVibes thinks so, and the command
    /// that fixes it. Exits 0 when nothing the engine needs is missing, 1 when
    /// something is.
    Doctor,

    /// Show or create the configuration file
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// List the keyboards KeyVibes can hear, then capture presses for a while
    InputTest {
        /// Capture duration in seconds
        #[arg(short = 't', long, default_value_t = 10)]
        seconds: u64,
    },

    /// Analyze source audio without writing anything
    ///
    /// Accepts WAV files (analyzed with default processing settings) or a
    /// pack.toml manifest (analyzed with its own `[processing]` section, so
    /// the report shows exactly what `pack build` would do).
    Analyze {
        /// WAV files or a pack.toml manifest
        #[arg(required = true)]
        sources: Vec<PathBuf>,
    },

    /// Process one WAV through the audio pipeline and write the result
    ///
    /// Applies DC correction, trim, fade, loudness normalization, peak
    /// protection, and dithered quantization; writes a 16-bit mono PCM WAV.
    Process {
        /// Input WAV file
        input: PathBuf,

        /// Output WAV path
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Measure end-to-end latency under load and fail on any XRUN
    ///
    /// Reports software / audio-engine latency only: it does not measure the
    /// DAC, amplifier, transducer, or acoustic path.
    Stress {
        /// Seconds to run
        #[arg(long, default_value_t = 60)]
        duration: u64,

        /// Audio output sample rate
        #[arg(long, default_value_t = 48000)]
        rate: u32,

        /// Average key events offered per second by the load generator
        #[arg(long, default_value_t = 80.0)]
        keys_per_second: f64,
    },

    /// Measure the engine and fail when it leaves its budget
    ///
    /// Times the lock-free command queue, a 32-voice mixer block against the
    /// wall-clock length of that block, a key press from lookup to queued
    /// command, and the sound pack lookup itself - then counts heap
    /// allocations during each with the binary's own allocator, and checks
    /// that repeatedly opening a pack does not grow resident memory.
    Benchmark,

    /// Build, validate, and inspect .kvpack sound packs
    Pack {
        #[command(subcommand)]
        action: PackAction,
    },

    /// Control a running engine over its local socket
    ///
    /// Talks to `keyvibes run` through a per-user unix socket (no network).
    /// Every action prints the engine's state as JSON on success, so this
    /// doubles as a scripting interface: `keyvibes ctl volume 0.5 | jq`.
    Ctl {
        #[command(subcommand)]
        action: CtlAction,

        /// Control socket path (default: $KEYVIBES_SOCKET or the XDG runtime dir)
        #[arg(long, global = true)]
        socket: Option<PathBuf>,
    },

    // ------------------------------------------------- verification commands
    // The per-phase acceptance checks, all named `*-test` and all listed last
    // so the product is what a reader sees first. `acceptance-test` leads
    // them because it is the one that answers "does the whole thing work".
    /// The full product flow, end to end, in one pass
    ///
    /// Launches the real engine, discovers input, loads a pack, starts
    /// PipeWire, feeds it keys, and checks the contract at each step: packs
    /// load, input enumerates, events become commands, variants rotate, 32
    /// voices mix and render, configuration round-trips, a pack can be
    /// swapped under load, audio reconnects, the real-time path allocates
    /// nothing, and nothing is logged, transmitted, grabbed, or injected.
    /// Steps that need hardware this host does not have are `NOT RUN`,
    /// never a pass.
    AcceptanceTest,

    /// Verify the imported upstream soundpacks, end to end
    ///
    /// Ten checks over the packs converted from the pinned rustyvibes-v2
    /// checkout: the catalog, installation, provenance and license, honest
    /// coverage, no cross-key substitution, non-silent clips, release-sound
    /// semantics, a byte-identical rebuild, the untouched synthesized packs,
    /// and a simulated press/release run through the live engine. Steps that
    /// need a resource this host does not have are `NOT RUN`, never a pass.
    SoundpackAcceptanceTest,

    /// Verify that input survives keyboards being removed and reconnected
    ///
    /// Always runs a deterministic walk of the exact device state machine the
    /// live pipeline uses (two keyboards attached, one removed, one
    /// reconnected, keys pressed throughout). Steps that need physical
    /// hardware are reported as `NOT RUN` when no keyboard is readable -
    /// never as a pass.
    HotplugTest {
        /// Seconds to keep the live pipeline running
        #[arg(long, default_value_t = 5)]
        duration: u64,
    },

    /// Verify the engine recovers when the audio connection drops
    ///
    /// Always runs a deterministic walk of the exact reconnection state
    /// machine the engine supervisor drives (bounded backoff, finite retry
    /// budget, reset on recovery). The live half forces a real connection
    /// rebuild and checks sound still comes out; it is reported as
    /// `NOT RUN` when no PipeWire session is reachable.
    AudioRecoveryTest {
        /// Seconds to keep the pipeline alive after the recovery cycle
        #[arg(long, default_value_t = 3)]
        duration: u64,
    },

    /// Verify idle pausing costs nothing and a key press wakes it at once
    ///
    /// Stays quiet until the engine pauses the stream, shows that a paused
    /// stream is charged no audio callbacks, then presses a key and measures
    /// how long until a voice is scheduled. Reported as `NOT RUN` when no
    /// PipeWire session is reachable.
    IdleTest {
        /// Quiet seconds required before the stream is paused
        #[arg(long, default_value_t = 1)]
        idle_after: u64,
    },

    /// Prove a sound pack can be replaced while the stream keeps running
    ///
    /// Runs a sustained load, swaps Holy Panda for Linear underneath it, and
    /// verifies the stream never stopped, the input side switched at once,
    /// and the retired pack is released only once it can no longer be
    /// referenced. Reported as `NOT RUN` when no PipeWire session is
    /// reachable.
    PackSwitchTest,

    /// Prove the engine depends on nothing but evdev and PipeWire
    ///
    /// Scans every source file (comments stripped) and every Cargo.toml for
    /// session variables, compositor APIs, and desktop crates, checks the
    /// transitive dependency graph, then re-executes this binary with
    /// `DISPLAY`, `WAYLAND_DISPLAY` and friends unset to show keyboards are
    /// still found and sound still plays. Reported as `NOT RUN` when no
    /// PipeWire session is reachable.
    CompatibilityTest,

    /// Prove installed sound packs are discovered, selected, and playable
    ///
    /// Checks `pack list`, selection by name (case-insensitively), by file
    /// stem, and by path, the `--pack` / config / default precedence, that
    /// the global `--pack` flag parses on either side of the subcommand, and -
    /// when a PipeWire session is reachable - that every installed pack
    /// renders sound.
    PackTest,

    /// Prove the configuration file round-trips, validates, and applies
    ///
    /// Writes to a scratch path (`$KEYVIBES_CONFIG`), so a real
    /// `~/.config/keyvibes/config.toml` is never touched.
    ConfigTest,

    /// Prove KeyVibes never stores, logs, sends, grabs, or injects a key
    ///
    /// Audits every production source file (comments and test modules
    /// stripped) for a network client, a file write outside the config
    /// writer, a key-formatting site, a grab or injection ioctl, a
    /// privilege call, and a device outside `/dev/input`; checks the
    /// dependency graph for network and logging crates; then proves in
    /// process that pack paths cannot traverse the filesystem and that a
    /// corrupt pack fails with an error instead of a panic.
    SecurityTest,

    /// Prove every failure is actionable and nothing panics
    ///
    /// Runs this binary as a child process under a broken configuration, an
    /// empty pack directory, an unknown `--pack`, and missing files, and
    /// checks that each message names what failed, why, and how to fix it -
    /// never a panic, a stack trace, or a bare errno.
    DoctorTest,

    /// Prove the command-line interface itself is complete and coherent
    ///
    /// Walks every command this binary advertises, asks each one for its own
    /// help, and fails if any advertised command is missing, exits non-zero,
    /// or prints nothing - the check that keeps `keyvibes --help` honest.
    CliTest,

    /// Run for a long time and report anything that drifted
    ///
    /// Cycles through rapid typing, bursts, sustained chords, idle-to-active
    /// transitions and pack swaps under load, while a watchdog records
    /// panics, wedges, missed deadlines, XRUNs, resident-memory growth,
    /// thread growth and a stream that silently reconnected. Hardware-dependent
    /// steps are reported `NOT RUN` when this host has no such hardware.
    SoakTest {
        /// How long to run: `30m`, `45s` or `2h`
        #[arg(long, default_value = "30m")]
        duration: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print the effective settings and where each one came from
    Show,

    /// Print the path of the configuration file
    ///
    /// Followed by whether that file exists, so a script can branch on it.
    Path,

    /// Write the default configuration file
    Init {
        /// Replace an existing file instead of leaving it alone
        #[arg(long)]
        force: bool,
    },
}

/// Actions `keyvibes ctl` can send to a running engine.
#[derive(Subcommand, Debug)]
pub enum CtlAction {
    /// Check the engine is alive and print its state
    Ping,

    /// Print the engine's full state (same as ping, semantically a query)
    Status,

    /// Enable sound on key presses
    On,

    /// Silence all key-triggered sound without stopping the engine
    Off,

    /// Set the master volume (0.0 - 1.0)
    Volume {
        /// New volume
        value: f32,
    },

    /// Turn pitch and/or gain variation on or off
    Variation {
        /// Pitch deviation per press
        #[arg(long, conflicts_with_all = ["no_pitch", "gain", "no_gain"])]
        pitch: bool,
        /// Disable pitch deviation
        #[arg(long, conflicts_with_all = ["pitch", "gain", "no_gain"])]
        no_pitch: bool,
        /// Gain deviation per press
        #[arg(long, conflicts_with_all = ["pitch", "no_pitch", "no_gain"])]
        gain: bool,
        /// Disable gain deviation
        #[arg(long, conflicts_with_all = ["pitch", "no_pitch", "gain"])]
        no_gain: bool,
    },

    /// Toggle key-position stereo spatialization
    Spatial {
        /// `on` or `off`
        state: OnOff,
    },

    /// Toggle release (key-up) sounds
    Release {
        /// `on` or `off`
        state: OnOff,
    },

    /// Switch the active pack by name or path
    Pack {
        /// Pack name from `pack list`, or a path to a .kvpack
        selector: String,
    },

    /// Play a few keys through the running engine
    Preview {
        /// Pack to preview (default: the active pack)
        #[arg(long)]
        pack: Option<String>,
        /// Key names, e.g. `--keys A S D F` (default: a short demo run)
        #[arg(long, num_args = 1..)]
        keys: Option<Vec<String>>,
    },

    /// Follow engine state changes until interrupted (Ctrl+C)
    Watch,

    /// Run the read-only environment checks and print them
    ///
    /// Input access, PipeWire, packs, configuration: each item reports what
    /// was observed and - when relevant - an exact command that fixes it.
    /// Nothing is changed by running this.
    Diagnostics,

    /// Ask the engine to exit cleanly
    Shutdown,
}

/// `on` / `off` flag value for boolean toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnOff {
    /// Turn the feature on.
    On,
    /// Turn the feature off.
    Off,
}

impl std::str::FromStr for OnOff {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "on" | "1" | "true" | "yes" => Ok(OnOff::On),
            "off" | "0" | "false" | "no" => Ok(OnOff::Off),
            other => Err(format!(
                "expected `on` or `off`, found {other:?} (try `keyvibes ctl spatial on`)"
            )),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum PackAction {
    /// List every installed sound pack with its name, keys, and clips
    List,

    /// Build a .kvpack from a pack.toml manifest
    Build {
        /// Path to pack.toml
        manifest: PathBuf,

        /// Output .kvpack path
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Validate an existing .kvpack
    Validate {
        /// Path to the .kvpack file
        pack: PathBuf,
    },

    /// Inspect the header, metadata, keys, and clips of a .kvpack
    Inspect {
        /// Path to the .kvpack file
        pack: PathBuf,

        /// List every clip entry as well
        #[arg(long)]
        clips: bool,
    },
}
