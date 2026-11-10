//! Built CLI against the real daemon and a FakeDriver worker over LocalLink.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
#[path = "support/daemon.rs"]
mod daemon;
use daemon::{Daemon, permission};
use navarch_acp::FakeStep;
use navarch_proto::{CardId, PermissionDecision};
use navarch_store::Store;
use serde_json::Value;
use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn commands_dispatch_results_sessions_handoffs_and_inbox_semantics() {
    let f = Daemon::new(vec![
        vec![FakeStep::Reply("first\ncomplete result".into())],
        vec![FakeStep::Reply("continued".into())],
        vec![FakeStep::Reply("handoff".into())],
    ])
    .await;
    let first = f.dispatch_cli(&["--json"], 0).await;
    let id: CardId = serde_json::from_value(first["card_id"].clone()).unwrap();
    f.card(id, "finished").await;
    let card = first["card_id"].as_str().unwrap();
    let task = f.cli(&["task", card, "--log", "--json"], 0).await;
    assert_eq!(task["steps"][0]["result_summary"], "first\ncomplete result");
    assert!(!task["log"].as_array().unwrap().is_empty());
    let readable = f.command(&["task", card]).output().await.unwrap();
    assert!(
        String::from_utf8(readable.stdout)
            .unwrap()
            .contains("first\ncomplete result")
    );
    let sent = f
        .cli(&["send", card, "--wait", "--json", "continue"], 0)
        .await;
    assert_eq!(sent["card_id"], first["card_id"]);
    assert_eq!(sent["result"], "continued");
    let updated = f.cli(&["task", card, "--json"], 0).await;
    assert_eq!(
        updated["steps"][0]["session"],
        updated["steps"][1]["session"]
    );
    let handoff = f
        .dispatch_cli(&["--card", card, "--wait", "--json"], 0)
        .await;
    assert_eq!(handoff["card_id"], first["card_id"]);
    assert_eq!(handoff["result"], "handoff");
    for args in [
        vec!["status", "--json"],
        vec!["machines", "--json"],
        vec!["tasks", "--all", "--json"],
        vec![
            "sessions",
            "--machine",
            &f.machine,
            "--agent",
            "fake",
            "--cwd",
            f.home.path().to_str().unwrap(),
            "--limit",
            "2",
            "--json",
        ],
    ] {
        let value = f.cli(&args, 0).await;
        assert!(value.is_object() || value.is_array());
    }
    let before = f.store.inbox(false).unwrap().len();
    let peek = f.cli(&["inbox", "--peek", "--json"], 0).await;
    assert_eq!(peek.as_array().unwrap().len(), before);
    assert_eq!(f.store.inbox(false).unwrap().len(), before);
    let read = f.cli(&["inbox", "--json"], 0).await;
    assert_eq!(read.as_array().unwrap().len(), before);
    assert!(f.store.inbox(false).unwrap().is_empty());
    assert_eq!(f.cli(&["cancel", card, "--json"], 0).await["queued"], false);
    let guide = f.cli(&["guide", "--json"], 0).await;
    assert!(
        guide["guide"]
            .as_str()
            .unwrap()
            .contains("Do not try to work")
    );
}

#[tokio::test]
async fn instant_dispatch_with_wait_never_misses_completion_fifty_times() {
    let f = Daemon::new(vec![]).await;
    for _ in 0..50 {
        let value = f
            .dispatch_cli(&["--wait", "--timeout", "5", "--json"], 0)
            .await;
        assert_eq!(value["result"], "OK");
        assert_eq!(value["outcome"], "finished");
        for key in ["card_id", "task_id", "seq", "events"] {
            assert!(!value[key].is_null());
        }
    }
}

#[tokio::test]
async fn failure_policy_rejection_permission_answer_resume_timeout_and_cancellation() {
    let f = Daemon::new(vec![
        vec![FakeStep::Crash],
        vec![
            FakeStep::Permission(permission()),
            FakeStep::Reply("permitted".into()),
        ],
        vec![FakeStep::Delay(Duration::from_secs(60))],
        vec![
            FakeStep::Permission(permission()),
            FakeStep::Reply("answered after read".into()),
        ],
    ])
    .await;
    let failed = f.dispatch_cli(&["--wait", "--json"], 3).await;
    assert_eq!(failed["outcome"], "failed");
    assert!(
        failed["result"]
            .as_str()
            .unwrap()
            .contains("scripted crash")
    );
    let permission = f.dispatch_cli(&["--wait", "--json"], 4).await;
    assert_eq!(permission["permission"]["tool"], "Edit");
    assert_eq!(permission["permission"]["options"][0]["id"], "yes");
    let card = permission["card_id"].as_str().unwrap();
    let seq = permission["seq"].to_string();
    let pending = f
        .command(&[
            "wait",
            "--card",
            card,
            "--after-seq",
            &failed["seq"].to_string(),
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(pending.status.code(), Some(4));
    let text = String::from_utf8(pending.stdout).unwrap();
    let request = permission["permission"]["request_id"].as_str().unwrap();
    assert!(text.contains(&format!("navarch hub permission {request} --choose 'yes'")));
    assert!(text.contains("Allow once"));
    f.cli(&["permission", request, "--choose", "yes", "--json"], 0)
        .await;
    let done = f
        .cli(&["wait", "--card", card, "--after-seq", &seq, "--json"], 0)
        .await;
    assert_eq!(done["result"], "permitted");
    let timed = f
        .dispatch_cli(&["--wait", "--timeout", "1", "--json"], 124)
        .await;
    let id: CardId = serde_json::from_value(timed["card_id"].clone()).unwrap();
    assert_eq!(f.store.task(id).unwrap().unwrap().steps[0].state, "running");
    let card = timed["card_id"].as_str().unwrap();
    f.cli(&["cancel", card, "--json"], 0).await;
    let cancelled = f
        .cli(
            &[
                "wait",
                "--card",
                card,
                "--after-seq",
                &timed["seq"].to_string(),
                "--json",
            ],
            3,
        )
        .await;
    assert_eq!(cancelled["outcome"], "cancelled");
    // Reading the inbox cannot remove the outstanding permission's public details.
    let request = f.dispatch_cli(&["--json"], 0).await;
    let id: CardId = serde_json::from_value(request["card_id"].clone()).unwrap();
    f.card(id, "waiting").await;
    let inbox = f.cli(&["inbox", "--json"], 0).await;
    let ask = inbox
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["card_id"] == request["card_id"])
        .unwrap();
    let ask_id = ask["envelope"]["body"]["request_id"].as_str().unwrap();
    let task = f
        .cli(&["task", request["card_id"].as_str().unwrap(), "--json"], 0)
        .await;
    assert_eq!(
        task["permission_requests"][0]["detail"],
        "edit a temporary test file"
    );
    f.cli(&["permission", ask_id, "--cancel", "--json"], 0)
        .await;
    // Worker policy rejects cwd outside its roots; this is a step failure, not a hub error.
    let mut args = f.dispatch("work");
    args[6] = "/".into();
    args.extend(["--wait".into(), "--json".into()]);
    let rejected = f
        .cli(&args.iter().map(String::as_str).collect::<Vec<_>>(), 3)
        .await;
    assert_eq!(rejected["outcome"], "rejected");
}

#[tokio::test]
async fn wait_reconnects_after_daemon_restart_and_resumes_cursor() {
    let mut f = Daemon::new(vec![vec![
        FakeStep::Permission(permission()),
        FakeStep::Reply("survived restart".into()),
    ]])
    .await;
    let value = f.dispatch_cli(&["--wait", "--json"], 4).await;
    let id = value["card_id"].as_str().unwrap();
    let request_id = value["permission"]["request_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let waiting = f
        .command(&[
            "wait",
            "--card",
            id,
            "--after-seq",
            &value["seq"].to_string(),
            "--timeout",
            "15",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Give the real process time to connect before interrupting the daemon.
    tokio::time::sleep(Duration::from_millis(200)).await;
    f.stop().await;
    f.worker.answer_permission(
        request_id,
        PermissionDecision::Selected {
            option_id: "yes".into(),
        },
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    f.restart();
    let output = tokio::time::timeout(Duration::from_secs(15), waiting.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "survived restart");
}

#[tokio::test]
async fn prompt_files_stdin_direct_sessions_and_input_rejection() {
    use tokio::io::AsyncWriteExt;
    let f = Daemon::new(vec![]).await;
    let prompt = f.home.path().join("prompt.txt");
    std::fs::write(&prompt, "from file").unwrap();
    let servers = f.home.path().join("servers.json");
    std::fs::write(&servers, "[]").unwrap();
    let mut args = f.dispatch("unused");
    args.pop();
    args.extend([
        "--prompt-file".into(),
        prompt.to_string_lossy().into(),
        "--mcp-servers".into(),
        servers.to_string_lossy().into(),
        "--wait".into(),
        "--json".into(),
    ]);
    let first = f
        .cli(&args.iter().map(String::as_str).collect::<Vec<_>>(), 0)
        .await;
    let card = first["card_id"].as_str().unwrap();
    let task = f.cli(&["task", card, "--json"], 0).await;
    let session = task["steps"][0]["session"].as_str().unwrap();
    let direct = f
        .cli(
            &[
                "send",
                "--machine",
                &f.machine,
                "--session",
                session,
                "--card",
                card,
                "--wait",
                "--json",
                "direct",
            ],
            0,
        )
        .await;
    assert_eq!(direct["card_id"], first["card_id"]);
    for explicit in [false, true] {
        let mut args = f.dispatch("-");
        if !explicit {
            args.pop();
        }
        args.extend(["--wait".into(), "--json".into()]);
        let mut child = f
            .command(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"from stdin")
            .await
            .unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(output.status.code(), Some(0));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["result"], "OK");
    }
    for (file, bytes) in [
        (&prompt, vec![b'x'; 256 * 1024 + 1]),
        (&servers, vec![b' '; 64 * 1024 + 1]),
    ] {
        std::fs::write(file, bytes).unwrap();
        let mut args = f.dispatch("work");
        if file == &prompt {
            args.pop();
            args.extend(["--prompt-file".into(), file.to_string_lossy().into()]);
        } else {
            args.extend(["--mcp-servers".into(), file.to_string_lossy().into()]);
        }
        let output = f
            .command(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("exceeds")
        );
    }
    assert!(f.store.tasks().unwrap().0.len() == 3);
    let running = f.dispatch_cli(&["--json"], 0).await;
    let waited = f
        .cli(
            &["wait", "--after-seq", &running["seq"].to_string(), "--json"],
            0,
        )
        .await;
    assert_eq!(waited["card_id"], running["card_id"]);
}

#[test]
fn removed_server_is_rejected_and_unreachable_commands_name_both_startup_options() {
    let home = tempfile::tempdir().unwrap();
    let removed = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .args(["hub", "mcp"])
        .output()
        .unwrap();
    assert_eq!(removed.status.code(), Some(1));
    assert!(removed.stdout.is_empty());
    for command in [
        "status", "tasks", "sessions", "inbox", "wait", "watch", "machines",
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
            .env("NAVARCH_HOME", home.path())
            .args(["hub", command, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("hub daemon is not running"));
        assert!(error.contains("navarch hub serve"));
        assert!(error.contains("navarch service install hub"));
    }
}
