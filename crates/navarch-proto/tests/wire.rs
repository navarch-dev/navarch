//! Wire format contracts.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_proto::*;
use uuid::Uuid;
fn id() -> Uuid {
    "00000000-0000-4000-8000-000000000001".parse().unwrap()
}
fn task() -> TaskId {
    id().to_string().parse().unwrap()
}
fn check(body: Body, golden: &str) {
    let envelope = Envelope {
        v: PROTOCOL_VERSION,
        id: id(),
        from: "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
        to: "77777777777777777777777774".parse().unwrap(),
        body,
    };
    assert!(envelope.check_version().is_ok());
    let expected: serde_json::Value = serde_json::from_str(golden).unwrap();
    assert_eq!(serde_json::to_value(&envelope).unwrap(), expected);
    assert_eq!(serde_json::from_str::<Envelope>(golden).unwrap(), envelope);
}
#[test]
fn golden_dispatch() {
    check(
        Body::Dispatch {
            task_id: task(),
            agent: "fake".into(),
            cwd: "/tmp".into(),
            prompt: "hi".into(),
            mode: None,
            model: None,
            mcp_servers: None,
        },
        include_str!("golden/dispatch.json"),
    );
}
#[test]
fn golden_send_to_session() {
    check(
        Body::SendToSession {
            agent: None,
            task_id: task(),
            session_id: "s".into(),
            prompt: "next".into(),
        },
        include_str!("golden/send_to_session.json"),
    );
}
#[test]
fn golden_cancel() {
    check(
        Body::Cancel { task_id: task() },
        include_str!("golden/cancel.json"),
    );
}
#[test]
fn golden_permission_answer() {
    check(
        Body::PermissionAnswer {
            request_id: id(),
            decision: PermissionDecision::Selected {
                option_id: "yes".into(),
            },
        },
        include_str!("golden/permission_answer.json"),
    );
}
#[test]
fn golden_probe() {
    check(Body::Probe, include_str!("golden/probe.json"));
}
#[test]
fn golden_ack() {
    check(Body::Ack { of: id() }, include_str!("golden/ack.json"));
}
#[test]
fn golden_accepted() {
    check(
        Body::Accepted {
            task_id: task(),
            session_id: "s".into(),
        },
        include_str!("golden/accepted.json"),
    );
}
#[test]
fn golden_rejected() {
    check(
        Body::Rejected {
            task_id: task(),
            reason: "no".into(),
        },
        include_str!("golden/rejected.json"),
    );
}
#[test]
fn golden_progress() {
    check(
        Body::Progress {
            task_id: task(),
            summary: "working".into(),
        },
        include_str!("golden/progress.json"),
    );
}
#[test]
fn golden_permission_ask() {
    check(
        Body::PermissionAsk {
            request_id: id(),
            task_id: task(),
            tool: "edit".into(),
            detail: "file".into(),
            options: vec![PermissionOption {
                id: "yes".into(),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
            }],
        },
        include_str!("golden/permission_ask.json"),
    );
}
#[test]
fn golden_finished() {
    check(
        Body::Finished {
            task_id: task(),
            session_id: "s".into(),
            stop_reason: "end_turn".into(),
            result: "done".into(),
        },
        include_str!("golden/finished.json"),
    );
}
#[test]
fn golden_failed() {
    check(
        Body::Failed {
            task_id: task(),
            error: "crash".into(),
        },
        include_str!("golden/failed.json"),
    );
}
#[test]
fn golden_probe_report() {
    check(
        Body::ProbeReport {
            capabilities: Capabilities {
                os: "test".into(),
                arch: "test".into(),
                ..Capabilities::default()
            },
        },
        include_str!("golden/probe_report.json"),
    );
}
#[test]
fn version_and_unknown_fields() {
    let mut envelope: Envelope = serde_json::from_str(include_str!("golden/probe.json")).unwrap();
    envelope.v = 2;
    assert_eq!(envelope.check_version().unwrap_err().0, 2);
    let mut value = serde_json::to_value(&envelope).unwrap();
    value["future"] = true.into();
    value["body"]["future"] = true.into();
    assert!(serde_json::from_value::<Envelope>(value.clone()).is_ok());
    value["body"]["type"] = "unknown".into();
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}
#[test]
fn dispatch_optional_settings_and_mcp_round_trip() {
    let body = Body::Dispatch {
        task_id: task(),
        agent: "fake".into(),
        cwd: "/tmp".into(),
        prompt: "hi".into(),
        mode: Some("custom".into()),
        model: Some("alternate".into()),
        mcp_servers: Some(vec![McpServer {
            name: "test".into(),
            command: "/bin/test".into(),
            args: vec!["one".into()],
            env: [("K".into(), "V".into())].into(),
        }]),
    };
    assert_eq!(
        serde_json::from_value::<Body>(serde_json::to_value(&body).unwrap()).unwrap(),
        body
    );
}

#[test]
fn golden_list_sessions() {
    check(
        Body::ListSessions {
            request_id: id(),
            cwd: Some("/tmp".into()),
        },
        include_str!("golden/list_sessions.json"),
    );
}
#[test]
fn golden_session_list() {
    check(
        Body::SessionList {
            request_id: id(),
            sessions: vec![WorkerSession {
                session_id: None,
                agent: "fake".into(),
                adapter_session_id: "external".into(),
                cwd: "/tmp".into(),
                title: Some("Work".into()),
                updated_at: Some("2026-11-10T00:00:00Z".into()),
                navarch_started: false,
            }],
        },
        include_str!("golden/session_list.json"),
    );
}
#[test]
fn card_ids_and_summary_bounds() {
    let id = CardId::new();
    assert_eq!(id.to_string().parse::<CardId>().unwrap(), id);
    assert_eq!(
        serde_json::from_value::<CardId>(serde_json::to_value(id).unwrap()).unwrap(),
        id
    );
    assert!("invalid".parse::<CardId>().is_err());
    assert_eq!(short_summary("  hello\n world "), "hello world");
    assert_eq!(short_summary(&"界".repeat(201)).chars().count(), 200);
}
