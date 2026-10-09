//! Command-line interface.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "keyvibes")]
#[command(about = "Linux native low-latency keyboard sound engine", long_about = None)]
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

    /// Run subsystem diagnostics (PipeWire + keyboard capture)
    #[arg(long)]
    pub diagnostics: bool,

    /// List available keyboards
    #[arg(long)]
    pub list_keyboards: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Build, validate, and inspect .kvpack sound packs
    Pack {
        #[command(subcommand)]
        action: PackAction,
    },

    /// Load a pack and play keyboard sounds
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

    /// Measure real-time latency under sustained load and fail on any XRUN
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

    /// Discover keyboards and capture key presses for a few seconds
    InputTest {
        /// Capture duration in seconds
        #[arg(short = 't', long, default_value_t = 10)]
        seconds: u64,
    },

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
    /// stem, and by path, the `--pack` / config / default precedence, that the
    /// global `--pack` flag parses on either side of the subcommand, and -
    /// when a PipeWire session is reachable - that every installed pack
    /// renders sound.
    PackTest,

    /// Prove the configuration file round-trips, validates, and applies
    ///
    /// Writes to a scratch path (`$KEYVIBES_CONFIG`), so a real
    /// `~/.config/keyvibes/config.toml` is never touched.
    ConfigTest,

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
