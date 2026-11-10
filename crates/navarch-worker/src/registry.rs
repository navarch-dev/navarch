//! Worker-owned session persistence, independent of the hub store.
use crate::{WorkerError, error::file};
use navarch_acp::NewSession;
use navarch_proto::{SessionId, TaskId};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
/// Session scheduling state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// No active or queued turn.
    Idle,
    /// A turn is active.
    Running,
    /// Waiting for a global turn slot.
    Queued,
}
/// Persistent session metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Whether Navarch originally created this session (legacy records were created here).
    #[serde(default = "started_by_navarch")]
    pub navarch_started: bool,
    /// Most recently submitted task ID.
    pub task_id: TaskId,
    /// Configured adapter name.
    pub agent: String,
    /// Worker session ID, independent of adapter ID namespaces.
    pub session_id: SessionId,
    /// Opaque session ID used only with the configured adapter.
    pub adapter_session_id: SessionId,
    /// Original cwd and options, needed for process-independent resume.
    #[serde(flatten)]
    pub config: NewSession,
    /// Scheduling state.
    pub state: SessionState,
    /// Creation timestamp in Unix milliseconds.
    pub created_at: u128,
    /// Last state change in Unix milliseconds.
    pub updated_at: u128,
}
pub(crate) fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
/// Session registry written with same-directory temporary files and atomic rename.
pub struct Registry {
    path: PathBuf,
    pub(crate) records: BTreeMap<String, SessionRecord>,
}
impl Registry {
    /// Load missing state as empty; quarantine corrupt state without deleting it.
    /// Stale running/queued states become idle after process restart.
    pub fn load(home: &Path) -> Result<Self, WorkerError> {
        let path = home.join("worker/sessions.json");
        let mut records: BTreeMap<String, SessionRecord> = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<BTreeMap<String, SessionRecord>>(&bytes) {
                Ok(records)
                    if records
                        .iter()
                        .all(|(key, record)| key == &record.session_id) =>
                {
                    records
                }
                _ => {
                    let backup = path
                        .with_file_name(format!("sessions.corrupt-{}.json", uuid::Uuid::new_v4()));
                    fs::rename(&path, &backup).map_err(|e| file(&path, e))?;
                    tracing::warn!(path=%path.display(),backup=%backup.display(),"quarantined corrupt session registry");
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(file(&path, e)),
        };
        for record in records.values_mut() {
            record.state = SessionState::Idle;
        }
        Ok(Self { path, records })
    }
    /// Return session metadata in session-ID order.
    pub fn sessions(&self) -> Vec<SessionRecord> {
        self.records.values().cloned().collect()
    }
    /// Persist all records atomically.
    pub fn save(&self) -> Result<(), WorkerError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| file(&self.path, "missing parent"))?;
        fs::create_dir_all(parent).map_err(|e| file(parent, e))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| file(parent, e))?;
        serde_json::to_writer_pretty(&mut temp, &self.records).map_err(|e| file(&self.path, e))?;
        temp.flush()
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|e| file(&self.path, e))?;
        temp.persist(&self.path).map_err(|e| file(&self.path, e))?;
        Ok(())
    }
}

fn started_by_navarch() -> bool {
    true
}
