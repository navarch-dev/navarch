//! Atomic hub state storage. User-edited configuration and derived task indexes.
#![forbid(unsafe_code)]
mod file;
mod memory;
mod role;
mod types;
pub use file::FileStore;
pub use memory::MemoryStore;
use navarch_proto::TaskId;
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
    fn task(&self, id: TaskId) -> Result<Option<TaskCard>, StoreError>;
    /// Write a card and update the active index while holding the writer lock.
    fn put_task(&self, card: TaskCard) -> Result<(), StoreError>;
    /// List every readable card and report corrupt ones.
    fn tasks(&self) -> Result<(Vec<TaskCard>, Vec<String>), StoreError>;
    /// Read the derived active index.
    fn index(&self) -> Result<Index, StoreError>;
    /// Rebuild the active index from authoritative task cards.
    fn rebuild_index(&self) -> Result<Index, StoreError>;
    /// Append an unread event idempotently by envelope ID.
    fn append_inbox(&self, event: InboxEvent) -> Result<(), StoreError>;
    /// Read unread events, optionally marking the returned batch read atomically.
    fn inbox(&self, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError>;
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
