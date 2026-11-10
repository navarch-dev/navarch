mod commands;
mod scheduling;
mod sessions;
use crate::{
    Clock, EscalationHandler, Registry, SessionRecord, SessionState, TokioClock, WorkerError,
    config::{AgentConfig, Config, canonical_cwd},
    permissions::Permissions,
    registry::now,
};
use navarch_acp::{
    AcpDriver, AdapterCommand, AgentDriver, AgentEvent, NewSession, ResumeSession,
    SharedPermissionHandler,
};
use navarch_policy::Policy;
use navarch_proto::{Body, SessionId, TaskId};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::mpsc;
/// A streamed agent update correlated with its task.
#[derive(Debug)]
pub struct TextEvent {
    /// Task producing the update.
    pub task_id: TaskId,
    /// Reduced agent event.
    pub event: AgentEvent,
}
/// Receivers for wire messages and optional fine-grained standalone streaming.
pub struct WorkerStreams {
    /// Worker-to-hub messages.
    pub messages: mpsc::UnboundedReceiver<Body>,
    /// Fine-grained agent events for standalone output.
    pub events: mpsc::UnboundedReceiver<TextEvent>,
}
struct Turn {
    task_id: TaskId,
    prompt: String,
}
struct Session {
    queue: VecDeque<Turn>,
    active: Option<TaskId>,
    restored: bool,
}
struct State {
    registry: Registry,
    sessions: HashMap<SessionId, Session>,
    starting: usize,
    running: usize,
}
struct Inner {
    config: Config,
    drivers: HashMap<String, Arc<dyn AgentDriver>>,
    permissions: Arc<Permissions>,
    state: Mutex<State>,
    output: mpsc::UnboundedSender<Body>,
    events: mpsc::UnboundedSender<TextEvent>,
}
/// Shared standalone and future hub worker path.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}
impl Worker {
    /// Load config, policy and sessions from the resolved `NAVARCH_HOME` directory.
    /// No adapter starts until a policy-approved operation or explicit probe.
    pub fn load(
        home: &Path,
        user_home: &Path,
        escalation: Option<Arc<dyn EscalationHandler>>,
    ) -> Result<(Self, WorkerStreams), WorkerError> {
        let config = Config::load(home)?;
        let policy = crate::config::load_policy(home, user_home)?;
        Self::with_factory(
            config,
            policy,
            Registry::load(home)?,
            escalation,
            Arc::new(TokioClock),
            |agent, permissions| {
                Arc::new(AcpDriver::new(
                    AdapterCommand {
                        command: agent.command.clone(),
                        args: agent.args.clone(),
                    },
                    permissions,
                ))
            },
        )
    }
    /// Construct a runtime with injectable drivers, permission UI and clock.
    pub fn with_factory(
        config: Config,
        policy: Policy,
        registry: Registry,
        escalation: Option<Arc<dyn EscalationHandler>>,
        clock: Arc<dyn Clock>,
        factory: impl Fn(&AgentConfig, SharedPermissionHandler) -> Arc<dyn AgentDriver>,
    ) -> Result<(Self, WorkerStreams), WorkerError> {
        let (output, messages) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        let permissions = Arc::new(Permissions::new(
            Arc::new(policy),
            output.clone(),
            escalation,
            clock,
        ));
        let drivers = config
            .agents
            .iter()
            .map(|a| (a.name.clone(), factory(a, permissions.scoped(&a.name))))
            .collect();
        let sessions = registry
            .sessions()
            .into_iter()
            .map(|r| {
                (
                    r.session_id,
                    Session {
                        queue: VecDeque::new(),
                        active: None,
                        restored: false,
                    },
                )
            })
            .collect();
        Ok((
            Self {
                inner: Arc::new(Inner {
                    config,
                    drivers,
                    permissions,
                    state: Mutex::new(State {
                        registry,
                        sessions,
                        starting: 0,
                        running: 0,
                    }),
                    output,
                    events,
                }),
            },
            WorkerStreams {
                messages,
                events: event_rx,
            },
        ))
    }
    fn state(&self) -> Result<MutexGuard<'_, State>, WorkerError> {
        self.inner
            .state
            .lock()
            .map_err(|_| WorkerError::Invalid("worker state lock poisoned".into()))
    }
    pub(crate) fn config(&self) -> &Config {
        &self.inner.config
    }
    pub(crate) fn drivers(&self) -> &HashMap<String, Arc<dyn AgentDriver>> {
        &self.inner.drivers
    }
    pub(crate) fn policy(&self) -> &Policy {
        &self.inner.permissions.policy
    }
    /// Current running turn count.
    pub fn running_turns(&self) -> usize {
        self.state()
            .map(|s| s.running + s.starting)
            .unwrap_or_default()
    }
    /// List persisted worker sessions without starting adapters.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>, WorkerError> {
        Ok(self.state()?.registry.sessions())
    }
    fn emit(&self, mut body: Body) {
        body.bound_worker_payload();
        let _ = self.inner.output.send(body);
    }
    /// Route the hub-to-worker operations through the same standalone runtime.
    /// Envelope versioning/acknowledgements belong to the future link receiver.
    pub async fn handle(&self, body: Body) -> Result<(), WorkerError> {
        match body {
            Body::Dispatch {
                task_id,
                agent,
                cwd,
                prompt,
                mode,
                model,
                mcp_servers,
            } => {
                self.dispatch(
                    task_id,
                    &agent,
                    NewSession {
                        cwd,
                        mode,
                        model,
                        mcp_servers: mcp_servers.unwrap_or_default(),
                    },
                    prompt,
                )
                .await
            }
            Body::SendToSession {
                task_id,
                session_id,
                prompt,
                agent,
            } => {
                self.send_target(task_id, &session_id, agent.as_deref(), prompt)
                    .await
            }
            Body::Cancel { task_id } => self.cancel(task_id).await,
            Body::PermissionAnswer {
                request_id,
                decision,
            } => {
                self.answer_permission(request_id, decision);
                Ok(())
            }
            Body::Probe => {
                self.emit(Body::ProbeReport {
                    capabilities: self.probe().await,
                });
                Ok(())
            }
            Body::ListSessions {
                request_id,
                agent,
                cwd,
                limit,
            } => {
                let (sessions, truncated) = self
                    .query_sessions(agent.as_deref(), cwd.as_deref(), limit)
                    .await?;
                self.emit(Body::SessionList {
                    request_id,
                    sessions,
                    truncated,
                });
                Ok(())
            }
            Body::Ack { .. } => Ok(()),
            _ => Err(WorkerError::Invalid(
                "expected a hub-to-worker message".into(),
            )),
        }
    }
}
