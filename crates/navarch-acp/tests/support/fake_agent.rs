//! Scripted, offline ACP stdio subprocess. Never invokes an agent or executes tools.
#![forbid(unsafe_code)]
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, oneshot, watch},
};
type Output = Arc<Mutex<tokio::io::Stdout>>;
async fn write(out: &Output, value: Value) -> std::io::Result<()> {
    let mut out = out.lock().await;
    out.write_all(format!("{value}\n").as_bytes()).await?;
    out.flush().await
}
fn options() -> Value {
    json!([
        {"id":"permission_selector","name":"Mode","category":"mode","type":"select","currentValue":"default","options":[{"value":"default","name":"Default"},{"value":"workspace-write","name":"Workspace"},{"value":"custom","name":"Custom"}]},
        {"id":"model_selector","name":"Model","category":"model","type":"select","currentValue":"fake","options":[{"group":"test","name":"Test","options":[{"value":"fake","name":"Fake"},{"value":"alternate","name":"Alternate"}]}]}
    ])
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("script path required")?);
    let script: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    if let Some(pid_file) = script["pid_file"].as_str() {
        std::fs::write(pid_file, std::process::id().to_string())?;
    }
    let state_path = path.with_extension("sessions.json");
    let mut sessions: HashMap<String, Value> = std::fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut cancellations: HashMap<String, watch::Sender<bool>> = HashMap::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    while let Some(line) = lines.next_line().await? {
        let request: Value = serde_json::from_str(&line)?;
        if let Some(log) = script["log"].as_str() {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)?,
                "{request}"
            )?;
        }
        let Some(method) = request["method"].as_str() else {
            if let Some(id) = request["id"].as_str()
                && let Some(tx) = pending.lock().await.remove(id)
            {
                let _ = tx.send(request["result"].clone());
            }
            continue;
        };
        let id = request["id"].clone();
        let params = &request["params"];
        let result = match method {
            "initialize" => {
                json!({"protocolVersion":1,"agentCapabilities":{"loadSession":script["load"].as_bool().unwrap_or(true),"sessionCapabilities":{"resume":if script["resume"].as_bool().unwrap_or(true){json!({})}else{Value::Null},"list":{}}},"agentInfo":{"name":"fake-acp-agent","version":"1.0"}})
            }
            "session/new" => {
                let sid = format!("session-{}", sessions.len() + 1);
                sessions.insert(
                    sid.clone(),
                    json!({"sessionId":sid,"cwd":params["cwd"],"title":"Fake session"}),
                );
                std::fs::write(&state_path, serde_json::to_vec(&sessions)?)?;
                json!({"sessionId":sid,"configOptions":options()})
            }
            "session/resume" | "session/load" => {
                if (method == "session/resume" && script["resume_error"] == true)
                    || !sessions.contains_key(params["sessionId"].as_str().unwrap_or(""))
                {
                    write(&out,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"cannot resume"}})).await?;
                    continue;
                }
                json!({"configOptions":options()})
            }
            "session/set_config_option" => json!({"configOptions":options()}),
            "session/list" => {
                let mut items: Vec<_> = sessions
                    .values()
                    .filter(|s| params.get("cwd").is_none_or(|cwd| cwd == &s["cwd"]))
                    .cloned()
                    .collect();
                items.sort_by_key(|s| s["sessionId"].as_str().unwrap_or("").to_owned());
                let offset = params["cursor"]
                    .as_str()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                let end =
                    (offset + script["page_size"].as_u64().unwrap_or(1) as usize).min(items.len());
                json!({"sessions":items.get(offset..end).unwrap_or_default(),"nextCursor":if end<items.len(){Some(end.to_string())}else{None}})
            }
            "session/cancel" => {
                if let Some(tx) = cancellations.get(params["sessionId"].as_str().unwrap_or("")) {
                    tx.send_replace(true);
                }
                continue;
            }
            "session/prompt" => {
                let sid = params["sessionId"]
                    .as_str()
                    .ok_or("missing session")?
                    .to_owned();
                let (cancel, mut cancelled) = watch::channel(false);
                cancellations.insert(sid.clone(), cancel);
                let out = out.clone();
                let pending = pending.clone();
                let counter = counter.clone();
                let script = script.clone();
                let path = path.clone();
                tokio::spawn(async move {
                    let work = async {
                        let steps = script["steps"]
                            .as_array()
                            .cloned()
                            .unwrap_or_else(|| vec![json!({"reply":"OK"})]);
                        for step in steps {
                            if let Some(text) = step["reply"].as_str() {
                                write(&out,json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})).await?;
                            }
                            if let Some(title) = step["tool"].as_str() {
                                write(&out,json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":title,"kind":"edit","status":"pending"}}})).await?;
                            }
                            if let Some(ms) = step["delay_ms"].as_u64() {
                                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            }
                            if step["crash"] == true && !path.with_extension("crashed").exists() {
                                std::fs::write(path.with_extension("crashed"), b"crashed")?;
                                std::process::exit(17);
                            }
                            if let Some(permission) = step.get("permission") {
                                let rid = format!(
                                    "permission-{}",
                                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                );
                                let (tx, rx) = oneshot::channel();
                                pending.lock().await.insert(rid.clone(), tx);
                                let mut params = permission.clone();
                                params["sessionId"] = json!(sid);
                                write(&out,json!({"jsonrpc":"2.0","id":rid,"method":"session/request_permission","params":params})).await?;
                                let answer = rx.await.unwrap_or(Value::Null);
                                write(&out,json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":answer.to_string()}}}})).await?;
                            }
                        }
                        Ok::<_, std::io::Error>("end_turn")
                    };
                    let reason = tokio::select! {r=work=>r.unwrap_or("refusal"),_=cancelled.changed()=>"cancelled"};
                    let _ = write(
                        &out,
                        json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":reason}}),
                    )
                    .await;
                });
                continue;
            }
            _ => {
                write(&out,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown method"}})).await?;
                continue;
            }
        };
        write(&out, json!({"jsonrpc":"2.0","id":id,"result":result})).await?;
    }
    Ok(())
}
