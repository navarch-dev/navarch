use async_trait::async_trait;
pub use navarch_proto::{McpServer, PermissionDecision, PermissionRequest, SessionId};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
/// Adapter executable and literal arguments.
#[derive(Clone, Debug)]
pub struct AdapterCommand {
    /// Executable name or path.
    pub command: PathBuf,
    /// Arguments passed without a shell.
    pub args: Vec<String>,
}
/// Parameters for a new session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewSession {
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// Requested permission mode.
    pub mode: Option<String>,
    /// Requested model ID.
    pub model: Option<String>,
    /// MCP servers exposed to the agent.
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
}
/// Parameters for restoring a session, including configuration after restart.
#[derive(Clone, Debug)]
pub struct ResumeSession {
    /// Existing adapter session ID.
    pub session_id: SessionId,
    /// Original session configuration.
    pub config: NewSession,
}
/// An established agent session.
#[derive(Clone, Debug)]
pub struct SessionHandle {
    /// Opaque session ID.
    pub session_id: SessionId,
    /// Available model IDs.
    pub models: Vec<String>,
    /// Adapter version reported by initialize.
    pub agent_version: Option<String>,
}
/// Listed adapter session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session ID.
    pub session_id: SessionId,
    /// Working directory.
    pub cwd: PathBuf,
    /// Optional display title.
    pub title: Option<String>,
    /// Adapter-provided update timestamp.
    #[serde(default)]
    pub updated_at: Option<String>,
}
/// Reduced agent update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    /// Text emitted by the agent.
    MessageChunk(String),
    /// A tool call or update.
    ToolCall(String),
    /// Plan update.
    Plan(String),
    /// Other update.
    Other(String),
}
/// Unbounded event channel; the consumer must drain it for streaming turns.
pub type EventSink = tokio::sync::mpsc::UnboundedSender<AgentEvent>;
/// Completed turn.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    /// Snake-case ACP stop reason.
    pub stop_reason: String,
    /// Agent text, truncated on a UTF-8 boundary to 64 KiB.
    pub result: String,
}
/// Driver failure without exposing SDK error types.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// Transport, adapter or protocol failure.
    #[error("adapter error: {0}")]
    Adapter(String),
    /// An operation or configuration is unsupported.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Session is unknown to this driver.
    #[error("unknown session {0}")]
    UnknownSession(String),
}
/// Permission callback supplied by the worker.
#[async_trait]
pub trait PermissionHandler: Send + Sync {
    /// Return an offered choice or cancel. The driver validates selected IDs.
    async fn request(&self, request: PermissionRequest) -> PermissionDecision;
}
/// Agent operations independent of ACP.
#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// Start a new session.
    async fn new_session(&self, req: NewSession) -> Result<SessionHandle, DriverError>;
    /// Restore a persistent session.
    async fn resume_session(&self, req: ResumeSession) -> Result<SessionHandle, DriverError>;
    /// Run a turn and stream updates.
    async fn prompt(
        &self,
        session: &SessionId,
        text: &str,
        events: EventSink,
    ) -> Result<TurnOutcome, DriverError>;
    /// Cancel a running turn.
    async fn cancel(&self, session: &SessionId) -> Result<(), DriverError>;
    /// List all pages of adapter sessions.
    async fn list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionInfo>, DriverError>;
}
/// Shared permission handler.
pub type SharedPermissionHandler = Arc<dyn PermissionHandler>;
/// Append text while preserving the wire result size bound.
pub(crate) fn append_bounded(target: &mut String, text: &str) {
    let mut end = text
        .len()
        .min((64 * 1024usize).saturating_sub(target.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}
