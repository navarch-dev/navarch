//! Persistent hub records, independent of worker and MCP representations.
use navarch_proto::{Capabilities, CardId, Envelope, MachineId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Pairing approval state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    /// Awaiting human approval.
    Pending,
    /// May receive work.
    Active,
    /// Access withdrawn.
    Revoked,
}
/// Known machine and its last capability report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// Derived machine ID.
    pub id: MachineId,
    /// Ed25519 public key.
    #[serde(with = "navarch_proto::public_key")]
    pub public_key: [u8; 32],
    /// Paired X25519 static public key; absent on legacy/local records.
    #[serde(default, with = "navarch_proto::optional_public_key")]
    pub static_public_key: Option<[u8; 32]>,
    /// Display name.
    pub display_name: String,
    /// Approval state.
    pub state: MachineState,
    /// Last report, retained while offline.
    pub capabilities: Option<Capabilities>,
}
/// Project configuration, keyed by project name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projects {
    /// Projects and aliases.
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
}
/// One user-defined project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Alternative names.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Repository names.
    #[serde(default)]
    pub repositories: Vec<String>,
}
/// Task-kind to role mappings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routes {
    /// Kind and role name.
    #[serde(default)]
    pub routes: BTreeMap<String, String>,
}
/// A role's TOML front matter followed by Markdown preferences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    /// Configured adapter name.
    pub agent: String,
    /// Optional model override.
    pub model: Option<String>,
    /// Optional mode override.
    pub mode: Option<String>,
    /// Markdown guidance appended to the dispatched prompt.
    pub preferences: String,
}
/// One independently identified turn in an ordered card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Independent wire turn identifier.
    pub task_id: TaskId,
    /// Submission time in Unix nanoseconds, used to find a session's latest owner.
    pub created_at: u64,
    /// Requested role, if any.
    pub role: Option<String>,
    /// Configured adapter name.
    pub agent: String,
    /// Target machine.
    pub machine: MachineId,
    /// Public worker session ID.
    pub session: Option<String>,
    /// Adapter ID when known, scoped to the step's agent.
    #[serde(default)]
    pub adapter_session_id: Option<String>,
    /// Worker-local directory when known, retained for stale session listings.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// Current turn state.
    pub state: String,
    /// Final or progress summary.
    pub result_summary: String,
}
/// Authoritative task card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCard {
    /// Card ID.
    pub id: CardId,
    /// User-facing title.
    pub title: String,
    /// Optional project name.
    pub project: Option<String>,
    /// Current task state.
    pub state: String,
    /// Dispatches and subsequent turns.
    pub steps: Vec<Step>,
    /// Compact human-facing event history.
    #[serde(default)]
    pub log: Vec<CardLog>,
    /// Durable deduplication metadata, separate from the human-facing log.
    #[serde(default)]
    pub applied_events: Vec<uuid::Uuid>,
    /// Permission routing and validation metadata retained across hub restarts.
    #[serde(default)]
    pub permissions: Vec<CardPermission>,
}
impl TaskCard {
    /// Recompute state from steps in the design's precedence order.
    pub fn derive_state(&mut self) {
        self.state = if self
            .steps
            .iter()
            .any(|s| matches!(s.state.as_str(), "running" | "queued"))
        {
            "running"
        } else if self.steps.iter().any(|s| s.state == "waiting") {
            "waiting"
        } else if self.steps.last().is_some_and(|s| s.state == "failed") {
            "failed"
        } else {
            "finished"
        }
        .into();
    }
    /// Whether this card belongs in the active index.
    pub fn is_active(&self) -> bool {
        matches!(self.state.as_str(), "running" | "waiting")
    }
}
/// One compact card-log entry. Never contains a wire envelope or tool payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardLog {
    /// Unix milliseconds.
    pub time: u64,
    /// Event category.
    pub kind: String,
    /// Wire turn ID.
    pub step: TaskId,
    /// Human-readable text of at most 200 characters.
    pub summary: String,
}
/// Durable permission metadata; omitted from tool responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPermission {
    /// Permission request ID.
    pub request_id: uuid::Uuid,
    /// Owning turn ID.
    pub step: TaskId,
    /// Allowed answers.
    pub options: Vec<navarch_proto::PermissionOption>,
    /// Whether the hub already forwarded an answer.
    pub answered: bool,
}
/// Derived active-task index entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Card ID.
    pub id: CardId,
    /// Task title.
    pub title: String,
    /// Current state.
    pub state: String,
}
/// Index rebuild result; corrupt cards are reported without hiding good cards.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Active task entries.
    #[serde(default)]
    pub tasks: Vec<TaskEntry>,
    /// Paths and parsing errors for unreadable cards.
    #[serde(default)]
    pub corrupt: Vec<String>,
}
/// Bounded hint published by the hub; inbox and cards remain authoritative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubEvent {
    /// Durable monotonic sequence, including lifecycle events.
    pub seq: u64,
    /// Event category.
    pub kind: String,
    /// Owning card, absent for machine events.
    pub card_id: Option<CardId>,
    /// Wire turn, absent for machine events.
    pub step: Option<TaskId>,
    /// Machine that produced the event.
    pub machine: MachineId,
    /// At most 200 Unicode characters.
    pub summary: String,
    /// Unix milliseconds.
    pub time: u64,
}
/// Durable unread event, with the original envelope when produced by a worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEvent {
    /// Stable idempotency key.
    pub id: uuid::Uuid,
    /// Original worker event; machine events have no envelope.
    pub envelope: Option<Envelope>,
    /// The original envelope is retained in storage but omitted from this read
    /// view because its enclosing inbox entry would exceed the control limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub envelope_omitted: bool,
    /// Bounded public event fields, including its durable sequence.
    #[serde(flatten)]
    pub event: HubEvent,
}
impl InboxEvent {
    /// Construct a worker event before the store allocates its sequence.
    pub fn worker(
        envelope: Envelope,
        card_id: Option<CardId>,
        kind: &str,
        summary: &str,
        time: u64,
    ) -> Self {
        Self {
            id: envelope.id,
            event: HubEvent {
                seq: 0,
                kind: kind.into(),
                card_id,
                step: envelope.body.worker_task_id(),
                machine: envelope.from.clone(),
                summary: navarch_proto::short_summary(summary),
                time,
            },
            envelope: Some(envelope),
            envelope_omitted: false,
        }
    }
}
