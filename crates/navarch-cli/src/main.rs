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
    Hub {
        #[command(subcommand)]
        command: navarch_cli::hub::HubCommand,
    },
    /// Run a worker.
    Worker {
        #[command(subcommand)]
        command: navarch_cli::worker::WorkerCommand,
    },
    /// Run the relay.
    Relay,
    /// Create a pairing code.
    Pair,
    /// Join a hub using a pairing code.
    Join,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Hub { command } => match navarch_cli::hub::run(command).await {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error:#}");
                ExitCode::from(1)
            }
        },
        Command::Worker { command } => match navarch_cli::worker::run(command).await {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error:#}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("not implemented yet");
            ExitCode::from(2)
        }
    }
}
