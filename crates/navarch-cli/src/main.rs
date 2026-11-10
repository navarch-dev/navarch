//! The Navarch command-line entry point.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "navarch",
    version,
    about = "Coordinate coding agents across machines"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the hub.
    Hub,
    /// Run a worker.
    Worker,
    /// Run the relay.
    Relay,
    /// Create a pairing code.
    Pair,
    /// Join a hub using a pairing code.
    Join,
}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    eprintln!("not implemented yet");
    ExitCode::from(2)
}
