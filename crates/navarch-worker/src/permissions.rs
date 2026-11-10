//! Policy-first permission handling with channel and terminal escalation adapters.
use crate::config::permission_path;
use async_trait::async_trait;
use navarch_acp::PermissionHandler;
use navarch_policy::{Decision, Policy};
use navarch_proto::{Body, PermissionDecision, PermissionOptionKind, PermissionRequest, TaskId};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
/// Injectable timer for the ten-minute permission deadline.
#[async_trait]
pub trait Clock: Send + Sync {
    /// Wait for a duration.
    async fn sleep(&self, duration: Duration);
}
/// Tokio's production timer; Tokio's paused clock can drive it in tests.
pub struct TokioClock;
#[async_trait]
impl Clock for TokioClock {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
/// Pluggable human permission UI. Without one, `PermissionAnswer` supplies answers.
#[async_trait]
pub trait EscalationHandler: Send + Sync {
    /// Ask a human using the original offered choices.
    async fn ask(&self, request: PermissionRequest) -> PermissionDecision;
}
type Contexts = HashMap<(String, String), (TaskId, PathBuf, String)>;
type Pending = HashMap<uuid::Uuid, (TaskId, oneshot::Sender<PermissionDecision>)>;
pub(crate) struct Permissions {
    pub policy: Arc<Policy>,
    pub contexts: Mutex<Contexts>,
    pending: Mutex<Pending>,
    output: mpsc::UnboundedSender<Body>,
    escalation: Option<Arc<dyn EscalationHandler>>,
    clock: Arc<dyn Clock>,
}
impl Permissions {
    pub fn new(
        policy: Arc<Policy>,
        output: mpsc::UnboundedSender<Body>,
        escalation: Option<Arc<dyn EscalationHandler>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            policy,
            contexts: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            output,
            escalation,
            clock,
        }
    }
    pub fn scoped(self: &Arc<Self>, agent: &str) -> Arc<dyn PermissionHandler> {
        Arc::new(AgentPermissions {
            agent: agent.to_owned(),
            permissions: self.clone(),
        })
    }
    pub fn answer(&self, id: uuid::Uuid, decision: PermissionDecision) {
        if let Ok(mut pending) = self.pending.lock()
            && let Some((_, tx)) = pending.remove(&id)
        {
            let _ = tx.send(decision);
        }
    }
    pub fn cancel(&self, task: TaskId) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, (id, _)| *id != task);
        }
    }
}
/// Pick a matching offered choice, failing closed when none is offered.
pub fn policy_answer(decision: Decision, request: &PermissionRequest) -> PermissionDecision {
    let kinds: &[PermissionOptionKind] = match decision {
        Decision::Allow { remember: true } => &[
            PermissionOptionKind::AllowAlways,
            PermissionOptionKind::AllowOnce,
        ],
        Decision::Allow { remember: false } => &[PermissionOptionKind::AllowOnce],
        Decision::Deny => &[
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ],
        Decision::Escalate => &[],
    };
    for kind in kinds {
        if let Some(option) = request.options.iter().find(|o| o.kind == *kind) {
            return PermissionDecision::Selected {
                option_id: option.id.clone(),
            };
        }
    }
    PermissionDecision::Cancelled
}
impl Permissions {
    async fn request_for(&self, agent: &str, mut request: PermissionRequest) -> PermissionDecision {
        let context = self.contexts.lock().ok().and_then(|c| {
            c.get(&(agent.to_owned(), request.session_id.clone()))
                .cloned()
        });
        let Some((task_id, cwd, session_id)) = context else {
            return PermissionDecision::Cancelled;
        };
        request.session_id = session_id;
        request.paths = request
            .paths
            .iter()
            .map(|p| permission_path(p, &cwd))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        let decision = self.policy.decide(&request);
        if decision != Decision::Escalate {
            return policy_answer(decision, &request);
        }
        let id = uuid::Uuid::new_v4();
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, (task_id, tx));
        } else {
            return PermissionDecision::Cancelled;
        }
        let _ = self.output.send(Body::PermissionAsk {
            request_id: id,
            task_id,
            tool: request.tool.clone(),
            detail: request.detail.clone(),
            options: request.options.clone(),
        });
        let human = async {
            match &self.escalation {
                Some(handler) => handler.ask(request.clone()).await,
                None => std::future::pending().await,
            }
        };
        let answer = tokio::select! {
            result=rx=>result.unwrap_or(PermissionDecision::Cancelled),
            result=human=>result,
            ()=self.clock.sleep(Duration::from_secs(600))=>PermissionDecision::Cancelled,
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        match answer {
            PermissionDecision::Selected { ref option_id }
                if !request.options.iter().any(|o| &o.id == option_id) =>
            {
                PermissionDecision::Cancelled
            }
            _ => answer,
        }
    }
}

struct AgentPermissions {
    agent: String,
    permissions: Arc<Permissions>,
}
#[async_trait]
impl PermissionHandler for AgentPermissions {
    async fn request(&self, request: PermissionRequest) -> PermissionDecision {
        self.permissions.request_for(&self.agent, request).await
    }
}
