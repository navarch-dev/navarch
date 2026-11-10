//! MCP-independent, line-delimited JSON control protocol.
use crate::HubError;
use navarch_proto::{MachineId, McpServer, PermissionDecision, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use uuid::Uuid;
/// Exactly one request per orchestrator tool. Unknown fields remain forward compatible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Known workers, last capabilities and current online state.
    ListWorkers,
    /// Sessions observed by this hub, optionally filtered by machine.
    ListSessions {
        /// Optional machine filter.
        machine: Option<MachineId>,
    },
    /// Dispatch to an explicit machine and role or adapter.
    Dispatch {
        /// Target machine.
        machine: MachineId,
        /// Explicit adapter, mutually exclusive with role.
        agent: Option<String>,
        /// Named role whose settings supply the adapter and prompt preferences.
        role: Option<String>,
        /// Worker-local cwd; worker policy remains authoritative.
        cwd: PathBuf,
        /// Work to perform.
        prompt: String,
        /// Optional human-readable title.
        title: Option<String>,
        /// Optional project name or alias.
        project: Option<String>,
        /// Optional mode override.
        mode: Option<String>,
        /// Optional model override.
        model: Option<String>,
        /// Optional command-based servers for the worker session.
        mcp_servers: Option<Vec<McpServer>>,
    },
    /// Add a turn to a known session, returning a new task ID for that turn.
    SendToSession {
        /// Target machine.
        machine: MachineId,
        /// Worker session ID.
        session_id: String,
        /// Follow-up prompt.
        prompt: String,
    },
    /// Cancel the turn for a task.
    Cancel {
        /// Task to cancel.
        task_id: TaskId,
    },
    /// Read one task card.
    GetTask {
        /// Task identifier.
        task_id: TaskId,
    },
    /// Read task cards, optionally including terminal states.
    ListTasks {
        /// Include completed, failed and rejected cards.
        #[serde(default)]
        include_finished: bool,
    },
    /// Return and mark unread worker events read.
    ReadInbox,
    /// Answer an outstanding permission with an offered choice or cancellation.
    AnswerPermission {
        /// Permission request ID from the inbox.
        request_id: Uuid,
        /// Offered choice or cancellation.
        decision: PermissionDecision,
    },
    /// Approve a known pending machine.
    ApproveMachine {
        /// Machine identifier.
        machine: MachineId,
    },
}
/// One response for each request, on its own JSON line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// Successful tool result.
    Ok {
        /// Tool-specific JSON result, using only Navarch records.
        value: serde_json::Value,
    },
    /// Validation or daemon failure readable by the orchestrator.
    Error {
        /// Actionable error text.
        message: String,
    },
}
/// Send one control request to a daemon. No state is retained by this client.
pub async fn call(path: &Path, request: &Request) -> Result<serde_json::Value, HubError> {
    let stream = UnixStream::connect(path).await.map_err(|e| {
        HubError::Invalid(format!(
            "cannot connect to hub daemon at {}: {e}; start `navarch hub serve`",
            path.display()
        ))
    })?;
    let (read, mut write) = stream.into_split();
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;
    let mut reader = BufReader::new(read);
    let line = read_line(&mut reader)
        .await?
        .ok_or_else(|| HubError::Invalid("hub closed before responding".into()))?;
    match serde_json::from_slice(&line)? {
        Response::Ok { value } => Ok(value),
        Response::Error { message } => Err(HubError::Invalid(message)),
    }
}
// Bound each line while reading, rather than allocating an unbounded String.
pub(crate) async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, HubError> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(HubError::Invalid("incomplete control JSON line".into()))
            };
        }
        let end = buffer.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let count = end.unwrap_or(buffer.len());
        if line.len() + count > navarch_transport::MAX_FRAME {
            return Err(HubError::Invalid("control line exceeds 1 MiB".into()));
        }
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}
