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
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    /// Create a pairing code.
    Pair {
        #[arg(long)]
        relay: Option<String>,
    },
    /// Join a hub using a pairing code.
    Join {
        code: String,
        #[arg(long)]
        relay: String,
    },
}

#[derive(Subcommand)]
enum RelayCommand {
    /// Serve opaque WebSocket routing (terminate TLS in front).
    Serve {
        #[arg(long)]
        listen: std::net::SocketAddr,
    },
}
async fn remote(command: Command) -> anyhow::Result<u8> {
    match command {
        Command::Pair { relay } => navarch_cli::remote::pair(relay).await,
        Command::Join { code, relay } => navarch_cli::remote::join(code, relay).await,
        Command::Relay {
            command: RelayCommand::Serve { listen },
        } => {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tokio::select! {result=navarch_relay::serve(listener)=>result?,result=tokio::signal::ctrl_c()=>result?};
            Ok(0)
        }
        _ => anyhow::bail!("invalid remote command"),
    }
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
        command => match remote(command).await {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error:#}");
                ExitCode::from(1)
            }
        },
    }
}
