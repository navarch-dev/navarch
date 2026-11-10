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
    assert!(card.log.iter().any(|e| e.kind == "accepted"));
    assert!(card.log.iter().any(|e| e.kind == "finished"));
    assert_eq!(card.steps[0].result_summary, "first");
    assert!(f.store.index().unwrap().tasks.is_empty());
    let session = card.steps[0].session.as_ref().unwrap();
    let second = f.follow(session);
    let second_card = f.card(second, "finished").await;
    assert_eq!(second_card.steps[1].session.as_ref(), Some(session));
    assert_eq!(second_card.steps[1].result_summary, "followup");
    let cancel = f.follow(session);
    wait(|| f.turn_card(cancel).log.iter().any(|e| e.kind == "progress")).await;
    assert!(
        f.store
            .index()
            .unwrap()
            .tasks
            .iter()
            .any(|e| e.id == card.id)
    );
    f.hub.request(Request::Cancel { task_id: cancel }).unwrap();
    f.card(cancel, "finished").await;
    let permission = f.follow(session);
    let card = f.card(permission, "waiting").await;
    assert_eq!(f.store.index().unwrap().tasks[0].state, "waiting");
    let ask = card
        .permissions
        .iter()
        .map(|p| p.request_id)
        .next()
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
        f.card(permission, "finished").await.steps[3].result_summary,
        "permitted"
    );
    assert!(f.store.index().unwrap().tasks.is_empty());
    assert_eq!(
        f.hub
            .request(Request::GetTask {
                task_id: card.id,
                include_log: false,
            })
            .unwrap()["state"],
        "finished"
    );
    assert_eq!(
        f.hub
            .request_async(Request::ListSessions {
                agent: None,
                limit: None,
                machine: None,
                cwd: None
            })
            .await
            .unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        2
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
        1
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
    let waiting = f.card(task, "waiting").await;
    let ask = waiting
        .permissions
        .iter()
        .map(|p| p.request_id)
        .next()
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
    assert_eq!(f.turn_card(task).state, "waiting");
    f.restart().await;
    let card = f.card(task, "finished").await;
    assert_eq!(card.steps[0].result_summary, "survived");
    assert!(f.store.index().unwrap().tasks.is_empty());
    let event = f
        .store
        .inbox(false)
        .unwrap()
        .into_iter()
        .find(|e| {
            matches!(
                e.envelope.as_ref().map(|e| &e.body),
                Some(Body::Finished { .. })
            )
        })
        .unwrap()
        .envelope
        .unwrap();
    let count = f.store.inbox(false).unwrap().len();
    f.hub.apply(&event).unwrap();
    assert_eq!(f.store.inbox(false).unwrap().len(), count);
    assert_eq!(f.turn_card(task).log, card.log);
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
    let card = f.card(rejected, "failed").await;
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

#[tokio::test]
async fn queued_turns_handoffs_and_card_overrides_preserve_independent_identity() {
    let f = Fixture::new(
        vec![
            vec![
                FakeStep::Tool("First: running".into()),
                FakeStep::Delay(Duration::from_secs(60)),
            ],
            vec![FakeStep::Reply("queued turn completed".into())],
            vec![FakeStep::Reply("handoff".into())],
            vec![FakeStep::Reply("separate card".into())],
            vec![FakeStep::Reply("界".repeat(300))],
            vec![FakeStep::Reply("latest owner".into())],
        ],
        false,
    )
    .await;
    let first = f.dispatch();
    wait(|| f.turn_card(first).log.iter().any(|e| e.kind == "progress")).await;
    let original = f.turn_card(first);
    let session = original.steps[0].session.as_ref().unwrap();
    let cancelled = f.follow(session);
    let queued = f.follow(session);
    assert_ne!(first, cancelled);
    assert_ne!(cancelled, queued);
    assert_eq!(f.turn_card(queued).id, original.id);
    assert_eq!(f.turn_card(first).steps.len(), 3);
    f.hub
        .request(Request::Cancel { task_id: cancelled })
        .unwrap();
    wait(|| f.turn_card(cancelled).steps[1].state == "cancelled").await;
    assert_eq!(f.turn_card(first).state, "running");
    f.hub.request(Request::Cancel { task_id: first }).unwrap();
    let done = f.card(queued, "finished").await;
    assert_eq!(done.steps[2].result_summary, "queued turn completed");
    let handoff = f.hub.request(request(json!({"type":"dispatch","machine":f.machine(),"agent":"fake","cwd":f.temp.path(),"prompt":"handoff","card_id":original.id}))).unwrap();
    assert_eq!(handoff["card_id"], original.id.to_string());
    let handoff = f.task_id(handoff);
    assert_eq!(
        f.card(handoff, "finished").await.steps[3].result_summary,
        "handoff"
    );
    let separate = f.dispatch();
    let separate_card = f.card(separate, "finished").await;
    let overridden = f.hub.request(request(json!({"type":"send_to_session","machine":f.machine(),"session_id":session,"prompt":"move ownership","card_id":separate_card.id}))).unwrap();
    assert_eq!(overridden["card_id"], separate_card.id.to_string());
    let overridden = f.task_id(overridden);
    let done = f.card(overridden, "finished").await;
    assert_eq!(done.steps.len(), 2);
    assert_eq!(done.steps[1].result_summary, "界".repeat(300));
    assert!(
        done.log
            .iter()
            .all(|entry| entry.summary.chars().count() <= 200)
    );
    let last = f.follow(session);
    let done = f.card(last, "finished").await;
    assert_eq!(done.id, separate_card.id);
    assert_eq!(done.steps.len(), 3);
    assert_eq!(done.steps[2].result_summary, "latest owner");
    let default = f
        .hub
        .request(request(json!({"type":"get_task","task_id":done.id})))
        .unwrap();
    assert!(default.get("log").is_none());
    assert!(default.get("permissions").is_none());
    assert!(default.get("applied_events").is_none());
    let full = f
        .hub
        .request(request(
            json!({"type":"get_task","task_id":done.id,"include_log":true}),
        ))
        .unwrap();
    let log = full["log"].as_array().unwrap();
    assert!(!log.is_empty());
    assert!(log.iter().all(|e| e.as_object().unwrap().len() == 4
        && e.get("time").is_some()
        && e.get("kind").is_some()
        && e.get("step").is_some()
        && e.get("summary").is_some()));
    let summaries = f
        .hub
        .request(Request::ListTasks {
            include_finished: true,
        })
        .unwrap();
    assert_eq!(summaries["tasks"].as_array().unwrap().len(), 2);
    assert!(
        summaries["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c.get("log").is_none() && c.get("steps").is_none())
    );
    // Late events for the first turn cannot change its completed sibling or card.
    f.hub
        .apply(&Envelope {
            v: 1,
            id: "00000000-0000-4000-8000-000000000099".parse().unwrap(),
            from: f.machine(),
            to: f.machine(),
            body: Body::Progress {
                task_id: first,
                summary: "late".into(),
            },
        })
        .unwrap();
    assert_eq!(f.turn_card(first).steps[0].state, "cancelled");
    assert_eq!(f.turn_card(first).steps[2].state, "finished");
    assert_eq!(f.turn_card(first).state, "finished");
}
#[tokio::test]
async fn session_discovery_queries_worker_and_offline_fallback_is_stale() {
    let f = Fixture::new(vec![], false).await;
    let task = f.dispatch();
    let card = f.card(task, "finished").await;
    use navarch_acp::{AgentDriver, NewSession};
    f.driver
        .new_session(NewSession {
            cwd: f.temp.path().canonicalize().unwrap(),
            mode: None,
            model: None,
            mcp_servers: vec![],
        })
        .await
        .unwrap();
    let list = f
        .hub
        .request_async(Request::ListSessions {
            agent: None,
            limit: None,
            machine: Some(f.machine()),
            cwd: Some(f.temp.path().to_owned()),
        })
        .await
        .unwrap();
    let sessions = list["sessions"].as_array().unwrap();
    // Both the dispatch and an independently created adapter session are discoverable.
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|s| s["stale"] == false));
    assert!(sessions.iter().any(|s| s["session_id"]
        == card.steps[0].session.as_ref().unwrap().as_str()
        && s["navarch_started"] == true));
    let external = sessions
        .iter()
        .find(|s| s["navarch_started"] == false)
        .unwrap();
    let adopted = f.hub.request(request(json!({"type":"send_to_session","machine":f.machine(),"agent":external["agent"],"session_id":external["adapter_session_id"],"prompt":"adopt"}))).unwrap();
    let adopted = f.card(f.task_id(adopted), "finished").await;
    assert!(adopted.steps[0].session.is_some());
    assert!(
        !f.worker
            .sessions()
            .unwrap()
            .iter()
            .find(|s| Some(&s.session_id) == adopted.steps[0].session.as_ref())
            .unwrap()
            .navarch_started
    );
    let follow = f.hub.request(request(json!({"type":"send_to_session","machine":f.machine(),"agent":external["agent"],"session_id":external["adapter_session_id"],"prompt":"same card"}))).unwrap();
    assert_eq!(follow["card_id"], adopted.id.to_string());
    f.card(f.task_id(follow), "finished").await;
    f.hub_task.abort();
    wait(|| f.hub.request(Request::ListWorkers).unwrap()[0]["online"] == false).await;
    let fallback = f
        .hub
        .request_async(Request::ListSessions {
            agent: None,
            limit: None,
            machine: Some(f.machine()),
            cwd: None,
        })
        .await
        .unwrap();
    assert_eq!(fallback["sessions"].as_array().unwrap().len(), 2);
    assert!(
        fallback["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["stale"] == true)
    );
}

#[tokio::test]
async fn five_thousand_long_title_sessions_are_bounded_and_link_stays_up() {
    let f = Fixture::new(vec![], false).await;
    f.driver
        .set_listed_sessions(
            (0..5000)
                .map(|i| navarch_acp::SessionInfo {
                    session_id: format!("external-{i:04}"),
                    cwd: f.temp.path().canonicalize().unwrap(),
                    title: Some("界\"\0".repeat(2000)),
                    updated_at: Some((1_700_000_000_000u64 + i).to_string()),
                })
                .collect(),
        )
        .await;
    for (agent, cwd, limit, expected) in [
        (None, None, None, 50),
        (Some("fake"), Some(f.temp.path().to_owned()), Some(9), 9),
        (None, None, Some(5000), 200),
        (Some("absent"), None, None, 0),
        (None, Some("/".into()), None, 0),
        (None, None, Some(0), 0),
    ] {
        let list = f
            .hub
            .request_async(Request::ListSessions {
                machine: Some(f.machine()),
                agent: agent.map(str::to_owned),
                cwd,
                limit,
            })
            .await
            .unwrap();
        assert_eq!(list["stale"], false);
        assert_eq!(
            list["truncated"],
            (agent != Some("absent") && expected != 0) || limit == Some(0)
        );
        let sessions = list["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), expected);
        assert!(
            sessions
                .iter()
                .all(|s| s["title"].as_str().unwrap().chars().count() == 120)
        );
        let timestamps: Vec<_> = sessions
            .iter()
            .map(|s| session_timestamp(s["updated_at"].as_str()))
            .collect();
        assert!(timestamps.windows(2).all(|w| w[0] >= w[1]));
        if expected > 0 {
            assert_eq!(sessions[0]["adapter_session_id"], "external-4999");
        }
        assert!(serde_json::to_vec(&list).unwrap().len() < LIST_ENVELOPE_BYTES);
        assert_eq!(
            f.hub.request(Request::ListWorkers).unwrap()[0]["online"],
            true
        );
    }
    // Exercise the exact worker-envelope budget with long opaque IDs and paths too.
    f.driver
        .set_listed_sessions(
            (0..5000)
                .map(|i| navarch_acp::SessionInfo {
                    session_id: format!("{i:04}{}", "\0".repeat(2048)),
                    cwd: f.temp.path().canonicalize().unwrap(),
                    title: Some("界".repeat(1000)),
                    updated_at: Some(i.to_string()),
                })
                .collect(),
        )
        .await;
    f.worker
        .handle(Body::ListSessions {
            request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            agent: None,
            cwd: None,
            limit: Some(200),
        })
        .await
        .unwrap();
    let (sessions, truncated) = f
        .worker
        .query_sessions(None, None, Some(200))
        .await
        .unwrap();
    assert!(truncated);
    assert!(!sessions.is_empty() && sessions.len() < 200);
    assert!(
        encoded_envelope_len(&Body::SessionList {
            request_id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
            sessions,
            truncated
        })
        .unwrap()
            <= LIST_ENVELOPE_BYTES
    );
    let result = f
        .hub
        .request_async(Request::ListSessions {
            machine: Some(f.machine()),
            agent: None,
            cwd: None,
            limit: Some(200),
        })
        .await
        .unwrap();
    assert_eq!(result["stale"], false);
    assert_eq!(result["truncated"], true);
    let task = f.dispatch();
    assert_eq!(f.card(task, "finished").await.steps[0].result_summary, "OK");
}

#[tokio::test]
async fn oversize_prompts_including_role_preferences_leave_no_queued_cards() {
    let f = Fixture::new(vec![], false).await;
    for prompt in [
        "x".repeat(MAX_ENVELOPE_BYTES),
        "\0".repeat(MAX_ENVELOPE_BYTES / 6),
    ] {
        for kind in ["dispatch", "send_to_session"] {
            let value = if kind == "dispatch" {
                json!({"type":kind,"machine":f.machine(),"agent":"fake","cwd":f.temp.path(),"prompt":prompt})
            } else {
                json!({"type":kind,"machine":f.machine(),"agent":"fake","session_id":"external","prompt":prompt})
            };
            let error = f.hub.request(request(value)).unwrap_err().to_string();
            assert!(error.contains("exceeds the 1 MiB frame limit"), "{error}");
            assert!(f.store.tasks().unwrap().0.is_empty());
        }
    }
    f.store
        .put_role(
            "large",
            navarch_store::Role {
                agent: "fake".into(),
                mode: None,
                model: None,
                preferences: "\0".repeat(MAX_ENVELOPE_BYTES / 6),
            },
        )
        .unwrap();
    let error = f.hub.request(request(json!({"type":"dispatch","machine":f.machine(),"role":"large","cwd":f.temp.path(),"prompt":"hi"}))).unwrap_err().to_string();
    assert!(error.contains("shorten the prompt"), "{error}");
    assert!(f.store.tasks().unwrap().0.is_empty());
    assert!(f.worker.sessions().unwrap().is_empty());
    f.card(f.dispatch(), "finished").await;
}
