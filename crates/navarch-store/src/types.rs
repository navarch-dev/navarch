//! Persistent hub records, independent of worker and MCP representations.
use navarch_proto::{Capabilities, Envelope, MachineId, TaskId};
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
    pub public_key: [u8; 32],
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
/// One turn in a task, identified by session once accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Requested role, if any.
    pub role: Option<String>,
    /// Configured adapter name.
    pub agent: String,
    /// Target machine.
    pub machine: MachineId,
    /// Public worker session ID.
    pub session: Option<String>,
    /// Current turn state.
    pub state: String,
    /// Final or progress summary.
    pub result_summary: String,
}
/// Authoritative task card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCard {
    /// Task ID.
    pub id: TaskId,
    /// User-facing title.
    pub title: String,
    /// Optional project name.
    pub project: Option<String>,
    /// Current task state.
    pub state: String,
    /// Dispatches and subsequent turns.
    pub steps: Vec<Step>,
    /// Applied worker envelopes, also used for restart-safe deduplication.
    #[serde(default)]
    pub log: Vec<Envelope>,
}
impl TaskCard {
    /// Whether this task belongs in the active index.
    pub fn is_active(&self) -> bool {
        !matches!(
            self.state.as_str(),
            "finished" | "failed" | "rejected" | "cancelled"
        )
    }
}
/// Derived active-task index entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Task ID.
    pub id: TaskId,
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
/// Unread worker event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEvent {
    /// Original envelope, with a stable event ID.
    pub envelope: Envelope,
}
