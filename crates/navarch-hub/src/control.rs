//! MCP-independent, line-delimited JSON control protocol.
use crate::HubError;
use navarch_proto::{CardId, MachineId, McpServer, PermissionDecision, TaskId};
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
    /// Query resumable worker and adapter sessions, with stale fallback.
    ListSessions {
        /// Optional machine filter.
        machine: Option<MachineId>,
        /// Optional adapter filter.
        agent: Option<String>,
        /// Maximum sessions per worker; defaults to 50, capped at 200.
        limit: Option<usize>,
        /// Optional worker-local directory filter.
        cwd: Option<PathBuf>,
    },
    /// Dispatch to an explicit machine and role or adapter.
    Dispatch {
        /// Append a turn to this card; otherwise create a new card.
        card_id: Option<CardId>,
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
        /// Override the session's latest owning card.
        card_id: Option<CardId>,
        /// Adapter name when session_id is an external adapter session ID.
        agent: Option<String>,
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
        /// Card identifier (the dispatch response’s card_id).
        #[serde(alias = "card_id")]
        task_id: CardId,
        /// Include compact event history; defaults to false.
        #[serde(default)]
        include_log: bool,
    },
    /// Read task cards, optionally including terminal states.
    ListTasks {
        /// Include finished and failed cards.
        #[serde(default)]
        include_finished: bool,
    },
    /// Return and mark unread worker events read.
    ReadInbox,
    /// Convert this connection to an event stream, replaying unread newer entries.
    Subscribe {
        /// Last delivered event sequence, absent to replay all unread events.
        after_seq: Option<u64>,
    },
    /// Subscribe only to future events; first line is {kind: "ready", seq: latest}.
    SubscribeLive,
    /// Wait for compact unread events (no envelopes), returning early when one arrives; never marks read.
    WaitForEvents {
        /// Default 300 seconds, capped at 1800.
        timeout_secs: Option<u64>,
        /// Only return entries newer than this cursor.
        after_seq: Option<u64>,
    },
    /// Answer an outstanding permission with an offered choice or cancellation.
    AnswerPermission {
        /// Permission request ID from the inbox.
        request_id: Uuid,
        /// Offered choice or cancellation.
        decision: PermissionDecision,
    },
    /// Save a locally completed pairing through the daemon state owner.
    RegisterPaired {
        /// Confirmed public introduction, from the local pairing CLI.
        peer: navarch_crypto::PairInfo,
    },
    /// Revoke an existing machine immediately.
    RevokeMachine {
        /// Machine identifier.
        machine: MachineId,
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
    navarch_transport::validate_socket_path(path)?;
    let stream = UnixStream::connect(path).await.map_err(|e| {
        HubError::Invalid(format!(
            "cannot connect to hub daemon at {}: {e}; start `navarch hub serve`",
            path.display()
        ))
    })?;
    let (read, mut write) = stream.into_split();
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    if bytes.len() > navarch_transport::MAX_FRAME {
        return Err(HubError::Invalid(
            "control request exceeds 1 MiB; shorten the prompt or payload".into(),
        ));
    }
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
            return Err(HubError::Invalid(
                "control line exceeds 1 MiB; shorten the prompt or payload".into(),
            ));
        }
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}

/// One live event hint, or a signal to re-read the authoritative inbox.
#[derive(Debug)]
pub enum StreamEvent {
    /// Bounded event fields, including a monotonic cursor.
    Event(crate::HubEvent),
    /// The subscriber fell behind; re-read the inbox and resubscribe.
    Lagged,
}
/// A dedicated streaming control connection.
pub struct Subscription {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}
impl Subscription {
    /// Read one JSON line; EOF means the daemon stopped or closed the subscription.
    pub async fn recv(&mut self) -> Result<Option<StreamEvent>, HubError> {
        let Some(line) = read_line(&mut self.reader).await? else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_slice(&line)?;
        if value["kind"] == "lagged" {
            return Ok(Some(StreamEvent::Lagged));
        }
        Ok(Some(StreamEvent::Event(serde_json::from_value(value)?)))
    }
}
/// Subscribe to a daemon's replay and live stream on a dedicated socket.
pub async fn subscribe(path: &Path, after_seq: Option<u64>) -> Result<Subscription, HubError> {
    navarch_transport::validate_socket_path(path)?;
    let stream = UnixStream::connect(path).await?;
    let (read, mut write) = stream.into_split();
    let mut bytes = serde_json::to_vec(&Request::Subscribe { after_seq })?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;
    Ok(Subscription {
        reader: BufReader::new(read),
    })
}

/// Atomically establish a live-only stream and learn its starting cursor.
/// Reconnect with `subscribe(path, Some(cursor))` after this succeeds.
pub async fn subscribe_live(path: &Path) -> Result<(u64, Subscription), HubError> {
    navarch_transport::validate_socket_path(path)?;
    let stream = UnixStream::connect(path).await?;
    let (read, mut write) = stream.into_split();
    write.write_all(b"{\"type\":\"subscribe_live\"}\n").await?;
    let mut reader = BufReader::new(read);
    let line = read_line(&mut reader)
        .await?
        .ok_or_else(|| HubError::Invalid("subscription closed before ready".into()))?;
    let ready: serde_json::Value = serde_json::from_slice(&line)?;
    let seq = ready["seq"]
        .as_u64()
        .filter(|_| ready["kind"] == "ready")
        .ok_or_else(|| HubError::Invalid("expected subscription ready cursor".into()))?;
    Ok((seq, Subscription { reader }))
}
