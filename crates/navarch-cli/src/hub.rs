//! Hub command parsing and startup wiring.
use anyhow::Result;
use clap::Subcommand;
use navarch_crypto::Identity;
use navarch_hub::control::{self, Request};
use navarch_store::FileStore;
use navarch_transport::LocalHello;
use std::sync::Arc;
/// Hub commands.
#[derive(Subcommand)]
pub enum HubCommand {
    /// Serve the local hub daemon and worker/control sockets.
    Serve,
    /// Serve MCP over stdio, connecting to the running daemon.
    Mcp,
    /// Show known workers, online status and capabilities as JSON.
    Status,
    /// Show task cards as JSON.
    Tasks {
        /// Include finished, cancelled, rejected and failed cards.
        #[arg(long)]
        all: bool,
    },
    /// Write commented example configuration; never overwrite existing files.
    Init,
}
/// Execute one hub command.
pub async fn run(command: HubCommand) -> Result<u8> {
    let home = crate::home::home()?;
    let socket = home.join("hub.sock");
    match command {
        HubCommand::Init => navarch_hub::init(&home)?,
        HubCommand::Serve => {
            let identity = Identity::load_or_create(&home)?;
            let store = Arc::new(FileStore::open(home.join("hub"))?);
            let hello = LocalHello {
                public_key: identity.public_key(),
                display_name: "hub".into(),
            };
            tokio::select! { result = navarch_hub::serve(&home,hello,store) => result?, result = tokio::signal::ctrl_c() => result? }
        }
        HubCommand::Mcp => navarch_hub::serve_mcp(&socket).await?,
        HubCommand::Status => println!(
            "{}",
            serde_json::to_string_pretty(&control::call(&socket, &Request::ListWorkers).await?)?
        ),
        HubCommand::Tasks { all } => println!(
            "{}",
            serde_json::to_string_pretty(
                &control::call(
                    &socket,
                    &Request::ListTasks {
                        include_finished: all
                    }
                )
                .await?
            )?
        ),
    }
    Ok(0)
}
