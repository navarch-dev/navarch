use super::{args::*, render, wait};
use anyhow::{Context, Result, bail};
use navarch_hub::control::{self, Request};
use navarch_proto::CardId;
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read},
    path::Path,
};

pub(super) fn read_bounded(reader: impl Read, limit: usize) -> Result<String> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("input exceeds {limit} bytes; shorten the prompt or file");
    }
    String::from_utf8(bytes).context("input must be UTF-8")
}
fn prompt(args: PromptArgs) -> Result<String> {
    let value = match (args.prompt, args.prompt_file) {
        (Some(_), Some(_)) => bail!("supply exactly one prompt source"),
        (None, Some(path)) => read_bounded(std::fs::File::open(path)?, 256 * 1024)?,
        (Some(text), None) if text != "-" => text,
        (text, None) => {
            if text.is_none() && std::io::stdin().is_terminal() {
                bail!("supply a prompt, --prompt-file, or stdin");
            }
            read_bounded(std::io::stdin().lock(), 256 * 1024)?
        }
    };
    if value.trim().is_empty() || value.len() > 256 * 1024 {
        bail!("prompt must be non-empty and at most 256 KiB");
    }
    Ok(value)
}
pub(super) async fn dispatch(socket: &Path, args: DispatchArgs) -> Result<u8> {
    let mcp_servers = args
        .mcp_servers
        .map(|file| -> Result<_> {
            let text = read_bounded(std::fs::File::open(file)?, 64 * 1024)?;
            serde_json::from_str(&text).context("invalid worker MCP servers JSON")
        })
        .transpose()?;
    let request = Request::Dispatch {
        card_id: args.card,
        machine: args.machine,
        agent: args.agent,
        role: args.role,
        cwd: args.cwd,
        prompt: prompt(args.prompt)?,
        title: args.title,
        project: args.project,
        mode: args.mode,
        model: args.model,
        mcp_servers,
    };
    submit(socket, request, args.follow).await
}
pub(super) async fn send(socket: &Path, mut args: SendArgs) -> Result<u8> {
    let (card_id, machine, session_id, agent) = if let Some(machine) = args.machine {
        // The first optional positional is prompt text in direct-session mode.
        if let Some(text) = args.card_id {
            if args.prompt.prompt.is_some() || args.prompt.prompt_file.is_some() {
                bail!("supply exactly one prompt source");
            }
            args.prompt.prompt = Some(text);
        }
        (
            args.card,
            machine,
            args.session.context("--session is required")?,
            args.agent,
        )
    } else {
        if args.session.is_some() || args.agent.is_some() || args.card.is_some() {
            bail!("direct session options require --machine and --session");
        }
        let id: CardId = args.card_id.context("card ID is required")?.parse()?;
        let card = get_task(socket, id, false).await?;
        let step = card["steps"]
            .as_array()
            .and_then(|s| s.last())
            .context("card has no steps")?;
        // A worker session ID is already unambiguous; agent is needed only for adapter IDs.
        let (session, agent) = if let Some(session) = step["session"].as_str() {
            (session.to_owned(), None)
        } else {
            (
                step["adapter_session_id"]
                    .as_str()
                    .context("latest step has no session yet")?
                    .to_owned(),
                Some(
                    step["agent"]
                        .as_str()
                        .context("step has no agent")?
                        .to_owned(),
                ),
            )
        };
        (
            Some(id),
            serde_json::from_value(step["machine"].clone())?,
            session,
            agent,
        )
    };
    submit(
        socket,
        Request::SendToSession {
            card_id,
            machine,
            session_id,
            agent,
            prompt: prompt(args.prompt)?,
        },
        args.follow,
    )
    .await
}
async fn submit(socket: &Path, request: Request, follow: FollowArgs) -> Result<u8> {
    let value = control::call(socket, &request).await?;
    let (value, code) = if follow.wait {
        let id = serde_json::from_value(value["card_id"].clone())?;
        let seq = value["seq"]
            .as_u64()
            .context("dispatch response missing cursor")?;
        let (mut outcome, code) = wait::run(socket, Some(id), Some(seq), follow.timeout).await?;
        outcome["card_id"] = value["card_id"].clone();
        outcome["task_id"] = value["task_id"].clone();
        (outcome, code)
    } else {
        (value, 0)
    };
    println!("{}", render::outcome(&value, follow.json, follow.wait)?);
    Ok(code)
}
pub(super) async fn get_task(socket: &Path, id: CardId, log: bool) -> Result<Value> {
    Ok(control::call(
        socket,
        &Request::GetTask {
            task_id: id,
            include_log: log,
        },
    )
    .await?)
}
pub(super) async fn cancel(
    socket: &Path,
    card: Option<CardId>,
    task: Option<navarch_proto::TaskId>,
    json_output: bool,
) -> Result<u8> {
    let task = if let Some(task) = task {
        Some(task)
    } else {
        let card = get_task(socket, card.context("supply a card or --task")?, false).await?;
        let steps = card["steps"].as_array().context("card has no steps")?;
        // Accepted queued turns also look running to the hub. Prefer the oldest
        // active turn, so a continuation cannot mask the turn ahead of it.
        steps
            .iter()
            .find(|s| matches!(s["state"].as_str(), Some("running" | "waiting")))
            .or_else(|| steps.iter().find(|s| s["state"] == "queued"))
            .map(|s| serde_json::from_value(s["task_id"].clone()))
            .transpose()?
    };
    let value = if let Some(task_id) = task {
        control::call(socket, &Request::Cancel { task_id }).await?;
        json!({"task_id":task_id,"queued":true})
    } else {
        json!({"queued":false,"message":"Nothing running."})
    };
    println!("{}", render::outcome(&value, json_output, false)?);
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn bounded_prompt_reading_rejects_empty_invalid_utf8_and_excess() {
        assert_eq!(read_bounded("界".as_bytes(), 3).unwrap(), "界");
        assert!(read_bounded("界".as_bytes(), 2).is_err());
        assert!(read_bounded([255].as_slice(), 20).is_err());
        assert!(
            prompt(PromptArgs {
                prompt: Some(" ".into()),
                prompt_file: None
            })
            .is_err()
        );
        assert!(
            prompt(PromptArgs {
                prompt: Some("x".repeat(256 * 1024 + 1)),
                prompt_file: None
            })
            .is_err()
        );
        assert!(
            prompt(PromptArgs {
                prompt: Some("hi".into()),
                prompt_file: Some("unused".into())
            })
            .is_err()
        );
    }
}
