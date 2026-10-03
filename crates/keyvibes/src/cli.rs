//! Command-line interface.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "keyvibes")]
#[command(about = "Linux native low-latency keyboard sound engine", long_about = None)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Run diagnostics
    #[arg(long)]
    pub diagnostics: bool,

    /// List available keyboards
    #[arg(long)]
    pub list_keyboards: bool,

    /// List installed sound packs
    #[arg(long)]
    pub list_packs: bool,

    /// Run audio test
    #[arg(long)]
    pub audio_test: bool,

    /// Run input test
    #[arg(long)]
    pub input_test: bool,

    /// Run self-test
    #[arg(long)]
    pub selftest: bool,

    /// Run benchmarks
    #[arg(long)]
    pub bench: bool,
}
