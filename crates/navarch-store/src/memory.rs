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
    tasks: BTreeMap<TaskId, TaskCard>,
    inbox: Vec<InboxEvent>,
    index: Index,
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
    fn task(&self, id: TaskId) -> Result<Option<TaskCard>, StoreError> {
        Ok(self.state()?.tasks.get(&id).cloned())
    }
    fn put_task(&self, value: TaskCard) -> Result<(), StoreError> {
        let mut state = self.state()?;
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
    fn append_inbox(&self, event: InboxEvent) -> Result<(), StoreError> {
        let mut state = self.state()?;
        if !state
            .inbox
            .iter()
            .any(|e| e.envelope.id == event.envelope.id)
        {
            state.inbox.push(event);
        }
        Ok(())
    }
    fn inbox(&self, mark_read: bool) -> Result<Vec<InboxEvent>, StoreError> {
        let mut state = self.state()?;
        Ok(if mark_read {
            std::mem::take(&mut state.inbox)
        } else {
            state.inbox.clone()
        })
    }
}
