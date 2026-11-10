//! Card cancellation prefers running work over its queued continuation.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
#[path = "support/daemon.rs"]
mod daemon;
use daemon::Daemon;
use navarch_acp::FakeStep;
use navarch_store::Store;
use std::time::Duration;

#[tokio::test]
async fn cancel_card_stops_running_step_before_queued_followup() {
    let f = Daemon::new(vec![
        vec![FakeStep::Delay(Duration::from_secs(60))],
        vec![FakeStep::Reply("queued followup completed".into())],
    ])
    .await;
    let first = f.dispatch_cli(&["--json"], 0).await;
    let id = serde_json::from_value(first["card_id"].clone()).unwrap();
    f.card(id, "running").await;
    let card = first["card_id"].as_str().unwrap();
    let followup = f
        .cli(&["send", card, "queued continuation", "--json"], 0)
        .await;
    let cancel = f.cli(&["cancel", card, "--json"], 0).await;
    assert_eq!(cancel["task_id"], first["task_id"]);
    let result = f
        .cli(
            &[
                "wait",
                "--card",
                card,
                "--after-seq",
                &followup["seq"].to_string(),
                "--json",
            ],
            3,
        )
        .await;
    assert_eq!(result["outcome"], "cancelled");
    f.card(id, "finished").await;
    let stored = f.store.task(id).unwrap().unwrap();
    assert_eq!(stored.steps[0].state, "cancelled");
    assert_eq!(stored.steps[1].result_summary, "queued followup completed");
}
