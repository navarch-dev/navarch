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
type SessionReply = tokio::sync::oneshot::Sender<(Vec<WorkerSession>, bool)>;

pub(crate) struct State {
    pub online: HashMap<MachineId, Connection>,
    pub links: HashMap<MachineId, navarch_transport::ReplacementSender>,
    pub session_queries: HashMap<Uuid, (MachineId, SessionReply)>,
}
/// Shared hub state; workers interact only through protocol envelopes.
#[derive(Clone)]
pub struct Hub {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) endpoints: Arc<Mutex<Vec<String>>>,
    pub(crate) events: tokio::sync::broadcast::Sender<navarch_store::HubEvent>,
}
impl Hub {
    /// Open runtime state over a file or in-memory store.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            endpoints: Default::default(),
            events: tokio::sync::broadcast::channel(256).0,
            state: Arc::new(Mutex::new(State {
                online: HashMap::new(),
                links: HashMap::new(),
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
        let _state = self.state()?;
        let id = hello.machine_id()?;
        let mut machine = self
            .store
            .machines()?
            .into_iter()
            .find(|m| m.id == id)
            .unwrap_or(Machine {
                id: id.clone(),
                public_key: hello.public_key,
                static_public_key: None,
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
        let mut state = self.state()?;
        state
            .online
            .insert(machine.clone(), Connection { token, sender });
        self.lifecycle(&machine, "worker_online")?;
        Ok(token)
    }
    /// Mark offline only if this is still the current connection.
    pub fn detach(&self, machine: &MachineId, token: Uuid) -> Result<(), HubError> {
        let mut state = self.state()?;
        if state.online.get(machine).is_some_and(|c| c.token == token) {
            state.online.remove(machine);
            self.lifecycle(machine, "worker_offline")?;
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
        sender.validate(&body)?;
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
        // request holds the publication lock, so no worker event can precede this cursor.
        let seq = self.store.latest_event_seq()?;
        sender.send(body)?;
        Ok(json!({"card_id":card_id,"task_id":task_id,"seq":seq}))
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
