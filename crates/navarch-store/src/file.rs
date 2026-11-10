use crate::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
};

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredInbox {
    #[serde(flatten)]
    event: InboxEvent,
    #[serde(default)]
    read: bool,
}
type Locks = HashMap<PathBuf, Weak<Mutex<()>>>;
static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
/// Filesystem store rooted at `NAVARCH_HOME/hub`, with atomic same-directory renames.
#[derive(Clone)]
pub struct FileStore {
    root: PathBuf,
    lock: Arc<Mutex<()>>,
}
impl FileStore {
    /// Open a hub directory, creating it and its task/role directories as needed.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(root.as_ref()).map_err(StoreError::from)?;
        let root = root.as_ref().canonicalize().map_err(StoreError::from)?;
        let mut locks = LOCKS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(StoreError::from)?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks.get(&root).and_then(Weak::upgrade).unwrap_or_else(|| {
            let lock = Arc::new(Mutex::new(()));
            locks.insert(root.clone(), Arc::downgrade(&lock));
            lock
        });
        let store = Self { root, lock };
        fs::create_dir_all(store.root.join("tasks")).map_err(StoreError::from)?;
        fs::create_dir_all(store.root.join("roles")).map_err(StoreError::from)?;
        store.rebuild_index()?;
        Ok(store)
    }
    fn guard(&self) -> Result<MutexGuard<'_, ()>, StoreError> {
        self.lock.lock().map_err(StoreError::from)
    }
    fn read(&self, path: &str) -> Result<Option<String>, StoreError> {
        let full = self.root.join(path);
        match fs::read_to_string(&full) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Invalid(format!("{}: {e}", full.display()))),
        }
    }
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let full = self.root.join(path);
        let parent = full
            .parent()
            .ok_or_else(|| StoreError::Invalid("missing parent".into()))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(StoreError::from)?;
        temp.write_all(bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(StoreError::from)?;
        temp.persist(&full).map_err(StoreError::from)?;
        Ok(())
    }
    fn toml<T: DeserializeOwned + Default>(&self, path: &str) -> Result<T, StoreError> {
        self.read(path)?
            .map(|s| toml::from_str(&s).map_err(|e| StoreError::Invalid(format!("{path}: {e}"))))
            .unwrap_or_else(|| Ok(T::default()))
    }
    fn write_toml<T: Serialize>(&self, path: &str, value: &T) -> Result<(), StoreError> {
        self.write(
            path,
            toml::to_string_pretty(value)
                .map_err(StoreError::from)?
                .as_bytes(),
        )
    }
    fn scan(&self) -> Result<(Vec<TaskCard>, Vec<String>), StoreError> {
        let mut cards = vec![];
        let mut corrupt = vec![];
        for entry in fs::read_dir(self.root.join("tasks")).map_err(StoreError::from)? {
            let path = entry.map_err(StoreError::from)?.path();
            if path.extension().is_none_or(|e| e != "toml")
                || path.file_name().is_some_and(|n| n == "index.toml")
            {
                continue;
            }
            let result = fs::read_to_string(&path)
                .map_err(StoreError::from)
                .and_then(|text| toml::from_str::<TaskCard>(&text).map_err(StoreError::from))
                .and_then(|mut card| {
                    card.derive_state();
                    if path
                        .file_stem()
                        .is_some_and(|n| n == card.id.to_string().as_str())
                    {
                        Ok(card)
                    } else {
                        Err(StoreError::Invalid("task ID differs from file name".into()))
                    }
                });
            match result {
                Ok(card) => cards.push(card),
                Err(e) => corrupt.push(format!("{}: {e}", path.display())),
            }
        }
        cards.sort_by_key(|c| c.id);
        corrupt.sort();
        Ok((cards, corrupt))
    }
    fn rebuild(&self) -> Result<Index, StoreError> {
        let (cards, corrupt) = self.scan()?;
        let index = derive_index(&cards, corrupt);
        self.write_toml("tasks/index.toml", &index)?;
        Ok(index)
    }
    fn read_inbox(&self) -> Result<Vec<StoredInbox>, StoreError> {
        self.read("inbox.jsonl")?
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let value: serde_json::Value =
                    serde_json::from_str(line).map_err(StoreError::from)?;
                if value.get("seq").is_none() {
                    let envelope: navarch_proto::Envelope =
                        serde_json::from_value(value["envelope"].clone())
                            .map_err(StoreError::from)?;
                    let kind = value["envelope"]["body"]["type"]
                        .as_str()
                        .unwrap_or("failed");
                    let mut event = InboxEvent::worker(
                        envelope,
                        None,
                        kind,
                        "Legacy worker event; read the task card",
                        0,
                    );
                    event.event.seq = self.reserve_seq()?;
                    Ok(StoredInbox { event, read: false })
                } else {
                    serde_json::from_value(value).map_err(StoreError::from)
                }
            })
            .collect()
    }
    fn write_inbox(&self, events: &[StoredInbox]) -> Result<(), StoreError> {
        let mut bytes = Vec::new();
        for event in events {
            serde_json::to_writer(&mut bytes, event).map_err(StoreError::from)?;
            bytes.push(b'\n');
        }
        self.write("inbox.jsonl", &bytes)
    }
    fn reserve_seq(&self) -> Result<u64, StoreError> {
        let seq: u64 = self
            .read("event-seq")?
            .unwrap_or_else(|| "0".into())
            .trim()
            .parse()
            .map_err(StoreError::from)?;
        let seq = seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("event sequence exhausted".into()))?;
        // Reserve before writing an inbox row: a crash may leave a gap, never reuse.
        self.write("event-seq", seq.to_string().as_bytes())?;
        Ok(seq)
    }
}
impl Store for FileStore {
    fn machines(&self) -> Result<Vec<Machine>, StoreError> {
        let _guard = self.guard()?;
        serde_json::from_str(&self.read("machines.json")?.unwrap_or_else(|| "[]".into()))
            .map_err(StoreError::from)
    }
    fn put_machine(&self, machine: Machine) -> Result<(), StoreError> {
        let _guard = self.guard()?;
        let mut machines: Vec<Machine> =
            serde_json::from_str(&self.read("machines.json")?.unwrap_or_else(|| "[]".into()))
                .map_err(StoreError::from)?;
        machines.retain(|m| m.id != machine.id);
        machines.push(machine);
        machines.sort_by(|a, b| a.id.cmp(&b.id));
        self.write(
            "machines.json",
            &serde_json::to_vec_pretty(&machines).map_err(StoreError::from)?,
        )
    }
    fn projects(&self) -> Result<Projects, StoreError> {
        let _g = self.guard()?;
        self.toml("projects.toml")
    }
    fn put_projects(&self, value: Projects) -> Result<(), StoreError> {
        let _g = self.guard()?;
        self.write_toml("projects.toml", &value)
    }
    fn routes(&self) -> Result<Routes, StoreError> {
        let _g = self.guard()?;
        self.toml("routes.toml")
    }
    fn put_routes(&self, value: Routes) -> Result<(), StoreError> {
        let _g = self.guard()?;
        self.write_toml("routes.toml", &value)
    }
    fn role(&self, name: &str) -> Result<Option<Role>, StoreError> {
        role::validate(name)?;
        let _g = self.guard()?;
        self.read(&format!("roles/{name}.md"))?
            .map(|s| role::parse(&s))
            .transpose()
    }
    fn put_role(&self, name: &str, value: Role) -> Result<(), StoreError> {
        role::validate(name)?;
        let _g = self.guard()?;
        self.write(&format!("roles/{name}.md"), role::format(value)?.as_bytes())
    }
    fn task(&self, id: CardId) -> Result<Option<TaskCard>, StoreError> {
        let _g = self.guard()?;
        let Some(text) = self.read(&format!("tasks/{id}.toml"))? else {
            return Ok(None);
        };
        // Old M2 cards lack per-step turn IDs. Keep the file intact and report it
        // in scans, but ignore it on direct reads instead of crashing the daemon.
        let raw: toml::Value = toml::from_str(&text).map_err(StoreError::from)?;
        if raw
            .get("steps")
            .and_then(toml::Value::as_array)
            .is_some_and(|steps| steps.iter().any(|s| s.get("task_id").is_none()))
        {
            return Ok(None);
        }
        let mut card: TaskCard = toml::from_str(&text).map_err(StoreError::from)?;
        card.derive_state();
        Ok(Some(card))
    }
    fn put_task(&self, mut value: TaskCard) -> Result<(), StoreError> {
        let _g = self.guard()?;
        value.derive_state();
        self.write_toml(&format!("tasks/{}.toml", value.id), &value)?;
        self.rebuild()?;
        Ok(())
    }
    fn tasks(&self) -> Result<(Vec<TaskCard>, Vec<String>), StoreError> {
        let _g = self.guard()?;
        self.scan()
    }
    fn index(&self) -> Result<Index, StoreError> {
        let _g = self.guard()?;
        self.toml("tasks/index.toml")
    }
    fn rebuild_index(&self) -> Result<Index, StoreError> {
        let _g = self.guard()?;
        self.rebuild()
    }
    fn append_inbox(&self, mut event: InboxEvent) -> Result<Option<InboxEvent>, StoreError> {
        let _g = self.guard()?;
        let mut events = self.read_inbox()?;
        if events.iter().any(|e| e.event.id == event.id) {
            return Ok(None);
        }
        event.event.seq = self.reserve_seq()?;
        events.push(StoredInbox {
            event: event.clone(),
            read: false,
        });
        self.write_inbox(&events)?;
        Ok(Some(event))
    }
    fn next_event_seq(&self) -> Result<u64, StoreError> {
        let _g = self.guard()?;
        self.reserve_seq()
    }
    fn latest_event_seq(&self) -> Result<u64, StoreError> {
        let _g = self.guard()?;
        // Assign durable sequences to legacy rows before establishing a live baseline.
        let events = self.read_inbox()?;
        self.write_inbox(&events)?;
        self.read("event-seq")?
            .unwrap_or_else(|| "0".into())
            .trim()
            .parse()
            .map_err(StoreError::from)
    }
    fn inbox_after(&self, after_seq: u64, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError> {
        let _g = self.guard()?;
        let mut events = self.read_inbox()?;
        let unread = inbox_batch(
            events
                .iter()
                .filter(|e| !e.read && e.event.event.seq > after_seq)
                .map(|e| e.event.clone()),
        )?;
        if mark_read {
            for event in &mut events {
                if unread.iter().any(|e| e.id == event.event.id) {
                    event.read = true;
                }
            }
        }
        // Also persist sequences assigned when migrating legacy inbox rows.
        self.write_inbox(&events)?;
        Ok(unread)
    }
}
