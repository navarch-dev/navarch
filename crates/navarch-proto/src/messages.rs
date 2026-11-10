//! Versioned hub/worker messages and ACP-independent values.
use crate::{MachineId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// An opaque session identifier; worker and adapter namespaces are independent.
pub type SessionId = String;
/// A named MCP server, passed verbatim as a command and arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    /// Server name.
    pub name: String,
    /// Executable.
    pub command: PathBuf,
    /// Arguments, without shell interpretation.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables supplied to the server.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}
/// A permission category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Read files.
    Read,
    /// Modify files.
    Edit,
    /// Execute a command.
    Execute,
    /// Fetch a URL.
    Fetch,
    /// Invoke MCP.
    Mcp,
    /// Unknown operation.
    Other,
}
/// An offered permission choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Adapter-defined choice ID.
    pub id: String,
    /// Human-readable label.
    pub name: String,
    /// Allow or reject, once or persistently.
    pub kind: PermissionOptionKind,
}
/// Meaning of a permission option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    /// Allow once.
    AllowOnce,
    /// Allow persistently.
    AllowAlways,
    /// Deny once.
    RejectOnce,
    /// Deny persistently.
    RejectAlways,
}
/// A permission answer, validated against the offered options by the driver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Select an offered ID.
    Selected {
        /// Option identifier.
        option_id: String,
    },
    /// Cancel the request.
    Cancelled,
}
/// A normalized tool permission request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Session requesting permission.
    pub session_id: SessionId,
    /// Tool title.
    pub tool: String,
    /// Permission category; unknown categories stay `Other`.
    pub kind: ToolKind,
    /// Human-readable tool details.
    pub detail: String,
    /// Paths reported by the adapter.
    pub paths: Vec<PathBuf>,
    /// Command, only when supplied by the adapter.
    pub command: Option<String>,
    /// Choices offered by the adapter.
    pub options: Vec<PermissionOption>,
}
/// A configured adapter's capability report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Configured name.
    pub name: String,
    /// Whether the executable is present.
    pub present: bool,
    /// Version reported by initialize.
    pub version: Option<String>,
    /// Model IDs discovered through session configuration.
    pub models: Vec<String>,
    /// Probe failure, if any.
    pub error: Option<String>,
}
/// Worker capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Operating system.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// Configured adapters.
    pub agents: Vec<AgentCapability>,
    /// Repositories found up to depth two below allowed roots.
    pub repositories: Vec<PathBuf>,
    /// Active turns.
    pub running_turns: usize,
}
/// An addressed, versioned message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Wire protocol version.
    pub v: u16,
    /// Message ID for acknowledgements and deduplication.
    pub id: Uuid,
    /// Source machine.
    pub from: MachineId,
    /// Destination machine.
    pub to: MachineId,
    /// Message body.
    pub body: Body,
}
/// Unsupported wire version.
#[derive(Debug, thiserror::Error)]
#[error("unsupported protocol version {0}; expected 1")]
pub struct VersionError(pub u16);
impl Envelope {
    /// Reject unknown versions before processing the body.
    pub fn check_version(&self) -> Result<(), VersionError> {
        if self.v == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(VersionError(self.v))
        }
    }
}
/// All hub-to-worker and worker-to-hub messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    /// Start a session.
    Dispatch {
        /// Task identifier.
        task_id: TaskId,
        /// Configured adapter.
        agent: String,
        /// Working directory.
        cwd: PathBuf,
        /// Initial prompt.
        prompt: String,
        /// Optional mode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// Optional model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional MCP servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_servers: Option<Vec<McpServer>>,
    },
    /// Continue a session.
    SendToSession {
        /// Task identifier.
        task_id: TaskId,
        /// Session identifier.
        session_id: SessionId,
        /// Prompt text.
        prompt: String,
    },
    /// Cancel a task's active turn.
    Cancel {
        /// Task identifier.
        task_id: TaskId,
    },
    /// Answer an escalated request.
    PermissionAnswer {
        /// Request ID.
        request_id: Uuid,
        /// Offered choice or cancellation.
        decision: PermissionDecision,
    },
    /// Request capabilities.
    Probe,
    /// Acknowledge an envelope.
    Ack {
        /// Acknowledged envelope ID.
        of: Uuid,
    },
    /// Session exists and prompt is running or queued.
    Accepted {
        /// Task identifier.
        task_id: TaskId,
        /// Session identifier.
        session_id: SessionId,
    },
    /// Policy or capacity rejected a task.
    Rejected {
        /// Task identifier.
        task_id: TaskId,
        /// Refusal reason.
        reason: String,
    },
    /// Coarse progress.
    Progress {
        /// Task identifier.
        task_id: TaskId,
        /// Progress summary.
        summary: String,
    },
    /// Request human permission.
    PermissionAsk {
        /// Request ID.
        request_id: Uuid,
        /// Task identifier.
        task_id: TaskId,
        /// Tool title.
        tool: String,
        /// Tool details.
        detail: String,
        /// Offered choices.
        options: Vec<PermissionOption>,
    },
    /// Turn completed.
    Finished {
        /// Task identifier.
        task_id: TaskId,
        /// Session identifier.
        session_id: SessionId,
        /// Normalized stop reason.
        stop_reason: String,
        /// Final message, at most 64 KiB.
        result: String,
    },
    /// Turn failed.
    Failed {
        /// Task identifier.
        task_id: TaskId,
        /// Error description.
        error: String,
    },
    /// Capability report.
    ProbeReport {
        /// Worker capabilities.
        capabilities: Capabilities,
    },
}
