//! Regression coverage for M5 review findings through the built CLI and real daemon.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
#[path = "support/daemon.rs"]
mod daemon;
use daemon::{Daemon, permission};
use navarch_acp::FakeStep;
use navarch_proto::{CardId, TaskId};
use navarch_store::{Store, TaskCard};
use std::{path::Path, time::Duration};

#[tokio::test]
async fn waits_replay_finished_and_permission_events_after_another_client_reads_inbox() {
    let f = Daemon::new(vec![
        vec![FakeStep::Reply("completed before inbox read".into())],
        vec![FakeStep::Permission(permission())],
    ])
    .await;
    for state in ["finished", "waiting"] {
        let dispatch = f.dispatch_cli(&["--json"], 0).await;
        let card = dispatch["card_id"].as_str().unwrap();
        f.card(card.parse().unwrap(), state).await;
        let read = f.cli(&["inbox", "--json"], 0).await;
        assert_eq!(read.as_array().unwrap().len(), 1);
        assert_eq!(
            f.cli(&["inbox", "--peek", "--json"], 0).await,
            serde_json::json!([])
        );
        let cursor = dispatch["seq"].to_string();
        let code = if state == "finished" { 0 } else { 4 };
        for scoped in [true, false] {
            let mut args = vec!["wait", "--after-seq", &cursor, "--timeout", "2", "--json"];
            if scoped {
                args.extend(["--card", card]);
            }
            let outcome = f.cli(&args, code).await;
            assert_eq!(outcome["card_id"], dispatch["card_id"]);
            assert_eq!(outcome["task_id"], dispatch["task_id"]);
            assert_eq!(outcome["seq"], read[0]["seq"]);
            if state == "finished" {
                assert_eq!(outcome["result"], "completed before inbox read");
            } else {
                assert_eq!(
                    outcome["permission"]["detail"],
                    "edit a temporary test file"
                );
            }
        }
        assert!(f.store.inbox(false).unwrap().is_empty());
    }
}

#[tokio::test]
async fn cancellation_is_stored_and_shown_consistently_by_wait_task_and_tasks() {
    let f = Daemon::new(vec![vec![FakeStep::Delay(Duration::from_secs(60))]]).await;
    let dispatch = f.dispatch_cli(&["--json"], 0).await;
    let card = dispatch["card_id"].as_str().unwrap();
    let id: CardId = card.parse().unwrap();
    f.card(id, "running").await;
    f.cli(&["cancel", card, "--json"], 0).await;
    let outcome = f
        .cli(
            &[
                "wait",
                "--card",
                card,
                "--after-seq",
                &dispatch["seq"].to_string(),
                "--timeout",
                "5",
                "--json",
            ],
            3,
        )
        .await;
    assert_eq!(outcome["outcome"], "cancelled");
    // Inspect the on-disk card, independently of state derivation on reads/rendering.
    let stored: TaskCard = toml::from_str(
        &std::fs::read_to_string(f.home.path().join(format!("hub/tasks/{card}.toml"))).unwrap(),
    )
    .unwrap();
    assert_eq!(stored.state, "cancelled");
    assert_eq!(stored.steps[0].state, "cancelled");
    assert!(!stored.is_active());
    let task = f.cli(&["task", card, "--json"], 0).await;
    assert_eq!(task["state"], "cancelled");
    assert_eq!(task["steps"][0]["state"], "cancelled");
    let tasks = f.cli(&["tasks", "--all", "--json"], 0).await;
    assert_eq!(tasks["tasks"][0]["state"], "cancelled");
    for args in [vec!["task", card], vec!["tasks", "--all"]] {
        let output = f.command(&args).output().await.unwrap();
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("cancelled"));
        assert!(!text.contains("finished"));
    }
}

fn assert_readable_result(output: std::process::Output, result: &str) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    let (metadata, body) = text.split_once("\n\n").unwrap();
    let lines: Vec<_> = metadata.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    let card = lines[0].strip_prefix("card_id: ").unwrap();
    card.parse::<CardId>().unwrap();
    lines[1]
        .strip_prefix("task_id: ")
        .unwrap()
        .parse::<TaskId>()
        .unwrap();
    assert_eq!(lines[2], "outcome: finished");
    lines[3]
        .strip_prefix("seq: ")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(body, format!("{result}\n"));
    card.to_owned()
}

#[tokio::test]
async fn readable_dispatch_send_and_card_wait_print_full_result_exactly_once() {
    let result = format!("unique opening\n{}\nunique ending", "界".repeat(300));
    let f = Daemon::new(vec![vec![FakeStep::Reply(result.clone())]; 3]).await;
    let mut args = f.dispatch("work");
    args.extend(["--wait".into(), "--timeout".into(), "5".into()]);
    let output = f
        .command(&args.iter().map(String::as_str).collect::<Vec<_>>())
        .output()
        .await
        .unwrap();
    let card = assert_readable_result(output, &result);
    let output = f
        .command(&["send", &card, "continue", "--wait", "--timeout", "5"])
        .output()
        .await
        .unwrap();
    assert_eq!(assert_readable_result(output, &result), card);
    let dispatch = f.dispatch_cli(&["--json"], 0).await;
    let card = dispatch["card_id"].as_str().unwrap();
    let output = f
        .command(&[
            "wait",
            "--card",
            card,
            "--after-seq",
            &dispatch["seq"].to_string(),
            "--timeout",
            "5",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(assert_readable_result(output, &result), card);
}

#[test]
fn guide_pointer_is_a_user_project_example_without_repository_root_instructions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(!root.join("AGENTS.md").exists());
    let docs = std::fs::read_to_string(root.join("docs/hub.md")).unwrap();
    assert!(docs.contains("Add this line to your project's"));
    assert!(
        docs.contains("To run work on other machines, run \"navarch hub guide\" and follow it.")
    );
}
