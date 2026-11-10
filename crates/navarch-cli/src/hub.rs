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
    Mcp {
        /// Send Claude Code research-preview channel notifications.
        #[arg(long)]
        channel: bool,
    },
    /// Follow unread replay and live hub events until interrupted.
    Watch {
        /// Print raw JSON lines.
        #[arg(long)]
        json: bool,
    },
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
    /// List machines or change approval state.
    Machines {
        /// Emit the machine list as JSON.
        #[arg(long)]
        json: bool,
        /// Optional approval action.
        #[command(subcommand)]
        action: Option<MachineAction>,
    },
    /// Write commented example configuration; never overwrite existing files.
    Init,
}
/// Machine approval commands.
#[derive(Subcommand)]
pub enum MachineAction {
    /// Approve a pending machine.
    Approve {
        /// Machine ID.
        id: navarch_proto::MachineId,
    },
    /// Immediately revoke a known machine.
    Revoke {
        /// Machine ID.
        id: navarch_proto::MachineId,
    },
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
            let config = crate::remote::HubConfig::load(&home)?;
            let identity = Arc::new(identity);
            let key = Arc::new(navarch_crypto::StaticKey::load_or_create(&home)?);
            let remote = config.relay.map(|url| navarch_hub::RemoteRelay {
                url,
                identity,
                static_key: key.clone(),
            });
            let direct = config.direct.map(|config| navarch_hub::RemoteDirect {
                config,
                static_key: key,
            });
            tokio::select! { result = navarch_hub::serve_with_links(&home,hello,store,remote,direct) => result?, result = tokio::signal::ctrl_c() => result? }
        }
        HubCommand::Machines { action, json } => match action {
            None => {
                let value = control::call(&socket, &Request::ListWorkers).await?;
                println!("{}", render(&value, json, false)?);
            }
            Some(MachineAction::Approve { id }) => {
                control::call(
                    &socket,
                    &Request::ApproveMachine {
                        machine: id.clone(),
                    },
                )
                .await?;
                println!("Approved machine {id}.");
            }
            Some(MachineAction::Revoke { id }) => {
                control::call(
                    &socket,
                    &Request::RevokeMachine {
                        machine: id.clone(),
                    },
                )
                .await?;
                println!("Revoked machine {id}.");
            }
        },
        HubCommand::Mcp { channel } => {
            navarch_hub::serve_mcp_with_channel(
                &socket,
                channel || std::env::var("NAVARCH_CHANNEL").is_ok_and(|s| s == "1"),
            )
            .await?
        }
        HubCommand::Watch { json } => {
            let mut subscription = control::subscribe(&socket, None).await?;
            loop {
                let event = tokio::select! { event = subscription.recv() => event?, result = tokio::signal::ctrl_c() => { result?; break; } };
                match event {
                    Some(control::StreamEvent::Event(event)) => {
                        if json {
                            println!("{}", serde_json::to_string(&event)?);
                        } else {
                            println!(
                                "{} {} {} card={} {}",
                                event.seq,
                                event.kind,
                                event.machine,
                                event
                                    .card_id
                                    .map(|id| id.to_string())
                                    .unwrap_or_else(|| "-".into()),
                                event
                                    .summary
                                    .split_whitespace()
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            );
                        }
                    }
                    Some(control::StreamEvent::Lagged) => {
                        if json {
                            println!("{{\"kind\":\"lagged\"}}");
                        } else {
                            eprintln!("Event stream lagged; re-read the inbox and restart watch.");
                        }
                        break;
                    }
                    None => break,
                }
            }
        }
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
            vec!["ID", "STATE", "ONLINE", "NAME", "LINK"]
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
                text(&item["connection"]),
            ]
        });
    }
    let columns = if tasks { 4 } else { 5 };
    let widths: Vec<_> = (0..columns)
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
                        " ".repeat(if i + 1 == columns {
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
