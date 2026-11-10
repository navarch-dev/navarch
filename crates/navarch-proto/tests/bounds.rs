//! Adversarial payload bounds: UTF-8, JSON escaping, counts and opaque metadata.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_proto::*;
use uuid::Uuid;

fn size(body: &Body) -> usize {
    let machine = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    let actual = serde_json::to_vec(&Envelope {
        v: 1,
        id: Uuid::nil(),
        from: machine,
        to: "77777777777777777777777774".parse().unwrap(),
        body: body.clone(),
    })
    .unwrap()
    .len();
    assert_eq!(actual, encoded_envelope_len(body).unwrap());
    actual
}
fn session(text: &str) -> WorkerSession {
    WorkerSession {
        session_id: None,
        agent: "fake".into(),
        adapter_session_id: text.into(),
        cwd: text.into(),
        title: Some(text.into()),
        updated_at: None,
        navarch_started: false,
    }
}
#[test]
fn property_worker_body_encoded_bounds_and_character_boundaries() {
    let task_id = TaskId::new();
    for unit in ["x", "界", "🙂", "\0", "\n\t\"\\"] {
        for count in [0, 1, 119, 120, 121, 1024, 65535, 65536, 65537, 180000] {
            let text = unit.repeat(count);
            let mut bodies = vec![
                Body::Finished {
                    task_id,
                    session_id: "s".into(),
                    stop_reason: text.clone(),
                    result: text.clone(),
                },
                Body::Progress {
                    task_id,
                    summary: text.clone(),
                },
                Body::PermissionAsk {
                    task_id,
                    request_id: Uuid::nil(),
                    tool: text.clone(),
                    detail: text.clone(),
                    options: (0..40)
                        .map(|i| PermissionOption {
                            id: format!("choice-{i}"),
                            name: text.clone(),
                            kind: PermissionOptionKind::AllowOnce,
                        })
                        .collect(),
                },
                Body::Rejected {
                    task_id,
                    reason: text.clone(),
                },
                Body::Failed {
                    task_id,
                    error: text.clone(),
                },
                Body::SessionList {
                    request_id: Uuid::nil(),
                    sessions: vec![session(&text); 3],
                    truncated: false,
                },
                Body::ProbeReport {
                    capabilities: Capabilities {
                        os: text.clone(),
                        arch: text.clone(),
                        agents: vec![AgentCapability {
                            name: text.clone(),
                            version: Some(text.clone()),
                            error: Some(text.clone()),
                            models: vec![text.clone(); 4],
                            present: true,
                        }],
                        repositories: vec![text.clone().into(); 4],
                        running_turns: 0,
                    },
                },
            ];
            for body in &mut bodies {
                body.bound_worker_payload();
                let budget = if matches!(body, Body::SessionList { .. } | Body::ProbeReport { .. })
                {
                    LIST_ENVELOPE_BYTES
                } else {
                    MAX_ENVELOPE_BYTES
                };
                assert!(size(body) <= budget);
                match body {
                    Body::Finished { result, .. } => {
                        assert!(result.len() <= RESULT_BYTES);
                        assert!(text.starts_with(result.as_str()));
                        let next = text[result.len()..].chars().next();
                        assert!(next.is_none_or(|c| result.len() + c.len_utf8() > RESULT_BYTES));
                    }
                    Body::Progress { summary, .. } => assert!(summary.chars().count() <= 200),
                    Body::PermissionAsk {
                        detail, options, ..
                    } => {
                        assert!(detail.len() <= 16 * 1024);
                        assert!(options.len() <= 32);
                        assert!(
                            options
                                .iter()
                                .all(|o| o.name.chars().count() <= 200
                                    && o.id.starts_with("choice-"))
                        );
                    }
                    Body::SessionList { sessions, .. } => assert!(
                        sessions
                            .iter()
                            .all(|s| s.title.as_ref().unwrap().chars().count() <= 120)
                    ),
                    _ => {}
                }
                let once = body.clone();
                body.bound_worker_payload();
                assert_eq!(*body, once);
            }
        }
    }
}
#[test]
fn count_bounds_and_oversize_opaque_fields_fail_closed() {
    let mut list = Body::SessionList {
        request_id: Uuid::nil(),
        sessions: vec![session("s"); 5000],
        truncated: false,
    };
    list.bound_worker_payload();
    assert!(
        matches!(list, Body::SessionList { ref sessions, truncated: true, .. } if sessions.len() == 200)
    );
    let mut list = Body::SessionList {
        request_id: Uuid::nil(),
        sessions: vec![session(&"\0".repeat(2048)); 200],
        truncated: false,
    };
    list.bound_worker_payload();
    assert!(size(&list) <= LIST_ENVELOPE_BYTES);
    assert!(
        matches!(list, Body::SessionList { ref sessions, truncated: true, .. } if !sessions.is_empty() && sessions.len() < 200)
    );
    let mut probe = Body::ProbeReport {
        capabilities: Capabilities {
            agents: vec![
                AgentCapability {
                    models: vec!["m".into(); 1000],
                    ..Default::default()
                };
                3
            ],
            repositories: vec!["/repo".into(); 5000],
            ..Default::default()
        },
    };
    probe.bound_worker_payload();
    let Body::ProbeReport { capabilities } = &probe else {
        panic!()
    };
    assert_eq!(capabilities.repositories.len(), 200);
    assert!(capabilities.agents.iter().all(|a| a.models.len() == 100));
    assert!(size(&probe) <= LIST_ENVELOPE_BYTES);
    let task_id = TaskId::new();
    for mut body in [
        Body::Accepted {
            task_id,
            session_id: "x".repeat(MAX_ENVELOPE_BYTES),
        },
        Body::Finished {
            task_id,
            session_id: "x".repeat(MAX_ENVELOPE_BYTES),
            stop_reason: "end_turn".into(),
            result: "done".into(),
        },
    ] {
        body.bound_worker_payload();
        assert!(matches!(body, Body::Failed { task_id: id, .. } if id == task_id));
        assert!(size(&body) <= MAX_ENVELOPE_BYTES);
    }
    let mut ask = Body::PermissionAsk {
        request_id: Uuid::nil(),
        task_id,
        tool: "edit".into(),
        detail: "details".into(),
        options: vec![PermissionOption {
            id: "界".repeat(1024),
            name: "Allow".into(),
            kind: PermissionOptionKind::AllowOnce,
        }],
    };
    ask.bound_worker_payload();
    assert!(matches!(ask, Body::PermissionAsk { ref options, .. } if options.is_empty()));
}
#[test]
fn limits_timestamp_ordering_and_legacy_wire_defaults() {
    assert_eq!(session_limit(None), 50);
    assert_eq!(session_limit(Some(99999)), 200);
    assert_eq!(session_limit(Some(0)), 0);
    assert_eq!(
        session_timestamp(Some("2026-11-10T01:00:00+01:00")),
        session_timestamp(Some("2026-11-10T00:00:00Z"))
    );
    assert!(session_timestamp(Some("1000")) > session_timestamp(Some("999")));
    let body: Body = serde_json::from_str(
        r#"{"type":"list_sessions","request_id":"00000000-0000-0000-0000-000000000000"}"#,
    )
    .unwrap();
    assert!(matches!(
        body,
        Body::ListSessions {
            agent: None,
            cwd: None,
            limit: None,
            ..
        }
    ));
    let body: Body = serde_json::from_str(r#"{"type":"session_list","request_id":"00000000-0000-0000-0000-000000000000","sessions":[]}"#).unwrap();
    assert!(matches!(
        body,
        Body::SessionList {
            truncated: false,
            ..
        }
    ));
}

#[test]
fn authenticated_hub_endpoints_are_strictly_bounded() {
    use navarch_proto::validate_direct_endpoints;
    assert!(validate_direct_endpoints(&vec!["[2001:db8::1]:7391".into(); 8]).is_ok());
    assert!(validate_direct_endpoints(&vec!["127.0.0.1:7391".into(); 9]).is_err());
    assert!(validate_direct_endpoints(&["x".repeat(65)]).is_err());
    assert!(validate_direct_endpoints(&["not a socket address".into()]).is_err());
    let body = navarch_proto::Body::HubEndpoints {
        direct: vec!["127.0.0.1:7391".into()],
    };
    let json = serde_json::to_string(&body).unwrap();
    assert_eq!(
        serde_json::from_str::<navarch_proto::Body>(&json).unwrap(),
        body
    );
}
