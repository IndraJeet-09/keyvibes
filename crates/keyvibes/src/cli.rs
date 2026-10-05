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
        /// Path to the .kvpack file
        #[arg(long)]
        pack: PathBuf,

        /// Audio output sample rate
        #[arg(long, default_value_t = 48000)]
        rate: u32,
    },

    /// Discover keyboards and capture key presses for a few seconds
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
}

#[derive(Subcommand, Debug)]
pub enum PackAction {
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
