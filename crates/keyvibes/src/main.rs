//! KeyVibes - Linux native low-latency keyboard sound engine.

use anyhow::Result;
use clap::Parser;

mod cli;
mod diagnostics;

use cli::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("KeyVibes v{}", env!("CARGO_PKG_VERSION"));
    println!("Phase 0: Repository structure initialized");

    Ok(())
}
