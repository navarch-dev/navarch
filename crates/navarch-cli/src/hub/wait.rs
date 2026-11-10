//! Shared wait loop, cursor advancement, reconnection and outcome projection.
use anyhow::{Context, Result};
use navarch_hub::{
    HubError,
    control::{self, Request},
};
use navarch_proto::CardId;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::time::Instant;

fn transient(error: &HubError) -> bool {
    matches!(error, HubError::Unavailable(_) | HubError::Io(_))
}
async fn reconnecting_call(socket: &Path, request: &Request) -> Result<Value> {
    let mut delay = Duration::from_millis(100);
    loop {
        match control::call(socket, request).await {
            Ok(value) => return Ok(value),
            Err(error) if transient(&error) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            Err(error) => return Err(error.into()),
        }
    }
}
/// Return one bounded batch/outcome and the corresponding process exit code.
pub(super) async fn run(
    socket: &Path,
    card: Option<CardId>,
    after: Option<u64>,
    timeout: Option<u64>,
) -> Result<(Value, u8)> {
    let seconds = timeout.unwrap_or(300);
    let mut cursor = after.unwrap_or(0);
    let deadline = if seconds == 0 {
        None
    } else {
        Some(
            Instant::now()
                .checked_add(Duration::from_secs(seconds))
                .context("timeout is too large")?,
        )
    };
    let work = async {
        loop {
            // Reissue after the daemon's 1800-second cap; cursor excludes unrelated events.
            let value = reconnecting_call(
                socket,
                &Request::WaitForEvents {
                    timeout_secs: Some(1800),
                    after_seq: Some(cursor),
                    include_read: true,
                },
            )
            .await?;
            let events = value.as_array().context("expected an event array")?;
            for event in events {
                let seq = cursor.max(event["seq"].as_u64().context("event missing sequence")?);
                if card.is_some_and(|id| event["card_id"] != id.to_string()) {
                    cursor = seq;
                    continue;
                }
                let kind = event["kind"].as_str().unwrap_or("");
                if card.is_some()
                    && !matches!(kind, "finished" | "failed" | "rejected" | "permission_ask")
                {
                    cursor = seq;
                    continue;
                }
                let id: Option<CardId> = serde_json::from_value(event["card_id"].clone())?;
                let mut output = json!({"events":[event],"seq":seq,"card_id":id,
                    "task_id":event["step"],"outcome":kind});
                let mut code = match kind {
                    "failed" | "rejected" => 3,
                    "permission_ask" => 4,
                    _ => 0,
                };
                if let Some(id) = id {
                    let task = reconnecting_call(
                        socket,
                        &Request::GetTask {
                            task_id: id,
                            include_log: false,
                        },
                    )
                    .await?;
                    if let Some(step) = task["steps"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|s| s["task_id"] == event["step"])
                    {
                        output["result"] = step["result_summary"].clone();
                        if step["state"] == "cancelled" {
                            output["outcome"] = json!("cancelled");
                            code = 3;
                        }
                    }
                    if kind == "permission_ask" {
                        // A concurrent answer/finish makes this hint obsolete; keep waiting.
                        let permission = task["permission_requests"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|p| p["task_id"] == event["step"]);
                        let Some(permission) = permission else {
                            cursor = seq;
                            continue;
                        };
                        output["permission"] = permission.clone();
                    }
                }
                if card.is_none() {
                    output["events"] = value.clone();
                    output["seq"] =
                        json!(events.last().and_then(|e| e["seq"].as_u64()).unwrap_or(seq));
                }
                return Ok((output, code));
            }
        }
    };
    // Drop in-flight calls/backoff on timeout, while leaving the worker's task running.
    if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline, work).await {
            Ok(result) => result,
            Err(_) => Ok((
                json!({"events":[],"seq":cursor,"card_id":card,"outcome":"timeout"}),
                124,
            )),
        }
    } else {
        work.await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn timeout_includes_backoff_and_does_not_cancel_work() {
        let temp = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let (value, code) = run(&temp.path().join("absent.sock"), None, Some(42), Some(2))
            .await
            .unwrap();
        assert_eq!(code, 124);
        assert_eq!(value["seq"], 42);
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }
    #[tokio::test]
    async fn renews_capped_requests_and_advances_past_unrelated_events() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let card = CardId::new();
        let other = CardId::new();
        let step = navarch_proto::TaskId::new();
        let (seen, first) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut seen = Some(seen);
            let mut gate = Some(gate);
            for (index, after) in [7, 7, 8, 0].into_iter().enumerate() {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                if index < 3 {
                    assert_eq!(request["timeout_secs"], 1800);
                    assert_eq!(request["after_seq"], after);
                    assert_eq!(request["include_read"], true);
                }
                let value = match index {
                    0 => {
                        seen.take().unwrap().send(()).unwrap();
                        gate.take().unwrap().await.unwrap();
                        json!([])
                    }
                    1 => json!([{"seq":8,"card_id":other,"kind":"finished"}]),
                    2 => json!([{"seq":9,"card_id":card,"step":step,"kind":"finished"}]),
                    _ => {
                        assert_eq!(request["type"], "get_task");
                        json!({"steps":[{"task_id":step,"state":"finished","result_summary":"done"}]})
                    }
                };
                write
                    .write_all(format!("{}\n", json!({"status":"ok","value":value})).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let waiting =
            tokio::spawn(
                async move { run(&socket, Some(card), Some(7), Some(3601)).await.unwrap() },
            );
        first.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1800)).await;
        tokio::time::resume();
        release.send(()).unwrap();
        let (value, code) = waiting.await.unwrap();
        assert_eq!(code, 0);
        assert_eq!(value["seq"], 9);
        assert_eq!(value["result"], "done");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn timeout_during_result_fetch_retains_the_previous_cursor() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let card = CardId::new();
        let (entered, fetching) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            write
                .write_all(
                    format!(
                        "{}\n",
                        json!({"status":"ok","value":[
                {"seq":8,"card_id":card,"kind":"finished"}]})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            entered.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let waiting =
            tokio::spawn(async move { run(&socket, Some(card), Some(7), Some(2)).await.unwrap() });
        fetching.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        let (value, code) = waiting.await.unwrap();
        tokio::time::resume();
        assert_eq!(code, 124);
        assert_eq!(value["seq"], 7);
        server.abort();
    }
}
