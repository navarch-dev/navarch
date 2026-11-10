//! Complete offline M3 workflows over ephemeral loopback WebSockets.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_acp::{FakeDriver, FakeStep};
use navarch_crypto::{Identity, PairCode, PairInfo, PairRole, Pairing, StaticKey};
use navarch_hub::{Hub, RemoteRelay, control::Request};
use navarch_proto::*;
use navarch_store::{MachineState, MemoryStore, Store};
use navarch_transport::{
    Connector, Link, MAX_FRAME, RelayConnector, RelayListener, RelaySocket, open_pair_room,
    pair_room,
};
use navarch_worker::{
    Registry, TokioClock, Worker,
    config::{AgentConfig, Config},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
struct Tasks(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}
async fn relay(tasks: &mut Tasks) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/ws", l.local_addr().unwrap());
    tasks.0.push(tokio::spawn(async move {
        navarch_relay::serve(l).await.unwrap();
    }));
    url
}
fn info(i: &Identity, k: &StaticKey) -> PairInfo {
    PairInfo {
        identity: i.public_key(),
        static_key: k.public_key(),
        display_name: "test".into(),
    }
}
async fn wait(test: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn request(value: Value) -> Request {
    serde_json::from_value(value).unwrap()
}
async fn paired(url: &str, h: PairInfo, w: PairInfo) -> (PairInfo, PairInfo) {
    let code = PairCode::parse("4172-123456").unwrap();
    let mut hs = RelaySocket::connect(url, &Identity::generate())
        .await
        .unwrap();
    open_pair_room(&mut hs, code.room()).await.unwrap();
    let mut ws = RelaySocket::connect(url, &Identity::generate())
        .await
        .unwrap();
    let (h, w) = tokio::join!(
        pair_room(
            &mut hs,
            code.room(),
            PairRole::Hub,
            Pairing::new(PairRole::Hub, &code, h, 0).unwrap()
        ),
        pair_room(
            &mut ws,
            code.room(),
            PairRole::Worker,
            Pairing::new(PairRole::Worker, &code, w, 0).unwrap()
        )
    );
    (h.unwrap(), w.unwrap())
}
#[tokio::test]
async fn pair_approve_dispatch_send_cancel_disconnect_mid_turn_and_exactly_once_result() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hi = Arc::new(Identity::generate());
    let hk = Arc::new(StaticKey::generate());
    let wi = Arc::new(Identity::generate());
    let wk = Arc::new(StaticKey::generate());
    let (hpeer, wpeer) = paired(&url, info(&hi, &hk), info(&wi, &wk)).await;
    assert_eq!(hpeer, info(&wi, &wk));
    assert_eq!(wpeer, info(&hi, &hk));
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store.clone());
    let worker_id = hub.register_paired(hpeer).unwrap();
    assert_eq!(store.machines().unwrap()[0].state, MachineState::Pending);
    assert!(hub.register_paired(info(&wi, &wk)).is_err());
    let h = hub.clone();
    let local = hi.machine_id().parse().unwrap();
    let config = RemoteRelay {
        url: url.clone(),
        identity: hi.clone(),
        static_key: hk,
    };
    tasks.0.push(tokio::spawn(async move {
        h.serve_relay(local, config).await.unwrap();
    }));
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        display_name: "fake".into(),
        local_hub_socket: None,
        direct: true,
        agents: vec![AgentConfig {
            name: "fake".into(),
            command: std::env::current_exe().unwrap(),
            args: vec![],
            mode: None,
        }],
    };
    let mut policy =
        navarch_policy::Policy::parse("allowed_agents=['fake']\ndefault='escalate'").unwrap();
    policy.allowed_roots = vec![temp.path().canonicalize().unwrap()];
    let permissions = PermissionRequest {
        session_id: String::new(),
        tool: "Gate".into(),
        kind: ToolKind::Other,
        detail: "wait until test drops link".into(),
        paths: vec![],
        command: None,
        options: vec![PermissionOption {
            id: "yes".into(),
            name: "Continue".into(),
            kind: PermissionOptionKind::AllowOnce,
        }],
    };
    let scripts = vec![
        vec![FakeStep::Reply("first".into())],
        vec![FakeStep::Reply("followup".into())],
        vec![
            FakeStep::Tool("cancel gate".into()),
            FakeStep::Delay(Duration::from_secs(3600)),
        ],
        vec![
            FakeStep::Permission(permissions),
            FakeStep::Reply("survived disconnect".into()),
        ],
    ];
    let (worker, streams) = Worker::with_factory(
        config,
        policy,
        Registry::load(temp.path()).unwrap(),
        None,
        Arc::new(TokioClock),
        |_, p| Arc::new(FakeDriver::new(scripts.clone(), p)),
    )
    .unwrap();
    // Approval precedes all normal traffic; pending protection is exercised separately.
    hub.request(Request::ApproveMachine {
        machine: worker_id.clone(),
    })
    .unwrap();
    let connector = RelayConnector {
        url: url.clone(),
        identity: wi.clone(),
        static_key: wk,
        peer: hi.machine_id().parse().unwrap(),
        peer_static: wpeer.static_key,
    };
    let w = worker.clone();
    let id = worker_id.clone();
    tasks.0.push(tokio::spawn(async move {
        navarch_worker::serve(w, streams, id, Box::new(connector))
            .await
            .unwrap();
    }));
    wait(|| store.machines().unwrap()[0].capabilities.is_some()).await;
    let dispatch = || {
        hub.request(request(json!({"type":"dispatch","machine":worker_id,"agent":"fake","cwd":temp.path(),"prompt":"test"}))).unwrap()
    };
    let first = dispatch();
    let card_id: CardId = serde_json::from_value(first["card_id"].clone()).unwrap();
    wait(|| store.task(card_id).unwrap().unwrap().state == "finished").await;
    let card = store.task(card_id).unwrap().unwrap();
    assert_eq!(card.steps[0].result_summary, "first");
    let session = card.steps[0].session.clone().unwrap();
    let follow = hub
        .request(Request::SendToSession {
            machine: worker_id.clone(),
            session_id: session.clone(),
            prompt: "again".into(),
            card_id: None,
            agent: None,
        })
        .unwrap();
    let turn: TaskId = serde_json::from_value(follow["task_id"].clone()).unwrap();
    wait(|| {
        store
            .task(card_id)
            .unwrap()
            .unwrap()
            .steps
            .iter()
            .any(|s| s.task_id == turn && s.state == "finished")
    })
    .await;
    assert_eq!(
        store.task(card_id).unwrap().unwrap().steps[1].result_summary,
        "followup"
    );
    let follow = hub
        .request(Request::SendToSession {
            machine: worker_id.clone(),
            session_id: session,
            prompt: "cancel".into(),
            card_id: None,
            agent: None,
        })
        .unwrap();
    let turn: TaskId = serde_json::from_value(follow["task_id"].clone()).unwrap();
    wait(|| {
        store
            .task(card_id)
            .unwrap()
            .unwrap()
            .log
            .iter()
            .any(|e| e.step == turn && e.kind == "progress")
    })
    .await;
    hub.request(Request::Cancel { task_id: turn }).unwrap();
    wait(|| store.task(card_id).unwrap().unwrap().steps[2].state == "cancelled").await;
    let last = dispatch();
    let last_card: CardId = serde_json::from_value(last["card_id"].clone()).unwrap();
    wait(|| store.task(last_card).unwrap().unwrap().state == "waiting").await;
    let ask = store.task(last_card).unwrap().unwrap().permissions[0].request_id;
    // Replace the worker's authenticated socket to deterministically sever its active
    // Noise link. Keep this connection until the fake turn has produced its result.
    let replacement = RelaySocket::connect(&url, &wi).await.unwrap();
    worker.answer_permission(
        ask,
        PermissionDecision::Selected {
            option_id: "yes".into(),
        },
    );
    wait(|| worker.running_turns() == 0).await;
    drop(replacement);
    wait(|| store.task(last_card).unwrap().unwrap().state == "finished").await;
    let card = store.task(last_card).unwrap().unwrap();
    assert_eq!(card.steps[0].result_summary, "survived disconnect");
    let inbox = store.inbox(false).unwrap();
    let finished:Vec<_>=inbox.iter().filter(|e|matches!(e.envelope.as_ref().map(|e| &e.body),Some(Body::Finished{task_id,..}) if task_id.to_string()==last["task_id"].as_str().unwrap())).collect();
    assert_eq!(finished.len(), 1);
    let count = inbox.len();
    hub.apply(finished[0].envelope.as_ref().unwrap()).unwrap();
    assert_eq!(store.inbox(false).unwrap().len(), count);
    hub.request(Request::RevokeMachine {
        machine: worker_id.clone(),
    })
    .unwrap();
    assert!(
        hub.request(Request::ApproveMachine { machine: worker_id })
            .is_err()
    );
    assert!(hub.apply(finished[0].envelope.as_ref().unwrap()).is_err());
}
#[tokio::test]
async fn wrong_secret_closes_room_for_correct_code_too() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let good = PairCode::parse("4172-123456").unwrap();
    let wrong = PairCode::parse("4172-000000").unwrap();
    let mut h = RelaySocket::connect(&url, &Identity::generate())
        .await
        .unwrap();
    open_pair_room(&mut h, good.room()).await.unwrap();
    let mut w = RelaySocket::connect(&url, &Identity::generate())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        pair_room(
            &mut h,
            good.room(),
            PairRole::Hub,
            Pairing::new(
                PairRole::Hub,
                &good,
                info(&Identity::generate(), &StaticKey::generate()),
                0
            )
            .unwrap()
        ),
        pair_room(
            &mut w,
            wrong.room(),
            PairRole::Worker,
            Pairing::new(
                PairRole::Worker,
                &wrong,
                info(&Identity::generate(), &StaticKey::generate()),
                0
            )
            .unwrap()
        )
    );
    assert!(a.is_err());
    assert!(b.is_err());
    let mut late = RelaySocket::connect(&url, &Identity::generate())
        .await
        .unwrap();
    assert!(
        pair_room(
            &mut late,
            good.room(),
            PairRole::Worker,
            Pairing::new(
                PairRole::Worker,
                &good,
                info(&Identity::generate(), &StaticKey::generate()),
                0
            )
            .unwrap()
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn relay_link_maximum_envelopes_full_duplex_and_fresh_reconnect() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hi = Identity::generate();
    let hk = Arc::new(StaticKey::generate());
    let wi = Arc::new(Identity::generate());
    let wk = Arc::new(StaticKey::generate());
    let wid: MachineId = wi.machine_id().parse().unwrap();
    let expected = wid.clone();
    let public = wk.public_key();
    let mut listener = RelayListener::connect(
        &url,
        &hi,
        hk.clone(),
        Arc::new(move |id| if id == &expected { Some(public) } else { None }),
    )
    .await
    .unwrap();
    let mut connector = RelayConnector {
        url,
        identity: wi,
        static_key: wk,
        peer: hi.machine_id().parse().unwrap(),
        peer_static: hk.public_key(),
    };
    for _ in 0..2 {
        let (a, b) = tokio::join!(connector.connect(), listener.accept());
        let mut a = a.unwrap();
        let (id, mut b) = b.unwrap();
        assert_eq!(id, wid);
        let (sa, sb) = tokio::join!(
            a.send(vec![1; MAX_FRAME].into()),
            b.send(vec![2; MAX_FRAME].into())
        );
        sa.unwrap();
        sb.unwrap();
        let (ra, rb) = tokio::join!(a.recv(), b.recv());
        assert_eq!(ra.unwrap().as_ref(), vec![2; MAX_FRAME]);
        assert_eq!(rb.unwrap().as_ref(), vec![1; MAX_FRAME]);
        assert!(a.send(vec![0; MAX_FRAME + 1].into()).await.is_err());
    }
}
#[tokio::test]
async fn unknown_pending_revoked_sources_never_reach_noise_or_application() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hi = Identity::generate();
    let hk = Arc::new(StaticKey::generate());
    let wi = Identity::generate();
    let wk = StaticKey::generate();
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store.clone());
    let wid: MachineId = wi.machine_id().parse().unwrap();
    let state = hub.clone();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = seen.clone();
    let auth = Arc::new(move |id: &MachineId| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        store
            .machines()
            .unwrap()
            .into_iter()
            .find(|m| &m.id == id && m.state == MachineState::Active)
            .and_then(|m| m.static_public_key)
    });
    let mut listener = RelayListener::connect(&url, &hi, hk.clone(), auth)
        .await
        .unwrap();
    let mut raw = RelaySocket::connect(&url, &wi).await.unwrap();
    let hid: MachineId = hi.machine_id().parse().unwrap();
    let mut expected = 0;
    for stage in 0..3 {
        if stage == 1 {
            hub.register_paired(info(&wi, &wk)).unwrap();
        }
        if stage == 2 {
            hub.request(Request::RevokeMachine {
                machine: wid.clone(),
            })
            .unwrap();
        }
        let mut hs = navarch_crypto::NoiseHandshake::new(true, &wk, &hk.public_key()).unwrap();
        let handshake = [vec![0], hs.write().unwrap()].concat();
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        for bytes in [
            vec![3; 33],
            handshake,
            vec![2; 64],
            serde_json::to_vec(&json!({"type":"ack","of":"00000000-0000-0000-0000-000000000000"}))
                .unwrap(),
        ] {
            raw.send(navarch_proto::relay::RelayRequest::Route {
                to: hid.clone(),
                payload: B64.encode(bytes),
            })
            .await
            .unwrap();
            expected += 1;
            wait(|| seen.load(std::sync::atomic::Ordering::SeqCst) >= expected).await;
        }
        assert!(
            state
                .apply(&Envelope {
                    v: 1,
                    id: uuid::Uuid::new_v4(),
                    from: wid.clone(),
                    to: hid.clone(),
                    body: Body::ProbeReport {
                        capabilities: Capabilities::default()
                    }
                })
                .is_err()
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn pairing_cli_persists_confirmed_public_pins_through_daemon_and_wires_worker_connector() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hub_home = tempfile::tempdir_in("/tmp").unwrap();
    let worker_home = tempfile::tempdir_in("/tmp").unwrap();
    let hi = Identity::load_or_create(hub_home.path()).unwrap();
    let store = Arc::new(navarch_store::FileStore::open(hub_home.path().join("hub")).unwrap());
    let h = hub_home.path().to_owned();
    let s = store.clone();
    tasks.0.push(tokio::spawn(async move {
        navarch_hub::serve(
            &h,
            navarch_transport::LocalHello {
                public_key: hi.public_key(),
                display_name: "hub".into(),
            },
            s,
        )
        .await
        .unwrap();
    }));
    let socket = hub_home.path().join("hub.sock");
    wait(|| socket.exists()).await;
    let mut pair = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", hub_home.path())
        .args(["pair", "--relay", &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = pair.stdout.take().unwrap();
    let mut lines = BufReader::new(output).lines();
    let code = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(PairCode::parse(&code).is_ok());
    let join = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", worker_home.path())
        .args(["join", &code, "--relay", &url])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        join.status.success(),
        "{}",
        String::from_utf8_lossy(&join.stderr)
    );
    let steps = String::from_utf8_lossy(&join.stderr);
    for step in [
        "navarch worker init",
        "worker-policy.toml",
        "navarch worker serve",
    ] {
        assert!(steps.contains(step));
    }
    assert!(pair.wait().await.unwrap().success());
    let machines = store.machines().unwrap();
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].state, MachineState::Pending);
    assert!(!machines[0].display_name.is_empty());
    assert!(!machines[0].display_name.chars().any(char::is_control));
    assert!(machines[0].display_name.len() <= 200);
    assert_eq!(
        machines[0].display_name,
        rustix::system::uname().nodename().to_str().unwrap()
    );
    for json in [false, true] {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"));
        cmd.env("NAVARCH_HOME", hub_home.path())
            .args(["hub", "machines"]);
        if json {
            cmd.arg("--json");
        }
        let listing = cmd.output().await.unwrap();
        assert!(listing.status.success());
        let text = String::from_utf8(listing.stdout).unwrap();
        if json {
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                value[0]["machine"]["display_name"],
                machines[0].display_name
            );
        } else {
            assert!(text.starts_with("ID "));
            for field in [
                "STATE",
                "ONLINE",
                "NAME",
                "pending",
                &machines[0].display_name,
            ] {
                assert!(text.contains(field));
            }
        }
    }
    let worker_identity = Identity::load_or_create(worker_home.path()).unwrap();
    assert_eq!(machines[0].id.to_string(), worker_identity.machine_id());
    assert_eq!(
        machines[0].static_public_key,
        Some(
            StaticKey::load_or_create(worker_home.path())
                .unwrap()
                .public_key()
        )
    );
    let pinned: Value =
        serde_json::from_slice(&std::fs::read(worker_home.path().join("worker/hub.json")).unwrap())
            .unwrap();
    assert_eq!(pinned["relay"], url);
    assert_eq!(pinned["hub"]["identity"].as_str().unwrap().len(), 64);
    assert_eq!(pinned["hub"]["static_key"].as_str().unwrap().len(), 64);
    assert_eq!(
        navarch_cli::remote::HubConfig::load(hub_home.path())
            .unwrap()
            .relay
            .as_deref(),
        Some(url.as_str())
    );
    // A second join cannot replace pins, even before touching the network.
    let again = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", worker_home.path())
        .args(["join", &code, "--relay", &url])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already paired"));
    let mut connector = navarch_cli::remote::worker_connector(
        worker_home.path(),
        Arc::new(worker_identity),
        Some(worker_home.path().join("explicit.sock")),
        "test".into(),
    )
    .unwrap();
    assert!(connector.connect().await.is_err());
    let approve = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", hub_home.path())
        .args(["hub", "machines", "approve", &machines[0].id.to_string()])
        .output()
        .await
        .unwrap();
    assert!(approve.status.success());
    assert_eq!(
        String::from_utf8(approve.stdout).unwrap(),
        format!("Approved machine {}.\n", machines[0].id)
    );
    let identity = Identity::load_or_create(hub_home.path()).unwrap();
    let key = Arc::new(StaticKey::load_or_create(hub_home.path()).unwrap());
    let expected = machines[0].id.clone();
    let remote = machines[0].static_public_key.unwrap();
    let mut listener = RelayListener::connect(
        &url,
        &identity,
        key,
        Arc::new(move |id| if id == &expected { Some(remote) } else { None }),
    )
    .await
    .unwrap();
    let mut connector = navarch_cli::remote::worker_connector(
        worker_home.path(),
        Arc::new(Identity::load_or_create(worker_home.path()).unwrap()),
        None,
        "test".into(),
    )
    .unwrap();
    let (worker, hub) = tokio::join!(connector.connect(), listener.accept());
    let mut worker = worker.unwrap();
    let (_, mut hub) = hub.unwrap();
    worker
        .send(b"wired from saved pins".as_slice().into())
        .await
        .unwrap();
    assert_eq!(hub.recv().await.unwrap().as_ref(), b"wired from saved pins");
    let revoke = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", hub_home.path())
        .args(["hub", "machines", "revoke", &machines[0].id.to_string()])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(revoke.status.success());
    assert_eq!(
        String::from_utf8(revoke.stdout).unwrap(),
        format!("Revoked machine {}.\n", machines[0].id)
    );
    assert_eq!(store.machines().unwrap()[0].state, MachineState::Revoked);
}
