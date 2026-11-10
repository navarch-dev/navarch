use crate::HubError;
use navarch_proto::{Body, CardId, MachineId, WorkerSession};
use navarch_store::{Machine, MachineState, Step, Store, TaskCard};
use navarch_transport::{LocalHello, ReliableHandle};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use uuid::Uuid;

pub(crate) struct Connection {
    pub token: Uuid,
    pub sender: ReliableHandle,
}
pub(crate) struct State {
    pub online: HashMap<MachineId, Connection>,
    pub session_queries:
        HashMap<Uuid, (MachineId, tokio::sync::oneshot::Sender<Vec<WorkerSession>>)>,
}
/// Shared hub state; workers interact only through protocol envelopes.
#[derive(Clone)]
pub struct Hub {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) state: Arc<Mutex<State>>,
}
impl Hub {
    /// Open runtime state over a file or in-memory store.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            state: Arc::new(Mutex::new(State {
                online: HashMap::new(),
                session_queries: HashMap::new(),
            })),
        }
    }
    pub(crate) fn state(&self) -> Result<MutexGuard<'_, State>, HubError> {
        self.state
            .lock()
            .map_err(|e| HubError::Invalid(e.to_string()))
    }
    /// Record a local peer. First local connection is automatically active;
    /// an existing revoked machine remains revoked.
    pub fn register_local(&self, hello: &LocalHello) -> Result<MachineId, HubError> {
        let id = hello.machine_id()?;
        let mut machine = self
            .store
            .machines()?
            .into_iter()
            .find(|m| m.id == id)
            .unwrap_or(Machine {
                id: id.clone(),
                public_key: hello.public_key,
                display_name: hello.display_name.clone(),
                state: MachineState::Active,
                capabilities: None,
            });
        machine.display_name = hello.display_name.clone();
        self.store.put_machine(machine)?;
        Ok(id)
    }
    /// Attach a reliable connection, returning a token guarding later detach.
    pub fn attach(&self, machine: MachineId, sender: ReliableHandle) -> Result<Uuid, HubError> {
        let token = Uuid::new_v4();
        self.state()?
            .online
            .insert(machine, Connection { token, sender });
        Ok(token)
    }
    /// Mark offline only if this is still the current connection.
    pub fn detach(&self, machine: &MachineId, token: Uuid) -> Result<(), HubError> {
        let mut state = self.state()?;
        if state.online.get(machine).is_some_and(|c| c.token == token) {
            state.online.remove(machine);
        }
        Ok(())
    }
    pub(crate) fn machine(&self, id: &MachineId) -> Result<Machine, HubError> {
        self.store
            .machines()?
            .into_iter()
            .find(|m| &m.id == id)
            .ok_or_else(|| HubError::Invalid(format!("unknown machine {id}")))
    }
    pub(crate) fn sender(&self, state: &State, id: &MachineId) -> Result<ReliableHandle, HubError> {
        let machine = self.machine(id)?;
        if machine.state != MachineState::Active {
            return Err(HubError::Invalid(format!("machine {id} is not active")));
        }
        state
            .online
            .get(id)
            .map(|c| c.sender.clone())
            .ok_or_else(|| HubError::Invalid(format!("machine {id} is offline")))
    }
    pub(crate) fn card(&self, id: CardId) -> Result<TaskCard, HubError> {
        self.store
            .task(id)?
            .ok_or_else(|| HubError::Invalid(format!("unknown task {id}")))
    }
    pub(crate) fn create(
        &self,
        sender: ReliableHandle,
        step: Step,
        card_id: Option<CardId>,
        title: String,
        project: Option<String>,
        body: Body,
    ) -> Result<Value, HubError> {
        let task_id = match &body {
            Body::Dispatch { task_id, .. } | Body::SendToSession { task_id, .. } => *task_id,
            _ => return Err(HubError::Invalid("expected task command".into())),
        };
        let mut card = if let Some(id) = card_id {
            self.card(id)?
        } else {
            TaskCard {
                id: CardId::new(),
                title,
                project,
                state: String::new(),
                steps: vec![],
                log: vec![],
                applied_events: vec![],
                permissions: vec![],
            }
        };
        let card_id = card.id;
        card.steps.push(step);
        card.derive_state();
        self.store.put_task(card)?;
        sender.send(body)?;
        Ok(json!({"card_id":card_id,"task_id":task_id}))
    }
}

/// Submission timestamp used for ordering steps across cards.
pub(crate) fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}
