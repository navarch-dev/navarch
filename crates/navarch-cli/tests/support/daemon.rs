//! Reusable real Unix daemon and LocalLink worker with a scripted offline driver.
#![allow(clippy::unwrap_used, dead_code)]
use navarch_acp::{FakeDriver, FakeStep};
use navarch_crypto::Identity;
use navarch_hub::control::{self, Request};
use navarch_policy::Policy;
use navarch_proto::{CardId, PermissionOption, PermissionOptionKind, PermissionRequest, ToolKind};
use navarch_store::{FileStore, Store};
use navarch_transport::{LocalConnector, LocalHello};
use navarch_worker::{
    Registry, TokioClock, Worker,
    config::{AgentConfig, Config},
};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub struct Task(pub tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub struct Daemon {
    pub home: tempfile::TempDir,
    pub hello: LocalHello,
    pub machine: String,
    pub hub: Task,
    pub worker_task: Task,
    pub worker: Worker,
    pub store: Arc<FileStore>,
}
fn spawn(home: PathBuf, hello: LocalHello, store: Arc<FileStore>) -> Task {
    Task(tokio::spawn(async move {
        navarch_hub::serve(&home, hello, store).await.unwrap();
    }))
}
impl Daemon {
    pub async fn new(scripts: Vec<Vec<FakeStep>>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(home.path()).unwrap();
        let machine = identity.machine_id();
        let hello = LocalHello {
            public_key: identity.public_key(),
            display_name: "test worker".into(),
        };
        let store = Arc::new(FileStore::open(home.path().join("hub")).unwrap());
        let hub = spawn(home.path().into(), hello.clone(), store.clone());
        let mut policy = Policy::parse("allowed_agents=['fake']\ndefault='escalate'").unwrap();
        policy.allowed_roots = vec![home.path().canonicalize().unwrap()];
        let (worker, streams) = Worker::with_factory(
            Config {
                local_hub_socket: None,
                direct: true,
                display_name: "fake".into(),
                agents: vec![AgentConfig {
                    name: "fake".into(),
                    command: std::env::current_exe().unwrap(),
                    args: vec![],
                    mode: None,
                }],
            },
            policy,
            Registry::load(home.path()).unwrap(),
            None,
            Arc::new(TokioClock),
            move |_, permissions| Arc::new(FakeDriver::new(scripts.clone(), permissions)),
        )
        .unwrap();
        let connector = LocalConnector {
            path: home.path().join("hub-worker.sock"),
            hello: hello.clone(),
        };
        let w = worker.clone();
        let id = machine.parse().unwrap();
        let worker_task = Task(tokio::spawn(async move {
            navarch_worker::serve(w, streams, id, Box::new(connector))
                .await
                .unwrap();
        }));
        let result = Self {
            home,
            hello,
            machine,
            hub,
            worker_task,
            worker,
            store,
        };
        result.ready().await;
        result
    }
    pub async fn ready(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if control::call(&self.socket(), &Request::ListWorkers)
                    .await
                    .is_ok_and(|v| {
                        v.as_array()
                            .unwrap()
                            .iter()
                            .any(|m| m["online"] == true && !m["machine"]["capabilities"].is_null())
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    pub fn socket(&self) -> PathBuf {
        self.home.path().join("hub.sock")
    }
    pub fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"));
        command
            .env("NAVARCH_HOME", self.home.path())
            .arg("hub")
            .args(args)
            .kill_on_drop(true);
        command
    }
    pub async fn cli(&self, args: &[&str], code: i32) -> Value {
        let output = self.command(args).output().await.unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    pub fn dispatch(&self, prompt: &str) -> Vec<String> {
        vec![
            "dispatch".into(),
            "--machine".into(),
            self.machine.clone(),
            "--agent".into(),
            "fake".into(),
            "--cwd".into(),
            self.home.path().to_string_lossy().into(),
            prompt.into(),
        ]
    }
    pub async fn dispatch_cli(&self, extras: &[&str], code: i32) -> Value {
        let mut args = self.dispatch("work");
        args.extend(extras.iter().map(|s| s.to_string()));
        self.cli(&args.iter().map(String::as_str).collect::<Vec<_>>(), code)
            .await
    }
    pub async fn card(&self, id: CardId, state: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .store
                    .task(id)
                    .unwrap()
                    .unwrap()
                    .steps
                    .last()
                    .unwrap()
                    .state
                    == state
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    pub async fn stop(&mut self) {
        self.hub.0.abort();
        let _ = (&mut self.hub.0).await;
    }
    pub fn restart(&mut self) {
        self.hub = spawn(
            self.home.path().into(),
            self.hello.clone(),
            self.store.clone(),
        );
    }
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
