//! In-process hub/worker workflows with FakeDriver.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
mod support;
use navarch_acp::FakeStep;
use navarch_hub::control::Request;
use navarch_proto::*;
use serde_json::json;
use std::time::Duration;
use support::*;
#[tokio::test]
async fn dispatch_followup_cancel_permission_inbox_cards_and_index() {
    let f = Fixture::new(
        vec![
            vec![FakeStep::Reply("first".into())],
            vec![FakeStep::Reply("followup".into())],
            vec![
                FakeStep::Tool("running".into()),
                FakeStep::Delay(Duration::from_secs(60)),
            ],
            vec![
                FakeStep::Permission(permission()),
                FakeStep::Reply("permitted".into()),
            ],
        ],
        false,
    )
    .await;
    let first = f.dispatch();
    let card = f.card(first, "finished").await;
    assert!(
        card.log
            .iter()
            .any(|e| matches!(e.body, Body::Accepted { .. }))
    );
    assert!(
        card.log
            .iter()
            .any(|e| matches!(e.body, Body::Finished { .. }))
    );
    assert_eq!(card.steps[0].result_summary, "first");
    assert!(f.store.index().unwrap().tasks.is_empty());
    let session = card.steps[0].session.as_ref().unwrap();
    let second = f.follow(session);
    let second_card = f.card(second, "finished").await;
    assert_eq!(second_card.steps[0].session.as_ref(), Some(session));
    assert_eq!(second_card.steps[0].result_summary, "followup");
    let cancel = f.follow(session);
    wait(|| {
        f.store
            .task(cancel)
            .unwrap()
            .unwrap()
            .log
            .iter()
            .any(|e| matches!(e.body, Body::Progress { .. }))
    })
    .await;
    assert!(
        f.store
            .index()
            .unwrap()
            .tasks
            .iter()
            .any(|e| e.id == cancel)
    );
    f.hub.request(Request::Cancel { task_id: cancel }).unwrap();
    f.card(cancel, "cancelled").await;
    let permission = f.follow(session);
    let card = f.card(permission, "permission").await;
    assert_eq!(f.store.index().unwrap().tasks[0].state, "permission");
    let ask = card
        .log
        .iter()
        .find_map(|e| {
            if let Body::PermissionAsk { request_id, .. } = e.body {
                Some(request_id)
            } else {
                None
            }
        })
        .unwrap();
    let inbox = f.hub.request(Request::ReadInbox).unwrap();
    assert!(
        inbox
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["envelope"]["body"]["request_id"] == ask.to_string())
    );
    assert_eq!(f.hub.request(Request::ReadInbox).unwrap(), json!([]));
    assert!(
        f.hub
            .request(Request::AnswerPermission {
                request_id: ask,
                decision: PermissionDecision::Selected {
                    option_id: "not-offered".into()
                }
            })
            .is_err()
    );
    f.hub
        .request(Request::AnswerPermission {
            request_id: ask,
            decision: PermissionDecision::Selected {
                option_id: "yes".into(),
            },
        })
        .unwrap();
    assert_eq!(
        f.card(permission, "finished").await.steps[0].result_summary,
        "permitted"
    );
    assert!(f.store.index().unwrap().tasks.is_empty());
    assert_eq!(
        f.hub
            .request(Request::GetTask {
                task_id: permission
            })
            .unwrap()["state"],
        "finished"
    );
    assert_eq!(
        f.hub
            .request(Request::ListSessions { machine: None })
            .unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.hub
            .request(Request::ListTasks {
                include_finished: true
            })
            .unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        f.hub
            .request(Request::ReadInbox)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn hub_restart_mid_turn_retains_finished_and_deduplicates_durable_events() {
    let mut f = Fixture::new(
        vec![vec![
            FakeStep::Permission(permission()),
            FakeStep::Reply("survived".into()),
        ]],
        true,
    )
    .await;
    let task = f.dispatch();
    let waiting = f.card(task, "permission").await;
    let ask = waiting
        .log
        .iter()
        .find_map(|e| {
            if let Body::PermissionAsk { request_id, .. } = e.body {
                Some(request_id)
            } else {
                None
            }
        })
        .unwrap();
    f.hub_task.abort();
    let _ = (&mut f.hub_task).await;
    // Release the fake turn while the daemon is absent. Only the test harness
    // supplies this gate; production permissions use the control request.
    f.worker.answer_permission(
        ask,
        PermissionDecision::Selected {
            option_id: "yes".into(),
        },
    );
    wait(|| f.worker.running_turns() == 0).await;
    assert_eq!(f.store.task(task).unwrap().unwrap().state, "permission");
    f.restart().await;
    let card = f.card(task, "finished").await;
    assert_eq!(card.steps[0].result_summary, "survived");
    assert!(f.store.index().unwrap().tasks.is_empty());
    let event = card
        .log
        .iter()
        .find(|e| matches!(e.body, Body::Finished { .. }))
        .unwrap();
    let count = f.store.inbox(false).unwrap().len();
    f.hub.apply(event).unwrap();
    assert_eq!(f.store.inbox(false).unwrap().len(), count);
    assert_eq!(f.store.task(task).unwrap().unwrap().log, card.log);
}
#[tokio::test]
async fn dispatch_validation_role_settings_and_local_approval() {
    let f = Fixture::new(vec![], false).await;
    assert_eq!(
        f.store.machines().unwrap()[0].state,
        navarch_store::MachineState::Active
    );
    for changes in [
        json!({"cwd":""}),
        json!({"agent":"unknown"}),
        json!({"role":"missing","agent":null}),
        json!({"role":"developer"}),
        json!({"agent":null}),
    ] {
        let mut value = json!({"type":"dispatch","machine":f.machine(),"agent":"fake","cwd":f.temp.path(),"prompt":"hi"});
        for (key, value_change) in changes.as_object().unwrap() {
            value[key] = value_change.clone();
        }
        assert!(f.hub.request(request(value)).is_err());
    }
    f.store
        .put_role(
            "developer",
            navarch_store::Role {
                agent: "fake".into(),
                mode: Some("custom".into()),
                model: Some("fake-model".into()),
                preferences: "test preference".into(),
            },
        )
        .unwrap();
    let task = f.task_id(f.hub.request(request(json!({"type":"dispatch","machine":f.machine(),"role":"developer","cwd":f.temp.path(),"prompt":"hi"}))).unwrap());
    let card = f.card(task, "finished").await;
    assert_eq!(card.steps[0].role.as_deref(), Some("developer"));
    let sessions = f.worker.sessions().unwrap();
    assert_eq!(sessions[0].config.mode.as_deref(), Some("custom"));
    assert_eq!(sessions[0].config.model.as_deref(), Some("fake-model"));
    f.hub_task.abort();
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while f.hub.request(Request::ListWorkers).unwrap()[0]["online"] == true {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(f.hub.request(request(json!({"type":"dispatch","machine":f.machine(),"agent":"fake","cwd":f.temp.path(),"prompt":"hi"}))).is_err());
}

#[tokio::test]
async fn rejection_and_driver_failure_reach_cards_index_and_inbox() {
    let f = Fixture::new(vec![vec![FakeStep::Crash]], false).await;
    let rejected = f.task_id(f.hub.request(request(json!({"type":"dispatch","machine":f.machine(),"agent":"fake","cwd":"/","prompt":"outside roots"}))).unwrap());
    let card = f.card(rejected, "rejected").await;
    assert!(card.steps[0].result_summary.contains("allowed_roots"));
    assert!(f.worker.sessions().unwrap().is_empty());
    let failed = f.dispatch();
    assert!(
        f.card(failed, "failed").await.steps[0]
            .result_summary
            .contains("scripted crash")
    );
    assert!(f.store.index().unwrap().tasks.is_empty());
    let inbox = f.hub.request(Request::ReadInbox).unwrap();
    let types: Vec<_> = inbox
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["envelope"]["body"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["rejected", "failed"]);
}
