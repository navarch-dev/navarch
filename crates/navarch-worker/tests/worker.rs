//! Worker tests use only in-memory scripted drivers and temporary files.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use async_trait::async_trait;
use navarch_acp::{AgentEvent, FakeDriver, FakeStep, NewSession};
use navarch_policy::Policy;
use navarch_proto::*;
use navarch_worker::{
    config::{AgentConfig, Config},
    *,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
struct ManualClock(tokio::sync::Semaphore);
#[async_trait]
impl Clock for ManualClock {
    async fn sleep(&self, duration: Duration) {
        assert_eq!(duration, Duration::from_secs(600));
        let permit = self.0.acquire().await.unwrap();
        permit.forget();
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    worker: Worker,
    streams: WorkerStreams,
    driver: Arc<FakeDriver>,
    clock: Arc<ManualClock>,
}
impl Fixture {
    fn new(scripts: Vec<Vec<FakeStep>>, limit: usize, default: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = Config {
            local_hub_socket: None,
            direct: true,
            display_name: "test".into(),
            agents: vec![AgentConfig {
                name: "fake".into(),
                command: "/nonexistent-adapter".into(),
                args: vec![],
                mode: None,
            }],
        };
        let mut policy = Policy::parse(&format!(
            "allowed_agents=['fake']\nmax_concurrent_turns={limit}\ndefault='{default}'"
        ))
        .unwrap();
        policy.allowed_roots = vec![root];
        let driver = OnceLock::new();
        let clock = Arc::new(ManualClock(tokio::sync::Semaphore::new(0)));
        let (worker, streams) = Worker::with_factory(
            config,
            policy,
            Registry::load(temp.path()).unwrap(),
            None,
            clock.clone(),
            |_, permissions| {
                let fake = Arc::new(FakeDriver::new(scripts.clone(), permissions));
                driver.set(fake.clone()).ok();
                fake
            },
        )
        .unwrap();
        Self {
            temp,
            worker,
            streams,
            driver: driver.get().unwrap().clone(),
            clock,
        }
    }
    fn req(&self) -> NewSession {
        NewSession {
            cwd: self.temp.path().canonicalize().unwrap(),
            mode: None,
            model: None,
            mcp_servers: vec![],
        }
    }
    async fn message(&mut self) -> Body {
        tokio::time::timeout(Duration::from_secs(3), self.streams.messages.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn accepted(&mut self) -> (TaskId, String) {
        match self.message().await {
            Body::Accepted {
                task_id,
                session_id,
            } => (task_id, session_id),
            body => panic!("expected Accepted, got {body:?}"),
        }
    }
    async fn finished(&mut self, task: TaskId) -> Body {
        loop {
            let body = self.message().await;
            if matches!(&body,Body::Finished{task_id,..}|Body::Failed{task_id,..} if *task_id==task)
            {
                return body;
            }
        }
    }
    async fn event(&mut self) -> TextEvent {
        tokio::time::timeout(Duration::from_secs(3), self.streams.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
fn permission() -> PermissionRequest {
    PermissionRequest {
        session_id: String::new(),
        tool: "Edit".into(),
        kind: ToolKind::Edit,
        detail: "Edit file".into(),
        paths: vec!["file.txt".into()],
        command: None,
        options: vec![
            PermissionOption {
                id: "yes".into(),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
            },
            PermissionOption {
                id: "no".into(),
                name: "Deny".into(),
                kind: PermissionOptionKind::RejectOnce,
            },
        ],
    }
}
#[tokio::test]
async fn dispatch_refuses_before_start_and_limits_capacity() {
    let mut f = Fixture::new(
        vec![vec![
            FakeStep::Reply("started".into()),
            FakeStep::Delay(Duration::from_secs(60)),
        ]],
        1,
        "deny",
    );
    let mut outside = f.req();
    outside.cwd = PathBuf::from("/");
    let task = TaskId::new();
    assert!(
        f.worker
            .dispatch(task, "fake", outside, "hi".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("allowed_roots")
    );
    assert!(matches!(f.message().await, Body::Rejected { .. }));
    assert!(f.worker.sessions().unwrap().is_empty());
    let task = TaskId::new();
    f.worker
        .dispatch(task, "fake", f.req(), "hi".into())
        .await
        .unwrap();
    let (_, id) = f.accepted().await;
    assert_eq!(
        f.event().await.event,
        AgentEvent::MessageChunk("started".into())
    );
    assert!(
        f.worker
            .dispatch(TaskId::new(), "fake", f.req(), "other".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("max_concurrent_turns")
    );
    assert!(matches!(f.message().await, Body::Rejected { .. }));
    assert_eq!(f.worker.sessions().unwrap().len(), 1);
    assert_eq!(f.worker.running_turns(), 1);
    f.worker.cancel(task).await.unwrap();
    assert!(
        matches!(f.finished(task).await,Body::Finished{stop_reason,..} if stop_reason=="cancelled")
    );
    assert_eq!(f.worker.sessions().unwrap()[0].session_id, id);
}
#[tokio::test]
async fn busy_session_prompts_are_fifo_and_cancellation_frees_capacity() {
    let mut f = Fixture::new(
        vec![
            vec![
                FakeStep::Reply("started".into()),
                FakeStep::Delay(Duration::from_secs(60)),
            ],
            vec![FakeStep::Reply("second".into())],
            vec![FakeStep::Reply("third".into())],
        ],
        1,
        "deny",
    );
    let first = TaskId::new();
    f.worker
        .dispatch(first, "fake", f.req(), "one".into())
        .await
        .unwrap();
    let (_, id) = f.accepted().await;
    f.event().await;
    let second = TaskId::new();
    let third = TaskId::new();
    f.worker.send(second, &id, "two".into()).await.unwrap();
    f.accepted().await;
    f.worker.send(third, &id, "three".into()).await.unwrap();
    f.accepted().await;
    assert_eq!(f.worker.running_turns(), 1);
    f.worker.cancel(first).await.unwrap();
    assert!(
        matches!(f.finished(first).await,Body::Finished{stop_reason,..} if stop_reason=="cancelled")
    );
    assert!(matches!(f.finished(second).await,Body::Finished{result,..} if result=="second"));
    assert!(matches!(f.finished(third).await,Body::Finished{result,..} if result=="third"));
    assert_eq!(f.worker.running_turns(), 0);
}
#[tokio::test]
async fn permissions_answer_and_injected_ten_minute_timeout() {
    let mut f = Fixture::new(
        vec![
            vec![FakeStep::Permission(permission())],
            vec![FakeStep::Permission(permission())],
        ],
        1,
        "escalate",
    );
    let task = TaskId::new();
    f.worker
        .dispatch(task, "fake", f.req(), "one".into())
        .await
        .unwrap();
    let (_, id) = f.accepted().await;
    let request_id = match f.message().await {
        Body::PermissionAsk {
            request_id,
            task_id,
            ..
        } => {
            assert_eq!(task_id, task);
            request_id
        }
        body => panic!("{body:?}"),
    };
    f.worker
        .handle(Body::PermissionAnswer {
            request_id,
            decision: PermissionDecision::Selected {
                option_id: "yes".into(),
            },
        })
        .await
        .unwrap();
    assert!(matches!(f.finished(task).await, Body::Finished { .. }));
    assert!(matches!(f.event().await.event,AgentEvent::Other(text) if text.contains("yes")));
    let task = TaskId::new();
    f.worker.send(task, &id, "two".into()).await.unwrap();
    f.accepted().await;
    assert!(matches!(f.message().await, Body::PermissionAsk { .. }));
    f.clock.0.add_permits(1);
    assert!(matches!(f.finished(task).await, Body::Finished { .. }));
    assert_eq!(f.event().await.event, AgentEvent::Other("Cancelled".into()));
}
#[tokio::test]
async fn permission_policy_allow_and_deny_skip_escalation() {
    for default in ["allow", "deny"] {
        let mut f = Fixture::new(vec![vec![FakeStep::Permission(permission())]], 1, default);
        let task = TaskId::new();
        f.worker
            .dispatch(task, "fake", f.req(), "one".into())
            .await
            .unwrap();
        f.accepted().await;
        assert!(matches!(f.message().await, Body::Finished { .. }));
    }
    let req = permission();
    assert_eq!(
        policy_answer(navarch_policy::Decision::Deny, &req),
        PermissionDecision::Selected {
            option_id: "no".into()
        }
    );
    assert_eq!(
        policy_answer(navarch_policy::Decision::Allow { remember: false }, &req),
        PermissionDecision::Selected {
            option_id: "yes".into()
        }
    );
}
#[tokio::test]
async fn registry_survives_worker_restart_and_resumes_lazily() {
    let mut f = Fixture::new(vec![], 1, "deny");
    let task = TaskId::new();
    f.worker
        .dispatch(task, "fake", f.req(), "one".into())
        .await
        .unwrap();
    let (_, id) = f.accepted().await;
    f.finished(task).await;
    let registry = Registry::load(f.temp.path()).unwrap();
    assert_eq!(registry.sessions()[0].state, SessionState::Idle);
    let (worker, mut streams) = Worker::with_factory(
        f.worker_config(),
        f.worker_policy(),
        registry,
        None,
        Arc::new(TokioClock),
        |_, _| f.driver.clone(),
    )
    .unwrap();
    assert_eq!(f.driver.resume_count(), 0);
    let task = TaskId::new();
    worker.send(task, &id, "again".into()).await.unwrap();
    assert!(matches!(
        streams.messages.recv().await.unwrap(),
        Body::Accepted { .. }
    ));
    assert!(matches!(
        streams.messages.recv().await.unwrap(),
        Body::Finished { .. }
    ));
    assert_eq!(f.driver.resume_count(), 1);
}
impl Fixture {
    fn worker_config(&self) -> Config {
        Config {
            local_hub_socket: None,
            direct: true,
            display_name: "test".into(),
            agents: vec![AgentConfig {
                name: "fake".into(),
                command: "/nonexistent".into(),
                args: vec![],
                mode: None,
            }],
        }
    }
    fn worker_policy(&self) -> Policy {
        let mut policy = Policy::parse("allowed_agents=['fake']\ndefault='deny'").unwrap();
        policy.allowed_roots = vec![self.temp.path().canonicalize().unwrap()];
        policy
    }
}
#[test]
fn missing_corrupt_registry_and_non_overwriting_init() {
    let temp = tempfile::tempdir().unwrap();
    assert!(Registry::load(temp.path()).unwrap().sessions().is_empty());
    let path = temp.path().join("worker/sessions.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{broken").unwrap();
    assert!(Registry::load(temp.path()).unwrap().sessions().is_empty());
    assert!(!path.exists());
    let backup = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(backup).unwrap(), b"{broken");
    config::init(temp.path()).unwrap();
    std::fs::write(temp.path().join("worker.toml"), "# mine").unwrap();
    config::init(temp.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("worker.toml")).unwrap(),
        "# mine"
    );
    assert!(
        config::load_policy(temp.path(), temp.path())
            .unwrap()
            .allowed_roots
            .is_empty()
    );
}
#[test]
fn config_errors_name_file_and_field_and_expand_home() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("worker.toml");
    let error = Config::parse("[[agents]]\nname='fake'\ncommand=42", &path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("worker.toml") && error.contains("command"));
    let policy = temp.path().join("worker-policy.toml");
    std::fs::write(&policy, "allowed_roots=['~']\nallowed_agents=['fake']").unwrap();
    assert_eq!(
        config::load_policy(temp.path(), temp.path())
            .unwrap()
            .allowed_roots,
        vec![temp.path().canonicalize().unwrap()]
    );
    std::fs::write(&policy, "max_concurrent_turns='wrong'").unwrap();
    let error = config::load_policy(temp.path(), temp.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("worker-policy.toml") && error.contains("max_concurrent_turns"));
    assert_eq!(
        AgentConfig {
            name: "claude".into(),
            command: "npx".into(),
            args: vec![],
            mode: None
        }
        .mode()
        .as_deref(),
        Some("default")
    );
    assert_eq!(
        AgentConfig {
            name: "codex".into(),
            command: "npx".into(),
            args: vec![],
            mode: None
        }
        .mode()
        .as_deref(),
        Some("workspace-write")
    );
}
#[tokio::test]
async fn probe_missing_adapter_and_repository_depth() {
    let f = Fixture::new(vec![], 1, "deny");
    for sub in ["repo/.git", "one/two/.git", "one/two/three/.git"] {
        std::fs::create_dir_all(f.temp.path().join(sub)).unwrap();
    }
    let report = f.worker.probe().await;
    assert_eq!(report.repositories.len(), 2);
    assert!(!report.agents[0].present);
    assert!(report.agents[0].error.is_some());
    assert!(f.worker.doctor()[0].contains("missing"));
}
#[cfg(unix)]
#[test]
fn filesystem_resolution_rejects_symlink_escape_and_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let mut policy = Policy::parse("allowed_agents=['fake']").unwrap();
    policy.allowed_roots = vec![root.canonicalize().unwrap()];
    for path in [root.join("../outside"), root.join("escape")] {
        assert!(
            policy
                .check_dispatch(&config::canonical_cwd(&path).unwrap(), "fake")
                .is_err()
        );
    }
    assert!(config::canonical_cwd(Path::new("/definitely-missing-navarch-cwd")).is_err());
}

#[tokio::test]
async fn probe_discovers_versions_and_models_from_throwaway_session() {
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        local_hub_socket: None,
        direct: true,
        display_name: "test".into(),
        agents: vec![AgentConfig {
            name: "fake".into(),
            command: std::env::current_exe().unwrap(),
            args: vec![],
            mode: None,
        }],
    };
    let (worker, _) = Worker::with_factory(
        config,
        Policy::default(),
        Registry::load(temp.path()).unwrap(),
        None,
        Arc::new(TokioClock),
        |_, permissions| Arc::new(FakeDriver::new(vec![], permissions)),
    )
    .unwrap();
    let report = worker.probe().await;
    assert_eq!(report.agents[0].version.as_deref(), Some("fake-1"));
    assert_eq!(report.agents[0].models, vec!["fake-model"]);
    assert!(worker.sessions().unwrap().is_empty());
}
#[tokio::test]
async fn queued_task_can_be_cancelled_without_running_it() {
    let mut f = Fixture::new(
        vec![vec![
            FakeStep::Reply("started".into()),
            FakeStep::Delay(Duration::from_secs(60)),
        ]],
        1,
        "deny",
    );
    let first = TaskId::new();
    f.worker
        .dispatch(first, "fake", f.req(), "one".into())
        .await
        .unwrap();
    let (_, id) = f.accepted().await;
    f.event().await;
    let queued = TaskId::new();
    f.worker.send(queued, &id, "two".into()).await.unwrap();
    f.accepted().await;
    f.worker.cancel(queued).await.unwrap();
    assert!(
        matches!(f.finished(queued).await,Body::Finished{stop_reason,..} if stop_reason=="cancelled")
    );
    f.worker.cancel(first).await.unwrap();
    f.finished(first).await;
    assert_eq!(f.worker.sessions().unwrap()[0].state, SessionState::Idle);
}
#[tokio::test]
async fn cancelling_permission_wait_cancels_turn() {
    let mut f = Fixture::new(
        vec![vec![
            FakeStep::Permission(permission()),
            FakeStep::Delay(Duration::from_secs(60)),
        ]],
        1,
        "escalate",
    );
    let task = TaskId::new();
    f.worker
        .dispatch(task, "fake", f.req(), "one".into())
        .await
        .unwrap();
    f.accepted().await;
    assert!(matches!(f.message().await, Body::PermissionAsk { .. }));
    f.worker.cancel(task).await.unwrap();
    assert!(
        matches!(f.finished(task).await,Body::Finished{stop_reason,..} if stop_reason=="cancelled")
    );
}

#[tokio::test]
async fn different_adapters_with_same_session_id_route_turns_and_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        local_hub_socket: None,
        direct: true,
        display_name: "test".into(),
        agents: ["a", "b"]
            .into_iter()
            .map(|name| AgentConfig {
                name: name.into(),
                command: "/not-executed".into(),
                args: vec![],
                mode: None,
            })
            .collect(),
    };
    let mut policy =
        Policy::parse("allowed_agents=['a','b']\ndefault='escalate'\nmax_concurrent_turns=2")
            .unwrap();
    policy.allowed_roots = vec![temp.path().canonicalize().unwrap()];
    let (worker, mut streams) = Worker::with_factory(
        config,
        policy,
        Registry::load(temp.path()).unwrap(),
        None,
        Arc::new(TokioClock),
        |agent, permissions| {
            Arc::new(FakeDriver::new(
                vec![vec![
                    FakeStep::Permission(permission()),
                    FakeStep::Reply(agent.name.clone()),
                ]],
                permissions,
            ))
        },
    )
    .unwrap();
    let req = NewSession {
        cwd: temp.path().to_owned(),
        mode: None,
        model: None,
        mcp_servers: vec![],
    };
    let a = TaskId::new();
    let b = TaskId::new();
    worker
        .dispatch(a, "a", req.clone(), "a".into())
        .await
        .unwrap();
    worker.dispatch(b, "b", req, "b".into()).await.unwrap();
    let mut finished = std::collections::HashSet::new();
    let mut asked = std::collections::HashSet::new();
    while finished.len() < 2 {
        match tokio::time::timeout(Duration::from_secs(3), streams.messages.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Body::PermissionAsk {
                request_id,
                task_id,
                ..
            } => {
                asked.insert(task_id);
                worker.answer_permission(
                    request_id,
                    PermissionDecision::Selected {
                        option_id: "yes".into(),
                    },
                );
            }
            Body::Finished {
                task_id, result, ..
            } => {
                assert_eq!(result, if task_id == a { "a" } else { "b" });
                finished.insert(task_id);
            }
            Body::Failed { error, .. } | Body::Rejected { reason: error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    assert_eq!(asked, [a, b].into());
    let records = worker.sessions().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].session_id, records[1].session_id);
    assert_eq!(records[0].adapter_session_id, records[1].adapter_session_id);
}

#[tokio::test]
async fn discover_and_adopt_external_sessions_only_inside_allowed_roots() {
    use navarch_acp::AgentDriver;
    let mut f = Fixture::new(vec![vec![FakeStep::Reply("adopted".into())]], 1, "deny");
    let external = f.driver.new_session(f.req()).await.unwrap().session_id;
    let mut outside = f.req();
    outside.cwd = PathBuf::from("/");
    let outside = f.driver.new_session(outside).await.unwrap().session_id;
    let request_id = uuid::Uuid::new_v4();
    f.worker
        .handle(Body::ListSessions {
            agent: None,
            limit: None,
            request_id,
            cwd: Some(f.temp.path().to_owned()),
        })
        .await
        .unwrap();
    let Body::SessionList {
        request_id: reply_id,
        sessions,
        ..
    } = f.message().await
    else {
        panic!("expected list");
    };
    assert_eq!(reply_id, request_id);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].adapter_session_id, external);
    assert!(!sessions[0].navarch_started);
    assert!(sessions[0].session_id.is_none());
    let denied = TaskId::new();
    assert!(
        f.worker
            .send_target(denied, &outside, Some("fake"), "no".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("allowed_roots")
    );
    assert!(matches!(f.message().await, Body::Rejected { task_id, .. } if task_id == denied));
    assert!(f.worker.sessions().unwrap().is_empty());
    assert_eq!(f.driver.resume_count(), 0);
    let accepted = TaskId::new();
    f.worker
        .handle(Body::SendToSession {
            task_id: accepted,
            agent: Some("fake".into()),
            session_id: external.clone(),
            prompt: "yes".into(),
        })
        .await
        .unwrap();
    let (_, public) = f.accepted().await;
    assert_ne!(public, external);
    assert!(
        matches!(f.finished(accepted).await, Body::Finished { result, .. } if result == "adopted")
    );
    assert_eq!(f.driver.resume_count(), 1);
    let sessions = f.worker.list_sessions(None).await.unwrap();
    assert_eq!(sessions.len(), 2);
    let session = sessions
        .iter()
        .find(|s| s.adapter_session_id == external)
        .unwrap();
    assert_eq!(session.session_id.as_deref(), Some(public.as_str()));
    assert!(!session.navarch_started);
    assert!(!Registry::load(f.temp.path()).unwrap().sessions()[0].navarch_started);
    let second = TaskId::new();
    f.worker
        .send_target(second, &external, Some("fake"), "again".into())
        .await
        .unwrap();
    assert_eq!(f.accepted().await.1, public);
    f.finished(second).await;
    assert_eq!(f.worker.sessions().unwrap().len(), 1);
}
#[tokio::test]
async fn session_discovery_merges_registry_and_adapter_and_honours_query_limit() {
    use navarch_acp::AgentDriver;
    let mut f = Fixture::new(vec![], 1, "deny");
    let turn = TaskId::new();
    f.worker
        .dispatch(turn, "fake", f.req(), "start".into())
        .await
        .unwrap();
    let (_, public) = f.accepted().await;
    f.finished(turn).await;
    for _ in 0..505 {
        f.driver.new_session(f.req()).await.unwrap();
    }
    let sessions = f.worker.list_sessions(None).await.unwrap();
    assert_eq!(sessions.len(), 50);
    let (capped, truncated) = f
        .worker
        .query_sessions(None, None, Some(5000))
        .await
        .unwrap();
    assert_eq!(capped.len(), 200);
    assert!(truncated);
    let owned = sessions
        .iter()
        .find(|s| s.session_id.as_deref() == Some(&public))
        .unwrap();
    assert!(owned.navarch_started);
    assert!(owned.updated_at.is_some());
    assert_eq!(sessions.iter().filter(|s| s.navarch_started).count(), 1);
    assert!(
        f.worker
            .send_target(TaskId::new(), "missing", Some("unknown"), "no".into())
            .await
            .is_err()
    );
    assert_eq!(f.worker.sessions().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_permission_details_and_choices_are_bounded_before_emission() {
    let mut request = permission();
    request.detail = "界\0".repeat(30000);
    request.tool = "tool".repeat(1000);
    request.options = (0..50)
        .map(|i| PermissionOption {
            id: format!("choice-{i}"),
            name: "界".repeat(1000),
            kind: PermissionOptionKind::AllowOnce,
        })
        .collect();
    let mut f = Fixture::new(vec![vec![FakeStep::Permission(request)]], 1, "escalate");
    let task_id = TaskId::new();
    f.worker
        .dispatch(task_id, "fake", f.req(), "test".into())
        .await
        .unwrap();
    f.accepted().await;
    let body = f.message().await;
    assert!(encoded_envelope_len(&body).unwrap() <= MAX_ENVELOPE_BYTES);
    let Body::PermissionAsk {
        request_id,
        tool,
        detail,
        options,
        ..
    } = body
    else {
        panic!()
    };
    assert_eq!(tool.chars().count(), 200);
    assert!(detail.len() <= 16 * 1024);
    assert_eq!(options.len(), 32);
    assert!(options.iter().all(|o| o.name.chars().count() == 200));
    // A choice removed by the wire bound must not authorize the operation.
    f.worker.answer_permission(
        request_id,
        PermissionDecision::Selected {
            option_id: "choice-49".into(),
        },
    );
    f.finished(task_id).await;
    assert_eq!(f.event().await.event, AgentEvent::Other("Cancelled".into()));
}

#[tokio::test]
async fn probe_repository_discovery_stops_at_two_hundred() {
    let f = Fixture::new(vec![], 1, "deny");
    for i in 0..250 {
        std::fs::create_dir_all(f.temp.path().join(format!("repo-{i}/.git"))).unwrap();
    }
    let report = f.worker.probe().await;
    assert_eq!(report.repositories.len(), 200);
    assert!(
        encoded_envelope_len(&Body::ProbeReport {
            capabilities: report
        })
        .unwrap()
            <= LIST_ENVELOPE_BYTES
    );
}
