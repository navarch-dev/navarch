//! Exercise production Unix listener/control/reconnect wiring with FakeDriver.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_acp::{FakeDriver, FakeStep};
use navarch_crypto::Identity;
use navarch_hub::control::{self, Request};
use navarch_policy::Policy;
use navarch_store::{FileStore, Store};
use navarch_transport::{LocalConnector, LocalHello};
use navarch_worker::{
    Registry, TokioClock, Worker,
    config::{AgentConfig, Config},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn daemon(home: PathBuf, hello: LocalHello) -> Task {
    Task(tokio::spawn(async move {
        let store = Arc::new(FileStore::open(home.join("hub")).unwrap());
        navarch_hub::serve(&home, hello, store).await.unwrap();
    }))
}
async fn ready(socket: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if control::call(socket, &Request::ListWorkers)
                .await
                .is_ok_and(|v| {
                    v.as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["online"] == true && !m["machine"]["capabilities"].is_null())
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn finished(store: &dyn Store, id: navarch_proto::CardId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.task(id).unwrap().unwrap().state != "finished" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn local_worker_reconnects_and_hub_cli_reads_real_control_socket() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let identity = Identity::load_or_create(home).unwrap();
    // Hub and its local worker deliberately share one machine identity.
    let hello = LocalHello {
        public_key: identity.public_key(),
        display_name: "same machine".into(),
    };
    let mut hub = daemon(home.into(), hello.clone());
    let socket = home.join("hub.sock");
    let mut policy = Policy::parse("allowed_agents=['fake']\ndefault='deny'").unwrap();
    policy.allowed_roots = vec![home.canonicalize().unwrap()];
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
        Registry::load(home).unwrap(),
        None,
        Arc::new(TokioClock),
        |_, permissions| {
            Arc::new(FakeDriver::new(
                vec![
                    vec![FakeStep::Reply("one".into())],
                    vec![FakeStep::Reply("two".into())],
                ],
                permissions,
            ))
        },
    )
    .unwrap();
    let machine = identity.machine_id().parse().unwrap();
    let connector = LocalConnector {
        path: home.join("hub-worker.sock"),
        hello: hello.clone(),
    };
    let mut worker_task = Task(tokio::spawn(async move {
        navarch_worker::serve(worker, streams, machine, Box::new(connector))
            .await
            .unwrap();
    }));
    ready(&socket).await;
    let req:Request = serde_json::from_value(json!({"type":"dispatch","machine":identity.machine_id(),"agent":"fake","cwd":home,"prompt":"first"})).unwrap();
    let task =
        serde_json::from_value(control::call(&socket, &req).await.unwrap()["card_id"].clone())
            .unwrap();
    let store = FileStore::open(home.join("hub")).unwrap();
    finished(&store, task).await;
    let session = store.task(task).unwrap().unwrap().steps[0]
        .session
        .clone()
        .unwrap();
    hub.0.abort();
    let _ = (&mut hub.0).await;
    hub = daemon(home.into(), hello);
    ready(&socket).await;
    let task = serde_json::from_value(
        control::call(
            &socket,
            &Request::SendToSession {
                card_id: None,
                agent: None,
                machine: identity.machine_id().parse().unwrap(),
                session_id: session,
                prompt: "second".into(),
            },
        )
        .await
        .unwrap()["card_id"]
            .clone(),
    )
    .unwrap();
    finished(&store, task).await;
    assert_eq!(
        store
            .task(task)
            .unwrap()
            .unwrap()
            .steps
            .last()
            .unwrap()
            .result_summary,
        "two"
    );
    for (args, json, header) in [
        (vec!["hub", "status", "--json"], true, "ID"),
        (vec!["hub", "tasks", "--all", "--json"], true, "CARD"),
        (vec!["hub", "status"], false, "ID"),
        (vec!["hub", "tasks", "--all"], false, "CARD"),
    ] {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
            .env("NAVARCH_HOME", home)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        if json {
            let _: Value = serde_json::from_slice(&output.stdout).unwrap();
        } else {
            let table = String::from_utf8(output.stdout).unwrap();
            assert!(table.starts_with(header), "{table}");
            assert!(table.contains(if header == "CARD" {
                "finished"
            } else {
                "same machine"
            }));
        }
    }
    worker_task.0.abort();
    let _ = (&mut worker_task.0).await;
    hub.0.abort();
    let _ = (&mut hub.0).await;
}
