//! Atomic hub state storage. User-edited configuration and derived task indexes.
#![forbid(unsafe_code)]
mod file;
mod memory;
mod role;
mod types;
pub use file::FileStore;
pub use memory::MemoryStore;
use navarch_proto::CardId;
pub use types::*;

/// Storage or serialization failure, with file context when applicable.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem, serialization, or invalid record details.
    #[error("store: {0}")]
    Invalid(String),
}
impl StoreError {
    pub(crate) fn from(error: impl std::fmt::Display) -> Self {
        Self::Invalid(error.to_string())
    }
}
/// Hub persistence. Implementations serialize concurrent read/modify/write operations.
/// FileStore handles opened for the same canonical directory share a process lock.
pub trait Store: Send + Sync {
    /// Read known machines.
    fn machines(&self) -> Result<Vec<Machine>, StoreError>;
    /// Insert or replace one machine without losing concurrent machine writes.
    fn put_machine(&self, machine: Machine) -> Result<(), StoreError>;
    /// Read projects.
    fn projects(&self) -> Result<Projects, StoreError>;
    /// Replace projects atomically.
    fn put_projects(&self, projects: Projects) -> Result<(), StoreError>;
    /// Read routes.
    fn routes(&self) -> Result<Routes, StoreError>;
    /// Replace routes atomically.
    fn put_routes(&self, routes: Routes) -> Result<(), StoreError>;
    /// Read one named role.
    fn role(&self, name: &str) -> Result<Option<Role>, StoreError>;
    /// Write a named role atomically.
    fn put_role(&self, name: &str, role: Role) -> Result<(), StoreError>;
    /// Read an authoritative card.
    fn task(&self, id: CardId) -> Result<Option<TaskCard>, StoreError>;
    /// Write a card and update the active index while holding the writer lock.
    fn put_task(&self, card: TaskCard) -> Result<(), StoreError>;
    /// List every readable card and report corrupt ones.
    fn tasks(&self) -> Result<(Vec<TaskCard>, Vec<String>), StoreError>;
    /// Read the derived active index.
    fn index(&self) -> Result<Index, StoreError>;
    /// Rebuild the active index from authoritative task cards.
    fn rebuild_index(&self) -> Result<Index, StoreError>;
    /// Append an unread event idempotently by envelope ID.
    fn append_inbox(&self, event: InboxEvent) -> Result<Option<InboxEvent>, StoreError>;
    /// Reserve and persist a sequence for a bus-only lifecycle event.
    fn next_event_seq(&self) -> Result<u64, StoreError>;
    /// Latest reserved event sequence, including lifecycle events and read inbox rows.
    fn latest_event_seq(&self) -> Result<u64, StoreError>;
    /// Read unread events, optionally marking the returned batch read atomically.
    fn inbox(&self, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError> {
        self.inbox_after(0, mark_read)
    }
    /// Read a deliverable unread prefix newer than a cursor; marking affects only that batch.
    fn inbox_after(&self, after_seq: u64, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError>;
    /// Replay a bounded compact event prefix after a cursor, optionally including read rows.
    /// Does not change inbox read markers.
    fn events_after(&self, after_seq: u64, include_read: bool)
    -> Result<Vec<HubEvent>, StoreError>;
}
pub(crate) fn derive_index(cards: &[TaskCard], corrupt: Vec<String>) -> Index {
    Index {
        tasks: cards
            .iter()
            .filter(|c| c.is_active())
            .map(|c| TaskEntry {
                id: c.id,
                title: c.title.clone(),
                state: c.state.clone(),
            })
            .collect(),
        corrupt,
    }
}

// Leave room for the control response wrapper and JSON array separators.
pub(crate) fn inbox_batch(
    events: impl Iterator<Item = InboxEvent>,
) -> Result<Vec<InboxEvent>, StoreError> {
    let mut batch = Vec::new();
    let mut bytes = 128;
    for mut event in events {
        let mut size = serde_json::to_vec(&event).map_err(StoreError::from)?.len() + 1;
        if size > navarch_proto::MAX_ENVELOPE_BYTES - 1024 - 128 && event.envelope.is_some() {
            // A deliverable wire envelope can leave too little room for inbox
            // metadata. Project only this cloned read view; storage keeps the
            // original envelope and its opaque fields unchanged.
            event.envelope = None;
            event.envelope_omitted = true;
            size = serde_json::to_vec(&event).map_err(StoreError::from)?.len() + 1;
        }
        if bytes + size > navarch_proto::MAX_ENVELOPE_BYTES - 1024 {
            if batch.is_empty() {
                return Err(StoreError::Invalid(
                    "inbox entry exceeds control response limit".into(),
                ));
            }
            break;
        }
        bytes += size;
        batch.push(event);
    }
    Ok(batch)
}
