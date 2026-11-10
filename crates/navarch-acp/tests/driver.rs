//! ACP boundary integration tests; every subprocess is the offline fake binary.
#![forbid(unsafe_code)]
#![cfg(feature = "testing")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use async_trait::async_trait;
use navarch_acp::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
struct Answer(PermissionDecision);
#[async_trait]
impl PermissionHandler for Answer {
    async fn request(&self, _request: PermissionRequest) -> PermissionDecision {
        self.0.clone()
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    driver: Arc<AcpDriver>,
    log: std::path::PathBuf,
}
impl Fixture {
    fn new(mut script: Value, answer: PermissionDecision) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("script.json");
        let log = temp.path().join("requests.jsonl");
        script["log"] = json!(log);
        std::fs::write(&path, script.to_string()).unwrap();
        let driver = Arc::new(AcpDriver::new(
            AdapterCommand {
                command: env!("CARGO_BIN_EXE_fake-acp-agent").into(),
                args: vec![path.to_string_lossy().into()],
            },
            Arc::new(Answer(answer)),
        ));
        Self { temp, driver, log }
    }
    fn req(&self) -> NewSession {
        NewSession {
            cwd: self.temp.path().to_owned(),
            mode: Some("custom".into()),
            model: Some("alternate".into()),
            mcp_servers: vec![],
        }
    }
    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    async fn prompt(&self, id: &str) -> Result<TurnOutcome, DriverError> {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            self.driver.prompt(&id.into(), "test", tx),
        )
        .await
        .unwrap()
    }
}
fn fixture(script: Value) -> Fixture {
    Fixture::new(script, PermissionDecision::Cancelled)
}
#[tokio::test]
async fn new_prompt_config_mcp_events_and_paginated_listing() {
    let f = fixture(json!({"steps":[{"reply":"hello "},{"tool":"Edit"},{"reply":"world"}]}));
    let mut req = f.req();
    req.mcp_servers = vec![McpServer {
        name: "mcp".into(),
        command: "/fake/server".into(),
        args: vec!["arg".into()],
        env: [("KEY".into(), "value".into())].into(),
    }];
    let handle = f.driver.new_session(req).await.unwrap();
    assert_eq!(handle.models, vec!["fake", "alternate"]);
    assert_eq!(handle.agent_version.as_deref(), Some("1.0"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = f.driver.prompt(&handle.session_id, "hi", tx).await.unwrap();
    assert_eq!(outcome.result, "world");
    assert_eq!(outcome.stop_reason, "end_turn");
    let mut events = vec![];
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(events.contains(&AgentEvent::ToolCall("Edit: pending".into())));
    f.driver.new_session(f.req()).await.unwrap();
    assert_eq!(
        f.driver
            .list_sessions(Some(f.temp.path()))
            .await
            .unwrap()
            .len(),
        2
    );
    let requests = f.requests();
    let new = requests
        .iter()
        .find(|r| r["method"] == "session/new")
        .unwrap();
    assert_eq!(new["params"]["mcpServers"][0]["name"], "mcp");
    assert_eq!(new["params"]["mcpServers"][0]["env"][0]["value"], "value");
    let configs: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/set_config_option")
        .collect();
    assert_eq!(configs[0]["params"]["configId"], "permission_selector");
    assert_eq!(configs[0]["params"]["value"], "custom");
    assert_eq!(configs[1]["params"]["configId"], "model_selector");
}
#[tokio::test]
async fn resume_from_new_driver_falls_back_to_load() {
    for (resume, error) in [(true, false), (false, false), (true, true)] {
        let f = fixture(json!({"resume":resume,"resume_error":error}));
        let id = f.driver.new_session(f.req()).await.unwrap().session_id;
        let driver = AcpDriver::new(
            AdapterCommand {
                command: env!("CARGO_BIN_EXE_fake-acp-agent").into(),
                args: vec![f.temp.path().join("script.json").to_string_lossy().into()],
            },
            Arc::new(Answer(PermissionDecision::Cancelled)),
        );
        driver
            .resume_session(ResumeSession {
                session_id: id,
                config: f.req(),
            })
            .await
            .unwrap();
        let requests = f.requests();
        assert!(requests.iter().any(|r| r["method"]
            == if !resume || error {
                "session/load"
            } else {
                "session/resume"
            }));
    }
}
#[tokio::test]
async fn crash_mid_turn_restarts_and_lazily_restores() {
    let f = fixture(json!({"steps":[{"reply":"before"},{"crash":true},{"reply":"after"}]}));
    let id = f.driver.new_session(f.req()).await.unwrap().session_id;
    assert!(f.prompt(&id).await.is_err());
    assert_eq!(f.prompt(&id).await.unwrap().result, "beforeafter");
    let requests = f.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "initialize")
            .count(),
        2
    );
    assert!(requests.iter().any(|r| r["method"] == "session/resume"));
}
#[tokio::test]
async fn cancel_running_prompt() {
    let f = fixture(json!({"steps":[{"reply":"started"},{"delay_ms":60000}]}));
    let id = f.driver.new_session(f.req()).await.unwrap().session_id;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let driver = f.driver.clone();
    let session = id.clone();
    let turn = tokio::spawn(async move { driver.prompt(&session, "hi", tx).await });
    assert_eq!(
        rx.recv().await.unwrap(),
        AgentEvent::MessageChunk("started".into())
    );
    f.driver.cancel(&id).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .stop_reason,
        "cancelled"
    );
}
fn permission() -> Value {
    json!({"toolCall":{"toolCallId":"edit-1","title":"Edit file","kind":"edit","locations":[{"path":"file.txt"}]},"options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"deny","name":"Deny","kind":"reject_once"}]})
}
#[tokio::test]
async fn permission_allow_deny_and_invalid_selection() {
    for selected in ["allow", "deny", "not-offered"] {
        let f = Fixture::new(
            json!({"steps":[{"permission":permission()}]}),
            PermissionDecision::Selected {
                option_id: selected.into(),
            },
        );
        let id = f.driver.new_session(f.req()).await.unwrap().session_id;
        let result = f.prompt(&id).await.unwrap().result;
        if selected == "not-offered" {
            assert!(result.contains("cancelled"));
        } else {
            assert!(result.contains(selected));
        }
    }
}
#[tokio::test]
async fn unsupported_config_and_restore_fail_clearly() {
    let f = fixture(json!({"resume":false,"load":false}));
    let mut req = f.req();
    req.mode = Some("missing".into());
    assert!(matches!(
        f.driver.new_session(req).await,
        Err(DriverError::Unsupported(_))
    ));
    let id = f.driver.new_session(f.req()).await.unwrap().session_id;
    assert!(matches!(
        f.driver
            .resume_session(ResumeSession {
                session_id: id,
                config: f.req()
            })
            .await,
        Err(DriverError::Unsupported(_))
    ));
}
#[tokio::test]
async fn result_truncates_on_utf8_boundary() {
    let f = fixture(json!({"steps":[{"reply":"界".repeat(30000)}]}));
    let id = f.driver.new_session(f.req()).await.unwrap().session_id;
    let result = f.prompt(&id).await.unwrap().result;
    assert_eq!(result.len(), 65535);
}

#[cfg(unix)]
#[tokio::test]
async fn killed_adapter_between_turns_resumes_on_next_call() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pid");
    let f = fixture(json!({"pid_file":pid_file}));
    let id = f.driver.new_session(f.req()).await.unwrap().session_id;
    assert_eq!(f.prompt(&id).await.unwrap().result, "OK");
    let pid = std::fs::read_to_string(pid_file).unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-KILL", pid.trim()])
            .status()
            .unwrap()
            .success()
    );
    // Wait for the SDK's child monitor to observe process termination, without
    // issuing a probe that could itself start a replacement adapter.
    for _ in 0..100 {
        if !std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.prompt(&id).await.unwrap().result, "OK");
    assert!(f.requests().iter().any(|r| r["method"] == "session/resume"));
}

#[tokio::test]
async fn missing_executable_reports_startup_failure() {
    let f = fixture(json!({}));
    let driver = AcpDriver::new(
        AdapterCommand {
            command: "/missing-navarch-test-adapter".into(),
            args: vec![],
        },
        Arc::new(Answer(PermissionDecision::Cancelled)),
    );
    let error = driver.new_session(f.req()).await.unwrap_err().to_string();
    assert!(error.contains("/missing-navarch-test-adapter"), "{error}");
}

#[tokio::test]
async fn listing_follows_pages_stops_at_500_and_preserves_metadata() {
    let f = fixture(json!({"page_size":180}));
    let sessions: std::collections::HashMap<_,_> = (0..505).map(|i| {
        let id = format!("external-{i:03}");
        (id.clone(), json!({"sessionId":id,"cwd":f.temp.path(),"title":"External work","updatedAt":"2026-11-10T00:00:00Z"}))
    }).collect();
    std::fs::write(
        f.temp.path().join("script.sessions.json"),
        serde_json::to_vec(&sessions).unwrap(),
    )
    .unwrap();
    let list = f.driver.list_sessions(Some(f.temp.path())).await.unwrap();
    assert_eq!(list.len(), 500);
    assert_eq!(list[0].title.as_deref(), Some("External work"));
    assert_eq!(list[0].updated_at.as_deref(), Some("2026-11-10T00:00:00Z"));
    let requests = f.requests();
    let pages: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/list")
        .collect();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[1]["params"]["cursor"], "180");
    assert_eq!(pages[2]["params"]["cursor"], "360");
}
