use crate::*;
use async_trait::async_trait;
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, watch};
/// One scripted in-memory turn action.
#[derive(Clone, Debug)]
pub enum FakeStep {
    /// Emit text.
    Reply(String),
    /// Emit a tool call.
    Tool(String),
    /// Ask the supplied permission handler.
    Permission(PermissionRequest),
    /// Wait for cancellation or the duration.
    Delay(std::time::Duration),
    /// Fail a turn.
    Crash,
}
/// Scripted offline driver for worker tests.
pub struct FakeDriver {
    scripts: Mutex<VecDeque<Vec<FakeStep>>>,
    sessions: Mutex<HashMap<String, (NewSession, watch::Sender<bool>)>>,
    listed: Mutex<Vec<SessionInfo>>,
    permissions: SharedPermissionHandler,
    next: AtomicUsize,
    resumes: AtomicUsize,
}
impl FakeDriver {
    /// Build a driver with one action list per prompt.
    pub fn new(scripts: Vec<Vec<FakeStep>>, permissions: SharedPermissionHandler) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            sessions: Mutex::new(HashMap::new()),
            permissions,
            listed: Mutex::new(vec![]),
            next: AtomicUsize::new(1),
            resumes: AtomicUsize::new(0),
        }
    }
    /// Set externally discoverable sessions without starting a turn.
    pub async fn set_listed_sessions(&self, sessions: Vec<SessionInfo>) {
        *self.listed.lock().await = sessions;
    }
    /// Number of explicit session restorations.
    pub fn resume_count(&self) -> usize {
        self.resumes.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl AgentDriver for FakeDriver {
    async fn new_session(&self, req: NewSession) -> Result<SessionHandle, DriverError> {
        let id = format!("fake-{}", self.next.fetch_add(1, Ordering::SeqCst));
        self.sessions
            .lock()
            .await
            .insert(id.clone(), (req, watch::channel(false).0));
        Ok(SessionHandle {
            session_id: id,
            models: vec!["fake-model".into()],
            agent_version: Some("fake-1".into()),
        })
    }
    async fn resume_session(&self, req: ResumeSession) -> Result<SessionHandle, DriverError> {
        self.resumes.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().await.insert(
            req.session_id.clone(),
            (req.config, watch::channel(false).0),
        );
        Ok(SessionHandle {
            session_id: req.session_id,
            models: vec![],
            agent_version: Some("fake-1".into()),
        })
    }
    async fn prompt(
        &self,
        id: &SessionId,
        _text: &str,
        events: EventSink,
    ) -> Result<TurnOutcome, DriverError> {
        let mut cancel = {
            let mut sessions = self.sessions.lock().await;
            let (_, sender) = sessions
                .get_mut(id)
                .ok_or_else(|| DriverError::UnknownSession(id.clone()))?;
            sender.send_replace(false);
            sender.subscribe()
        };
        let script = self
            .scripts
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| vec![FakeStep::Reply("OK".into())]);
        let work = async {
            let mut result = String::new();
            for step in script {
                match step {
                    FakeStep::Reply(text) => {
                        append_bounded(&mut result, &text);
                        let _ = events.send(AgentEvent::MessageChunk(text));
                    }
                    FakeStep::Tool(text) => {
                        let _ = events.send(AgentEvent::ToolCall(text));
                    }
                    FakeStep::Permission(mut request) => {
                        request.session_id = id.clone();
                        let decision = self.permissions.request(request).await;
                        let text = match decision {
                            PermissionDecision::Selected { option_id } => {
                                format!("Selected {option_id}")
                            }
                            PermissionDecision::Cancelled => "Cancelled".into(),
                        };
                        let _ = events.send(AgentEvent::Other(text));
                    }
                    FakeStep::Delay(duration) => tokio::time::sleep(duration).await,
                    FakeStep::Crash => return Err(DriverError::Adapter("scripted crash".into())),
                }
            }
            Ok(TurnOutcome {
                stop_reason: "end_turn".into(),
                result,
            })
        };
        tokio::select! {result=work=>result,_=cancel.changed()=>Ok(TurnOutcome{stop_reason:"cancelled".into(),result:String::new()})}
    }
    async fn cancel(&self, id: &SessionId) -> Result<(), DriverError> {
        if let Some((_, tx)) = self.sessions.lock().await.get(id) {
            tx.send_replace(true);
        }
        Ok(())
    }
    async fn list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionInfo>, DriverError> {
        let mut result: Vec<_> = self
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, (req, _))| cwd.is_none_or(|cwd| cwd == req.cwd))
            .map(|(id, (req, _))| SessionInfo {
                session_id: id.clone(),
                cwd: req.cwd.clone(),
                updated_at: None,
                title: None,
            })
            .collect();
        result.extend(
            self.listed
                .lock()
                .await
                .iter()
                .filter(|s| cwd.is_none_or(|cwd| cwd == s.cwd))
                .cloned(),
        );
        result.sort_by_key(|s| {
            std::cmp::Reverse(navarch_proto::session_timestamp(s.updated_at.as_deref()))
        });
        Ok(result)
    }
}
