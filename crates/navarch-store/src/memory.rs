use crate::*;
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};
#[derive(Default)]
struct State {
    machines: BTreeMap<navarch_proto::MachineId, Machine>,
    projects: Projects,
    routes: Routes,
    roles: BTreeMap<String, Role>,
    tasks: BTreeMap<CardId, TaskCard>,
    inbox: Vec<InboxEvent>,
    index: Index,
    seq: u64,
    inbox_ids: std::collections::HashSet<uuid::Uuid>,
    read_inbox_ids: std::collections::HashSet<uuid::Uuid>,
}
/// Process-local store for tests, with the same locking and index semantics as files.
#[derive(Default)]
pub struct MemoryStore(Mutex<State>);
impl MemoryStore {
    fn state(&self) -> Result<MutexGuard<'_, State>, StoreError> {
        self.0.lock().map_err(StoreError::from)
    }
}
impl Store for MemoryStore {
    fn machines(&self) -> Result<Vec<Machine>, StoreError> {
        Ok(self.state()?.machines.values().cloned().collect())
    }
    fn put_machine(&self, value: Machine) -> Result<(), StoreError> {
        self.state()?.machines.insert(value.id.clone(), value);
        Ok(())
    }
    fn projects(&self) -> Result<Projects, StoreError> {
        Ok(self.state()?.projects.clone())
    }
    fn put_projects(&self, value: Projects) -> Result<(), StoreError> {
        self.state()?.projects = value;
        Ok(())
    }
    fn routes(&self) -> Result<Routes, StoreError> {
        Ok(self.state()?.routes.clone())
    }
    fn put_routes(&self, value: Routes) -> Result<(), StoreError> {
        self.state()?.routes = value;
        Ok(())
    }
    fn role(&self, name: &str) -> Result<Option<Role>, StoreError> {
        role::validate(name)?;
        Ok(self.state()?.roles.get(name).cloned())
    }
    fn put_role(&self, name: &str, value: Role) -> Result<(), StoreError> {
        role::validate(name)?;
        self.state()?.roles.insert(name.into(), value);
        Ok(())
    }
    fn task(&self, id: CardId) -> Result<Option<TaskCard>, StoreError> {
        Ok(self.state()?.tasks.get(&id).cloned())
    }
    fn put_task(&self, mut value: TaskCard) -> Result<(), StoreError> {
        let mut state = self.state()?;
        value.derive_state();
        state.tasks.insert(value.id, value);
        state.index = derive_index(&state.tasks.values().cloned().collect::<Vec<_>>(), vec![]);
        Ok(())
    }
    fn tasks(&self) -> Result<(Vec<TaskCard>, Vec<String>), StoreError> {
        Ok((self.state()?.tasks.values().cloned().collect(), vec![]))
    }
    fn index(&self) -> Result<Index, StoreError> {
        Ok(self.state()?.index.clone())
    }
    fn rebuild_index(&self) -> Result<Index, StoreError> {
        let mut state = self.state()?;
        state.index = derive_index(&state.tasks.values().cloned().collect::<Vec<_>>(), vec![]);
        Ok(state.index.clone())
    }
    fn append_inbox(&self, mut event: InboxEvent) -> Result<Option<InboxEvent>, StoreError> {
        let mut state = self.state()?;
        if state.inbox_ids.contains(&event.id) {
            return Ok(None);
        }
        state.seq = state
            .seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("event sequence exhausted".into()))?;
        event.event.seq = state.seq;
        state.inbox_ids.insert(event.id);
        state.inbox.push(event.clone());
        Ok(Some(event))
    }
    fn next_event_seq(&self) -> Result<u64, StoreError> {
        let mut state = self.state()?;
        state.seq = state
            .seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("event sequence exhausted".into()))?;
        Ok(state.seq)
    }
    fn latest_event_seq(&self) -> Result<u64, StoreError> {
        Ok(self.state()?.seq)
    }
    fn inbox_after(&self, after_seq: u64, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError> {
        let mut state = self.state()?;
        let batch = inbox_batch(
            state
                .inbox
                .iter()
                .filter(|e| e.event.seq > after_seq && !state.read_inbox_ids.contains(&e.id))
                .cloned(),
        )?;
        if mark_read {
            state.read_inbox_ids.extend(batch.iter().map(|e| e.id));
        }
        Ok(batch)
    }
    fn events_after(
        &self,
        after_seq: u64,
        include_read: bool,
    ) -> Result<Vec<HubEvent>, StoreError> {
        let state = self.state()?;
        let batch = inbox_batch(
            state
                .inbox
                .iter()
                .filter(|e| {
                    e.event.seq > after_seq
                        && (include_read || !state.read_inbox_ids.contains(&e.id))
                })
                .cloned(),
        )?;
        Ok(batch.into_iter().map(|entry| entry.event).collect())
    }
}
