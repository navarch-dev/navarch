//! Hub command parsing, rendering and daemon startup wiring.
mod args;
mod dispatch;
mod render;
mod wait;
use anyhow::Result;
pub use args::{HubCommand, MachineAction};
use navarch_crypto::Identity;
use navarch_hub::control::{self, Request};
use navarch_store::FileStore;
use navarch_transport::LocalHello;
use serde_json::json;
use std::{path::Path, sync::Arc};

/// Execute a hub command against the local daemon.
pub async fn run(command: HubCommand) -> Result<u8> {
    if let HubCommand::Guide { json } = command {
        return guide(json);
    }
    let home = crate::home::home()?;
    execute(&home.join("hub.sock"), &home, command).await
}
async fn execute(socket: &Path, home: &Path, command: HubCommand) -> Result<u8> {
    match command {
        HubCommand::Guide { json } => return guide(json),
        HubCommand::Init => navarch_hub::init(home)?,
        HubCommand::Serve => {
            let identity = Identity::load_or_create(home)?;
            let store = Arc::new(FileStore::open(home.join("hub"))?);
            let hello = LocalHello {
                public_key: identity.public_key(),
                display_name: "hub".into(),
            };
            let config = crate::remote::HubConfig::load(home)?;
            let identity = Arc::new(identity);
            let key = Arc::new(navarch_crypto::StaticKey::load_or_create(home)?);
            let remote = config.relay.map(|url| navarch_hub::RemoteRelay {
                url,
                identity,
                static_key: key.clone(),
            });
            let direct = config.direct.map(|config| navarch_hub::RemoteDirect {
                config,
                static_key: key,
            });
            tokio::select! { result = navarch_hub::serve_with_links(home,hello,store,remote,direct) => result?, result = tokio::signal::ctrl_c() => result? }
        }
        HubCommand::Watch { json } => {
            let mut subscription = control::subscribe(socket, None).await?;
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

        HubCommand::Dispatch(args) => return dispatch::dispatch(socket, args).await,
        HubCommand::Send(args) => return dispatch::send(socket, args).await,
        HubCommand::Cancel {
            card_id,
            task,
            json,
        } => return dispatch::cancel(socket, card_id, task, json).await,
        HubCommand::Task { card_id, log, json } => {
            let value = dispatch::get_task(socket, card_id, log).await?;
            println!("{}", render::task(&value, json)?);
        }
        HubCommand::Sessions {
            machine,
            agent,
            cwd,
            limit,
            json,
        } => {
            let value = control::call(
                socket,
                &Request::ListSessions {
                    machine,
                    agent,
                    cwd,
                    limit,
                },
            )
            .await?;
            println!("{}", render::records(&value, json, true)?);
        }
        HubCommand::Inbox { peek, json } => {
            let request = if peek {
                Request::WaitForEvents {
                    timeout_secs: Some(0),
                    after_seq: None,
                    include_read: false,
                }
            } else {
                Request::ReadInbox
            };
            let value = control::call(socket, &request).await?;
            println!("{}", render::records(&value, json, false)?);
        }
        HubCommand::Wait(args) => {
            // Fail clearly when initially absent; reconnect only once a wait has started.
            control::call(socket, &Request::ListWorkers).await?;
            if let Some(id) = args.card {
                dispatch::get_task(socket, id, false).await?;
            }
            let (value, code) = wait::run(socket, args.card, args.after_seq, args.timeout).await?;
            println!(
                "{}",
                render::outcome(&value, args.json, args.card.is_some())?
            );
            return Ok(code);
        }
        HubCommand::Permission {
            request_id,
            choose,
            cancel: _,
            json,
        } => {
            let decision = choose
                .map(|option_id| navarch_proto::PermissionDecision::Selected { option_id })
                .unwrap_or(navarch_proto::PermissionDecision::Cancelled);
            let request_id = request_id.parse()?;
            let value = control::call(
                socket,
                &Request::AnswerPermission {
                    request_id,
                    decision,
                },
            )
            .await?;
            println!("{}", render::outcome(&value, json, false)?);
        }
        HubCommand::Machines { action, json } => match action {
            None => {
                let value = control::call(socket, &Request::ListWorkers).await?;
                println!("{}", render::table(&value, json, false)?);
            }
            Some(action) => {
                let (request, message) = match action {
                    MachineAction::Approve { id } => (
                        Request::ApproveMachine {
                            machine: id.clone(),
                        },
                        format!("Approved machine {id}."),
                    ),
                    MachineAction::Revoke { id } => (
                        Request::RevokeMachine {
                            machine: id.clone(),
                        },
                        format!("Revoked machine {id}."),
                    ),
                };
                let value = control::call(socket, &request).await?;
                if json {
                    println!("{}", serde_json::to_string(&value)?);
                } else {
                    println!("{message}");
                }
            }
        },
        HubCommand::Status { json } => {
            let value = control::call(socket, &Request::ListWorkers).await?;
            println!("{}", render::table(&value, json, false)?);
        }
        HubCommand::Tasks { all, json } => {
            let value = control::call(
                socket,
                &Request::ListTasks {
                    include_finished: all,
                },
            )
            .await?;
            println!("{}", render::table(&value, json, true)?);
        }
    }
    Ok(0)
}
fn guide(json: bool) -> Result<u8> {
    let guide = include_str!("../../../../docs/orchestrator.md");
    if json {
        println!("{}", serde_json::to_string(&json!({"guide":guide}))?);
    } else {
        print!("{guide}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests;
