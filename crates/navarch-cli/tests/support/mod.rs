//! Hub and M1 Worker integration with FakeDriver, no network or real agents.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use navarch_acp::{FakeDriver, FakeStep};
use navarch_crypto::Identity;
use navarch_hub::{Hub, control::Request};
use navarch_policy::Policy;
use navarch_proto::*;
use navarch_store::{FileStore, MemoryStore, Store, TaskCard};
use navarch_transport::{Connector, Link, LinkError, LocalHello, memory_pair};
use navarch_worker::{
    Registry, TokioClock, Worker,
    config::{AgentConfig, Config},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

struct Queue(mpsc::UnboundedReceiver<Box<dyn Link>>);
#[async_trait]
impl Connector for Queue {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        self.0.recv().await.ok_or(LinkError::Closed)
    }
}
pub fn request(value: Value) -> Request {
    serde_json::from_value(value).unwrap()
}
pub fn permission() -> PermissionRequest {
    PermissionRequest {
        session_id: String::new(),
        tool: "Edit".into(),
        kind: ToolKind::Edit,
        detail: "edit a temporary test file".into(),
        paths: vec!["file".into()],
        command: None,
        options: vec![PermissionOption {
            id: "yes".into(),
            name: "Allow once".into(),
            kind: PermissionOptionKind::AllowOnce,
        }],
    }
}
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub store: Arc<dyn Store>,
    pub hub: Hub,
    pub worker: Worker,
    pub driver: Arc<FakeDriver>,
    local: MachineId,
    hello: LocalHello,
    reconnect: mpsc::UnboundedSender<Box<dyn Link>>,
    worker_task: tokio::task::JoinHandle<()>,
    pub hub_task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.worker_task.abort();
        self.hub_task.abort();
    }
}
impl Fixture {
    pub async fn new(scripts: Vec<Vec<FakeStep>>, files: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = if files {
            Arc::new(FileStore::open(temp.path().join("hub")).unwrap())
        } else {
            Arc::new(MemoryStore::default())
        };
        let hub = Hub::new(store.clone());
        let identity = Identity::generate();
        let local = Identity::generate()
            .machine_id()
            .parse::<MachineId>()
            .unwrap();
        let hello = LocalHello {
            public_key: identity.public_key(),
            display_name: "fake worker".into(),
        };
        let machine = hello.machine_id().unwrap();
        let config = Config {
            local_hub_socket: None,
            display_name: "fake".into(),
            agents: vec![AgentConfig {
                name: "fake".into(),
                command: std::env::current_exe().unwrap(),
                args: vec![],
                mode: None,
            }],
        };
        let mut policy = Policy::parse("allowed_agents=['fake']\ndefault='escalate'").unwrap();
        policy.allowed_roots = vec![temp.path().canonicalize().unwrap()];
        let driver = std::sync::OnceLock::new();
        let (worker, streams) = Worker::with_factory(
            config,
            policy,
            Registry::load(temp.path()).unwrap(),
            None,
            Arc::new(TokioClock),
            |_, permissions| {
                let fake = Arc::new(FakeDriver::new(scripts.clone(), permissions));
                driver.set(fake.clone()).ok();
                fake
            },
        )
        .unwrap();
        let (hub_link, worker_link) = memory_pair(local.clone(), machine.clone());
        let (ht, hr) = mpsc::unbounded_channel();
        let (wt, wr) = mpsc::unbounded_channel();
        ht.send(Box::new(hub_link) as Box<dyn Link>).ok().unwrap();
        wt.send(Box::new(worker_link) as Box<dyn Link>)
            .ok()
            .unwrap();
        let h = hub.clone();
        let l = local.clone();
        let he = hello.clone();
        let hub_task = tokio::spawn(async move {
            let _sender = ht;
            h.run_local_peer(l, he, Box::new(Queue(hr))).await.unwrap();
        });
        let w = worker.clone();
        let worker_task = tokio::spawn(async move {
            navarch_worker::serve(w, streams, machine, Box::new(Queue(wr)))
                .await
                .unwrap();
        });
        let f = Self {
            temp,
            store,
            hub,
            worker,
            driver: driver.get().unwrap().clone(),
            local,
            hello,
            reconnect: wt,
            worker_task,
            hub_task,
        };
        f.ready().await;
        f
    }
    pub fn machine(&self) -> MachineId {
        self.hello.machine_id().unwrap()
    }
    async fn ready(&self) {
        wait(|| {
            self.hub
                .request(Request::ListWorkers)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["online"] == true && !v["machine"]["capabilities"].is_null())
        })
        .await;
    }
    pub fn dispatch(&self) -> TaskId {
        self.task_id(self.hub.request(request(json!({"type":"dispatch","machine":self.machine(),"agent":"fake","cwd":self.temp.path(),"prompt":"do it"}))).unwrap())
    }
    pub fn follow(&self, session: &str) -> TaskId {
        self.task_id(
            self.hub
                .request(Request::SendToSession {
                    card_id: None,
                    agent: None,
                    machine: self.machine(),
                    session_id: session.into(),
                    prompt: "again".into(),
                })
                .unwrap(),
        )
    }
    pub fn task_id(&self, value: Value) -> TaskId {
        serde_json::from_value(value["task_id"].clone()).unwrap()
    }
    pub fn turn_card(&self, id: TaskId) -> TaskCard {
        self.store
            .tasks()
            .unwrap()
            .0
            .into_iter()
            .find(|c| c.steps.iter().any(|s| s.task_id == id))
            .unwrap()
    }
    pub async fn card(&self, id: TaskId, state: &str) -> TaskCard {
        wait(|| self.turn_card(id).state == state).await;
        self.turn_card(id)
    }
    pub async fn restart(&mut self) {
        if !self.hub_task.is_finished() {
            self.hub_task.abort();
            let _ = (&mut self.hub_task).await;
        }
        let store: Arc<dyn Store> =
            Arc::new(FileStore::open(self.temp.path().join("hub")).unwrap());
        self.store = store.clone();
        self.hub = Hub::new(store);
        let (hub_link, worker_link) = memory_pair(self.local.clone(), self.machine());
        let (ht, hr) = mpsc::unbounded_channel();
        ht.send(Box::new(hub_link) as Box<dyn Link>).ok().unwrap();
        self.reconnect
            .send(Box::new(worker_link) as Box<dyn Link>)
            .ok()
            .unwrap();
        let hub = self.hub.clone();
        let local = self.local.clone();
        let hello = self.hello.clone();
        self.hub_task = tokio::spawn(async move {
            let _sender = ht;
            hub.run_local_peer(local, hello, Box::new(Queue(hr)))
                .await
                .unwrap();
        });
        self.ready().await;
    }
}
pub async fn wait(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
