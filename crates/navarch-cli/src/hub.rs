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
    /// Show known workers and online status.
    Status {
        /// Emit JSON instead of a readable table.
        #[arg(long)]
        json: bool,
    },
    /// Show task-card summaries.
    Tasks {
        /// Emit JSON instead of a readable table.
        #[arg(long)]
        json: bool,
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
        HubCommand::Status { json } => {
            let value = control::call(&socket, &Request::ListWorkers).await?;
            println!("{}", render(&value, json, false)?);
        }
        HubCommand::Tasks { all, json } => {
            let value = control::call(
                &socket,
                &Request::ListTasks {
                    include_finished: all,
                },
            )
            .await?;
            println!("{}", render(&value, json, true)?);
        }
    }
    Ok(0)
}

fn render(value: &serde_json::Value, json: bool, tasks: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    let mut rows = vec![
        if tasks {
            vec!["CARD", "STATE", "STEPS", "TITLE"]
        } else {
            vec!["MACHINE", "STATE", "ONLINE", "NAME"]
        }
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    ];
    let items = if tasks { &value["tasks"] } else { value };
    for item in items.as_array().into_iter().flatten() {
        let text = |v: &serde_json::Value| {
            v.as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        rows.push(if tasks {
            vec![
                text(&item["id"]),
                text(&item["state"]),
                item["step_count"].to_string(),
                text(&item["title"]),
            ]
        } else {
            vec![
                text(&item["machine"]["id"]),
                text(&item["machine"]["state"]),
                if item["online"] == true { "yes" } else { "no" }.into(),
                text(&item["machine"]["display_name"]),
            ]
        });
    }
    let widths: Vec<_> = (0..4)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut output = rows
        .into_iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| {
                    format!(
                        "{cell}{}",
                        " ".repeat(if i == 3 {
                            0
                        } else {
                            widths[i] - cell.chars().count() + 2
                        })
                    )
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(corrupt) = value["corrupt"].as_array() {
        for error in corrupt {
            output.push_str(&format!(
                "\nWarning: {}",
                error.as_str().unwrap_or("unreadable card")
            ));
        }
    }
    Ok(output)
}
