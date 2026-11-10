mod connection;
use crate::{mapping::*, *};
use agent_client_protocol::v1::{ConnectionTo, schema as acp};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};
use tokio::sync::{Mutex, oneshot};

type Streams = Arc<
    Mutex<
        HashMap<
            String,
            (
                EventSink,
                String,
                bool,
                HashMap<String, (String, acp::ToolCallStatus)>,
            ),
        >,
    >,
>;
struct Live {
    connection: ConnectionTo<Agent>,
    initialized: acp::InitializeResponse,
    task: tokio::task::JoinHandle<Result<(), DriverError>>,
    restored: HashSet<String>,
}
impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct State {
    live: Option<Live>,
    sessions: HashMap<String, NewSession>,
}
/// A lazily started, shared ACP subprocess. One process serves all its sessions.
pub struct AcpDriver {
    command: AdapterCommand,
    permissions: SharedPermissionHandler,
    state: Mutex<State>,
    streams: Streams,
}
impl AcpDriver {
    /// Create a driver without spawning an adapter.
    pub fn new(command: AdapterCommand, permissions: SharedPermissionHandler) -> Self {
        Self {
            command,
            permissions,
            state: Mutex::new(State {
                live: None,
                sessions: HashMap::new(),
            }),
            streams: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    async fn ensure<'a>(&self, state: &'a mut State) -> Result<&'a mut Live, DriverError> {
        if state
            .live
            .as_ref()
            .is_none_or(|l| l.task.is_finished() || l.connection.is_incoming_closed())
        {
            state.live = Some(self.start().await?);
        }
        state
            .live
            .as_mut()
            .ok_or_else(|| error("adapter did not start"))
    }
    async fn configure(
        live: &Live,
        id: &str,
        req: &NewSession,
        mut options: Vec<acp::SessionConfigOption>,
    ) -> Result<SessionHandle, DriverError> {
        for (category, desired) in [("mode", &req.mode), ("model", &req.model)] {
            if let Some(value) = desired {
                let option = options
                    .iter()
                    .find(|o| is_category(o, category) && values(o).contains(value))
                    .ok_or_else(|| {
                        DriverError::Unsupported(format!(
                            "{category} value {value:?} is not offered by session config options"
                        ))
                    })?;
                options = live
                    .connection
                    .send_request(acp::SetSessionConfigOptionRequest::new(
                        id.to_owned(),
                        option.id.clone(),
                        acp::SessionConfigOptionValue::value_id(value.clone()),
                    ))
                    .block_task()
                    .await
                    .map_err(error)?
                    .config_options;
            }
        }
        let models = options
            .iter()
            .filter(|o| is_category(o, "model"))
            .flat_map(values)
            .collect();
        Ok(SessionHandle {
            session_id: id.to_owned(),
            models,
            agent_version: live
                .initialized
                .agent_info
                .as_ref()
                .map(|i| i.version.clone()),
        })
    }
    async fn restore(
        live: &mut Live,
        id: &str,
        req: &NewSession,
    ) -> Result<SessionHandle, DriverError> {
        let caps = &live.initialized.agent_capabilities;
        let options = if caps.session_capabilities.resume.is_some() {
            match live
                .connection
                .send_request(
                    acp::ResumeSessionRequest::new(id.to_owned(), &req.cwd)
                        .mcp_servers(servers(&req.mcp_servers)),
                )
                .block_task()
                .await
            {
                Ok(response) => response.config_options,
                Err(e) if caps.load_session => {
                    let _ = e;
                    live.connection
                        .send_request(
                            acp::LoadSessionRequest::new(id.to_owned(), &req.cwd)
                                .mcp_servers(servers(&req.mcp_servers)),
                        )
                        .block_task()
                        .await
                        .map_err(error)?
                        .config_options
                }
                Err(e) => return Err(error(e)),
            }
        } else if caps.load_session {
            live.connection
                .send_request(
                    acp::LoadSessionRequest::new(id.to_owned(), &req.cwd)
                        .mcp_servers(servers(&req.mcp_servers)),
                )
                .block_task()
                .await
                .map_err(error)?
                .config_options
        } else {
            return Err(DriverError::Unsupported(
                "adapter supports neither resume nor load".into(),
            ));
        };
        let handle = Self::configure(live, id, req, options.unwrap_or_default()).await?;
        live.restored.insert(id.to_owned());
        Ok(handle)
    }
}
#[async_trait]
impl AgentDriver for AcpDriver {
    async fn new_session(&self, req: NewSession) -> Result<SessionHandle, DriverError> {
        let mut state = self.state.lock().await;
        let live = self.ensure(&mut state).await?;
        let response = live
            .connection
            .send_request(
                acp::NewSessionRequest::new(&req.cwd).mcp_servers(servers(&req.mcp_servers)),
            )
            .block_task()
            .await
            .map_err(error)?;
        let id = response.session_id.to_string();
        let handle =
            Self::configure(live, &id, &req, response.config_options.unwrap_or_default()).await?;
        live.restored.insert(id.clone());
        state.sessions.insert(id, req);
        Ok(handle)
    }
    async fn resume_session(&self, req: ResumeSession) -> Result<SessionHandle, DriverError> {
        let mut state = self.state.lock().await;
        let live = self.ensure(&mut state).await?;
        let handle = Self::restore(live, &req.session_id, &req.config).await?;
        state.sessions.insert(req.session_id, req.config);
        Ok(handle)
    }
    async fn prompt(
        &self,
        session: &SessionId,
        text: &str,
        events: EventSink,
    ) -> Result<TurnOutcome, DriverError> {
        let connection = {
            let mut state = self.state.lock().await;
            let req = state
                .sessions
                .get(session)
                .cloned()
                .ok_or_else(|| DriverError::UnknownSession(session.clone()))?;
            let live = self.ensure(&mut state).await?;
            if !live.restored.contains(session) {
                Self::restore(live, session, &req).await?;
            }
            live.connection.clone()
        };
        self.streams.lock().await.insert(
            session.clone(),
            (events, String::new(), false, HashMap::new()),
        );
        let response = connection
            .send_request(acp::PromptRequest::new(
                session.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
            ))
            .block_task()
            .await;
        let result = self
            .streams
            .lock()
            .await
            .remove(session)
            .map(|(_, text, _, _)| text)
            .unwrap_or_default();
        let response = response.map_err(error)?;
        let stop_reason = serde_json::to_value(response.stop_reason)
            .map_err(error)?
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        Ok(TurnOutcome {
            stop_reason,
            result,
        })
    }
    async fn cancel(&self, session: &SessionId) -> Result<(), DriverError> {
        let state = self.state.lock().await;
        let live = state
            .live
            .as_ref()
            .ok_or_else(|| error("adapter is not running"))?;
        live.connection
            .send_notification(acp::CancelNotification::new(session.clone()))
            .map_err(error)
    }
    async fn list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionInfo>, DriverError> {
        let mut state = self.state.lock().await;
        let live = self.ensure(&mut state).await?;
        if live
            .initialized
            .agent_capabilities
            .session_capabilities
            .list
            .is_none()
        {
            return Err(DriverError::Unsupported("session/list".into()));
        }
        let mut result = vec![];
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let mut req = acp::ListSessionsRequest::default();
            req.cwd = cwd.map(Path::to_owned);
            req.cursor = cursor;
            let response = live
                .connection
                .send_request(req)
                .block_task()
                .await
                .map_err(error)?;
            result.extend(response.sessions.into_iter().map(|s| SessionInfo {
                session_id: s.session_id.to_string(),
                cwd: s.cwd,
                title: s.title,
                updated_at: s.updated_at,
            }));
            result.truncate(500);
            if result.len() >= 500 {
                break;
            }
            cursor = response.next_cursor;
            if let Some(c) = &cursor {
                if !seen.insert(c.clone()) {
                    return Err(error("session/list repeated cursor"));
                }
            } else {
                break;
            }
        }
        Ok(result)
    }
}
