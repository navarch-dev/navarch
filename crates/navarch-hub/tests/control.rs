//! Control API wire contracts contain only Navarch values.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_hub::{Hub, control::*};
use navarch_store::{FileStore, Store};
use serde_json::json;
#[test]
fn every_request_round_trips_and_ignores_unknown_fields() {
    for text in [
        json!({"type":"list_workers"}),
        json!({"type":"list_sessions","machine":"aaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        json!({"type":"dispatch","machine":"aaaaaaaaaaaaaaaaaaaaaaaaaa","agent":"fake","cwd":"/work","prompt":"hi"}),
        json!({"type":"send_to_session","machine":"aaaaaaaaaaaaaaaaaaaaaaaaaa","session_id":"s","prompt":"again"}),
        json!({"type":"cancel","task_id":"00000000-0000-4000-8000-000000000001"}),
        json!({"type":"get_task","task_id":"00000000-0000-4000-8000-000000000001"}),
        json!({"type":"list_tasks"}),
        json!({"type":"read_inbox"}),
        json!({"type":"answer_permission","request_id":"00000000-0000-4000-8000-000000000001","decision":{"type":"selected","option_id":"yes"}}),
        json!({"type":"approve_machine","machine":"aaaaaaaaaaaaaaaaaaaaaaaaaa"}),
    ] {
        let req: Request = serde_json::from_value(text.clone()).unwrap();
        assert_eq!(
            serde_json::from_value::<Request>(serde_json::to_value(&req).unwrap()).unwrap(),
            req
        );
        let mut extra = text;
        extra["future"] = true.into();
        assert_eq!(serde_json::from_value::<Request>(extra).unwrap(), req);
    }
    assert!(serde_json::from_value::<Request>(json!({"type":"bogus"})).is_err());
    assert!(serde_json::from_value::<Request>(json!({"type":"dispatch"})).is_err());
    for response in [
        Response::Ok {
            value: json!({"tasks":[]}),
        },
        Response::Error {
            message: "hub offline".into(),
        },
    ] {
        let text = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::to_string(&serde_json::from_str::<Response>(&text).unwrap()).unwrap(),
            text
        );
    }
}
#[test]
fn init_preserves_files_and_examples_are_parseable() {
    let temp = tempfile::tempdir().unwrap();
    navarch_hub::init(temp.path()).unwrap();
    let store = FileStore::open(temp.path().join("hub")).unwrap();
    assert!(store.projects().unwrap().projects.is_empty());
    assert!(store.routes().unwrap().routes.is_empty());
    assert_eq!(store.role("developer").unwrap().unwrap().agent, "codex");
    for path in ["projects.toml", "routes.toml", "roles/developer.md"] {
        std::fs::write(temp.path().join("hub").join(path), "# mine").unwrap();
    }
    navarch_hub::init(temp.path()).unwrap();
    for path in ["projects.toml", "routes.toml", "roles/developer.md"] {
        assert_eq!(
            std::fs::read_to_string(temp.path().join("hub").join(path)).unwrap(),
            "# mine"
        );
    }
}
#[tokio::test]
async fn absent_daemon_is_actionable_and_creates_no_socket() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hub.sock");
    let error = call(&path, &Request::ListWorkers)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("start `navarch hub serve`"));
    assert!(!path.exists());
}
#[test]
fn unknown_machine_and_pending_or_revoked_are_refused() {
    use navarch_proto::*;
    use navarch_store::*;
    use std::sync::Arc;
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store.clone());
    let machine: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    for state in [
        None,
        Some(MachineState::Pending),
        Some(MachineState::Revoked),
        Some(MachineState::Active),
    ] {
        if let Some(state) = state {
            store
                .put_machine(Machine {
                    id: machine.clone(),
                    public_key: [0; 32],
                    display_name: String::new(),
                    state,
                    capabilities: None,
                })
                .unwrap();
        }
        let req: Request = serde_json::from_value(
            json!({"type":"dispatch","machine":machine,"agent":"fake","cwd":"/work","prompt":"hi"}),
        )
        .unwrap();
        assert!(hub.request(req).is_err());
        assert!(store.tasks().unwrap().0.is_empty());
    }
}

#[test]
fn cross_machine_events_update_their_own_steps_and_card_state_is_derived() {
    use navarch_proto::*;
    use navarch_store::*;
    use std::sync::Arc;
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store.clone());
    let a: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    let b: MachineId = "77777777777777777777777774".parse().unwrap();
    for machine in [&a, &b] {
        store
            .put_machine(Machine {
                id: machine.clone(),
                public_key: [0; 32],
                display_name: "test".into(),
                state: MachineState::Active,
                capabilities: None,
            })
            .unwrap();
    }
    let first = TaskId::new();
    let latest = TaskId::new();
    let id = CardId::new();
    store
        .put_task(TaskCard {
            id,
            title: "handoff".into(),
            project: None,
            state: "ignored".into(),
            steps: [(first, a.clone()), (latest, b.clone())]
                .into_iter()
                .map(|(task_id, machine)| Step {
                    task_id,
                    machine,
                    created_at: 1,
                    role: Some("developer".into()),
                    agent: "fake".into(),
                    session: None,
                    adapter_session_id: None,
                    cwd: None,
                    state: "running".into(),
                    result_summary: String::new(),
                })
                .collect(),
            log: vec![],
            applied_events: vec![],
            permissions: vec![],
        })
        .unwrap();
    let event = |from: MachineId, body: Body| Envelope {
        v: 1,
        id: uuid::Uuid::new_v4(),
        from,
        to: a.clone(),
        body,
    };
    let failed = Body::Failed {
        task_id: first,
        error: "old turn failed".into(),
    };
    assert!(hub.apply(&event(b.clone(), failed.clone())).is_err());
    hub.apply(&event(a.clone(), failed)).unwrap();
    assert_eq!(store.task(id).unwrap().unwrap().state, "running");
    hub.apply(&event(
        b.clone(),
        Body::PermissionAsk {
            request_id: uuid::Uuid::new_v4(),
            task_id: latest,
            tool: "Edit".into(),
            detail: "file".into(),
            options: vec![],
        },
    ))
    .unwrap();
    assert_eq!(store.task(id).unwrap().unwrap().state, "waiting");
    hub.apply(&event(
        b.clone(),
        Body::Accepted {
            task_id: latest,
            session_id: "second".into(),
        },
    ))
    .unwrap();
    assert_eq!(store.task(id).unwrap().unwrap().state, "waiting");
    let done = event(
        b,
        Body::Finished {
            task_id: latest,
            session_id: "second".into(),
            stop_reason: "end_turn".into(),
            result: "done".into(),
        },
    );
    hub.apply(&done).unwrap();
    hub.apply(&done).unwrap();
    let card = store.task(id).unwrap().unwrap();
    assert_eq!(card.state, "finished");
    assert_eq!(card.steps[0].state, "failed");
    assert_eq!(card.steps[1].state, "finished");
    assert_eq!(card.log.len(), 4);
    assert_eq!(store.inbox(false).unwrap().len(), 3);
}
