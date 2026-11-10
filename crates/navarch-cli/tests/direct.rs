//! M4 direct migration, reconnect and listener authorization end-to-end tests.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_acp::{FakeDriver, FakeStep};
use navarch_crypto::{Identity, PairInfo, StaticKey};
use navarch_hub::{DirectConfig, Hub, RemoteDirect, RemoteRelay, control::Request};
use navarch_proto::*;
use navarch_store::{MemoryStore, Store};
use navarch_transport::{
    Connector, DirectConnector, EndpointState, Link, PreferredConnector, RelayConnector,
    RelayListener,
};
use navarch_worker::{
    Registry, TokioClock, Worker,
    config::{AgentConfig, Config},
};
use serde_json::json;
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
    tasks.0.push(tokio::spawn(async move {
        navarch_relay::serve(listener).await.unwrap();
    }));
    url
}
async fn wait(mut test: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !test() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        tokio::task::yield_now().await;
    }
}
fn info(i: &Identity, k: &StaticKey) -> PairInfo {
    PairInfo {
        identity: i.public_key(),
        static_key: k.public_key(),
        display_name: "test".into(),
    }
}
fn request(value: serde_json::Value) -> Request {
    serde_json::from_value(value).unwrap()
}
fn connected(hub: &Hub) -> String {
    hub.request(Request::ListWorkers).unwrap()[0]["connection"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}
async fn direct(
    hub: &Hub,
    hi: &Identity,
    hk: Arc<StaticKey>,
    address: &str,
) -> navarch_transport::DirectListener {
    hub.direct_listener(
        &hi.machine_id().parse().unwrap(),
        &RemoteDirect {
            config: DirectConfig {
                listen: address.into(),
                advertise: Some(vec![address.into()]),
            },
            static_key: hk,
        },
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn relay_to_direct_mid_turn_finishes_once_events_order_and_direct_only_hub_restart() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hi = Arc::new(Identity::generate());
    let hk = Arc::new(StaticKey::generate());
    let wi = Arc::new(Identity::generate());
    let wk = Arc::new(StaticKey::generate());
    let local: MachineId = hi.machine_id().parse().unwrap();
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store.clone());
    let machine = hub.register_paired(info(&wi, &wk)).unwrap();
    hub.request(Request::ApproveMachine {
        machine: machine.clone(),
    })
    .unwrap();
    let (_, mut events) = hub.subscribe_events(None).unwrap();
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    // Initially advertise a refused endpoint so the turn starts on the relay.
    let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_address = refused.local_addr().unwrap().to_string();
    let listener = hub
        .direct_listener(
            &local,
            &RemoteDirect {
                config: DirectConfig {
                    listen: address.clone(),
                    advertise: Some(vec![old_address.clone()]),
                },
                static_key: hk.clone(),
            },
        )
        .await
        .unwrap();
    drop(refused);
    let h = hub.clone();
    let id = local.clone();
    tasks.0.push(tokio::spawn(async move {
        h.serve_direct_listener(id, listener).await.unwrap();
    }));
    let h = hub.clone();
    let id = local.clone();
    let config = RemoteRelay {
        url: url.clone(),
        identity: hi.clone(),
        static_key: hk.clone(),
    };
    tasks.0.push(tokio::spawn(async move {
        h.serve_relay(id, config).await.unwrap();
    }));
    let temp = tempfile::tempdir().unwrap();
    let permissions = PermissionRequest {
        session_id: String::new(),
        tool: "Gate".into(),
        kind: ToolKind::Other,
        detail: "hold turn through link migration".into(),
        paths: vec![],
        command: None,
        options: vec![PermissionOption {
            id: "yes".into(),
            name: "Continue".into(),
            kind: PermissionOptionKind::AllowOnce,
        }],
    };
    let scripts = vec![
        vec![
            FakeStep::Permission(permissions),
            FakeStep::Reply("survived migration".into()),
        ],
        vec![FakeStep::Reply("survived restart".into())],
    ];
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
    let (worker, streams) = Worker::with_factory(
        config,
        policy,
        Registry::load(temp.path()).unwrap(),
        None,
        Arc::new(TokioClock),
        |_, p| Arc::new(FakeDriver::new(scripts.clone(), p)),
    )
    .unwrap();
    let endpoint_path = temp.path().join("worker/endpoints.json");
    let endpoints = EndpointState::load(&endpoint_path).unwrap();
    let connector = PreferredConnector::new(
        RelayConnector {
            url,
            identity: wi,
            static_key: wk,
            peer: local.clone(),
            peer_static: hk.public_key(),
        },
        endpoints.clone(),
        true,
    );
    let id = machine.clone();
    let w = worker.clone();
    tasks.0.push(tokio::spawn(async move {
        navarch_worker::serve(w, streams, id, Box::new(connector))
            .await
            .unwrap();
    }));
    wait(|| {
        store.machines().unwrap()[0].capabilities.is_some()
            && !endpoints.addresses().unwrap().is_empty()
    })
    .await;
    assert_eq!(connected(&hub), "relay");
    assert_eq!(
        EndpointState::load(&endpoint_path)
            .unwrap()
            .addresses()
            .unwrap(),
        vec![old_address]
    );
    let dispatched=hub.request(request(json!({"type":"dispatch","machine":machine,"agent":"fake","cwd":temp.path(),"prompt":"test"}))).unwrap();
    let card: CardId = serde_json::from_value(dispatched["card_id"].clone()).unwrap();
    wait(|| store.task(card).unwrap().unwrap().state == "waiting").await;
    // Update the hub's next announcement as well as the live worker's endpoint state.
    drop(
        hub.direct_listener(
            &local,
            &RemoteDirect {
                config: DirectConfig {
                    listen: "127.0.0.1:0".into(),
                    advertise: Some(vec![address.clone()]),
                },
                static_key: hk.clone(),
            },
        )
        .await
        .unwrap(),
    );
    endpoints.update(vec![address.clone()]).unwrap();
    tokio::time::timeout(Duration::from_secs(3), wait(|| connected(&hub) == "direct"))
        .await
        .unwrap();
    let permission = store.task(card).unwrap().unwrap().permissions[0].request_id;
    hub.request(Request::AnswerPermission {
        request_id: permission,
        decision: PermissionDecision::Selected {
            option_id: "yes".into(),
        },
    })
    .unwrap();
    wait(|| store.task(card).unwrap().unwrap().state == "finished").await;
    assert_eq!(
        store.task(card).unwrap().unwrap().steps[0].result_summary,
        "survived migration"
    );
    let inbox = store.inbox(false).unwrap();
    assert_eq!(
        inbox
            .iter()
            .filter(|e| e.event.kind == "finished" && e.event.card_id == Some(card))
            .count(),
        1
    );
    let mut observed = vec![];
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }
    assert_eq!(observed[0].kind, "worker_online");
    assert_eq!(observed.last().unwrap().kind, "finished");
    assert!(observed.windows(2).all(|w| w[0].seq < w[1].seq));
    // Stop relay and both old hub accept loops; stored endpoints alone must reconnect.
    tasks.0[0].abort();
    tasks.0[1].abort();
    tasks.0[2].abort();
    wait(|| connected(&hub).is_empty()).await;
    let restarted = Hub::new(store.clone());
    let listener = direct(&restarted, &hi, hk, &address).await;
    let h = restarted.clone();
    tasks.0.push(tokio::spawn(async move {
        h.serve_direct_listener(local, listener).await.unwrap();
    }));
    wait(|| connected(&restarted) == "direct").await;
    let dispatched=restarted.request(request(json!({"type":"dispatch","machine":machine,"agent":"fake","cwd":temp.path(),"prompt":"test restart"}))).unwrap();
    let card: CardId = serde_json::from_value(dispatched["card_id"].clone()).unwrap();
    wait(|| store.task(card).unwrap().unwrap().state == "finished").await;
    assert_eq!(
        store.task(card).unwrap().unwrap().steps[0].result_summary,
        "survived restart"
    );
}
#[tokio::test]
async fn unknown_pending_revoked_direct_machines_are_never_admitted() {
    let hi = Identity::generate();
    let hk = Arc::new(StaticKey::generate());
    let wi = Identity::generate();
    let wk = Arc::new(StaticKey::generate());
    let store = Arc::new(MemoryStore::default());
    let hub = Hub::new(store);
    let listener = hub
        .direct_listener(
            &hi.machine_id().parse().unwrap(),
            &RemoteDirect {
                config: DirectConfig {
                    listen: "127.0.0.1:0".into(),
                    advertise: Some(vec![]),
                },
                static_key: hk.clone(),
            },
        )
        .await
        .unwrap();
    let address = listener.local_addr().to_string();
    let mut connector = DirectConnector {
        address,
        local: wi.machine_id().parse().unwrap(),
        static_key: wk.clone(),
        peer: hi.machine_id().parse().unwrap(),
        peer_static: hk.public_key(),
    };
    assert!(connector.connect().await.is_err());
    let machine = hub.register_paired(info(&wi, &wk)).unwrap();
    assert!(connector.connect().await.is_err());
    hub.request(Request::RevokeMachine { machine }).unwrap();
    assert!(connector.connect().await.is_err());
}
#[tokio::test]
async fn refused_and_blackhole_direct_addresses_fall_back_to_relay() {
    for blackhole in [false, true] {
        let mut tasks = Tasks(vec![]);
        let url = relay(&mut tasks).await;
        let hi = Identity::generate();
        let hk = Arc::new(StaticKey::generate());
        let wi = Arc::new(Identity::generate());
        let wk = Arc::new(StaticKey::generate());
        let remote = wk.public_key();
        let machine: MachineId = wi.machine_id().parse().unwrap();
        let expected = machine.clone();
        let mut listener = RelayListener::connect(
            &url,
            &hi,
            hk.clone(),
            Arc::new(move |id| if id == &expected { Some(remote) } else { None }),
        )
        .await
        .unwrap();
        let black = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = black.local_addr().unwrap().to_string();
        if blackhole {
            tasks.0.push(tokio::spawn(async move {
                let (_stream, _) = black.accept().await.unwrap();
                std::future::pending::<()>().await;
            }));
        } else {
            drop(black);
        }
        let state = EndpointState::default();
        state.update(vec![address]).unwrap();
        let mut connector = PreferredConnector::new(
            RelayConnector {
                url,
                identity: wi,
                static_key: wk,
                peer: hi.machine_id().parse().unwrap(),
                peer_static: hk.public_key(),
            },
            state,
            true,
        );
        let (link, accepted) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(connector.connect(), listener.accept())
        })
        .await
        .unwrap();
        assert_eq!(link.unwrap().kind(), "relay");
        assert_eq!(accepted.unwrap().0, machine);
    }
}

#[tokio::test]
async fn daemon_configures_advertisements_before_first_relay_connection() {
    let mut tasks = Tasks(vec![]);
    let url = relay(&mut tasks).await;
    let hi = Arc::new(Identity::generate());
    let hk = Arc::new(StaticKey::generate());
    let wi = Arc::new(Identity::generate());
    let wk = Arc::new(StaticKey::generate());
    let store = Arc::new(MemoryStore::default());
    let setup = Hub::new(store.clone());
    let machine = setup.register_paired(info(&wi, &wk)).unwrap();
    setup.request(Request::ApproveMachine { machine }).unwrap();
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap().to_string();
    drop(reserved);
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let path = home.path().to_owned();
    let key = hk.clone();
    let identity = hi.clone();
    let advertised = address.clone();
    let hub_url = url.clone();
    tasks.0.push(tokio::spawn(async move {
        navarch_hub::serve_with_links(
            &path,
            navarch_transport::LocalHello {
                public_key: identity.public_key(),
                display_name: "hub".into(),
            },
            store,
            Some(RemoteRelay {
                url: hub_url,
                identity,
                static_key: key.clone(),
            }),
            Some(RemoteDirect {
                config: DirectConfig {
                    listen: advertised.clone(),
                    advertise: Some(vec![advertised]),
                },
                static_key: key,
            }),
        )
        .await
        .unwrap();
    }));
    wait(|| home.path().join("hub.sock").exists()).await;
    // A connector may beat the hub's relay registration; retry failures as production does.
    let mut connector = RelayConnector {
        url,
        identity: wi,
        static_key: wk,
        peer: hi.machine_id().parse().unwrap(),
        peer_static: hk.public_key(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut link = loop {
        match connector.connect().await {
            Ok(link) => break link,
            Err(_) => {
                assert!(std::time::Instant::now() < deadline);
                tokio::task::yield_now().await;
            }
        }
    };
    let envelope: Envelope = serde_json::from_slice(&link.recv().await.unwrap()).unwrap();
    assert!(matches!(envelope.body,Body::HubEndpoints {direct} if direct==vec![address]));
}

#[tokio::test]
async fn new_authenticated_endpoints_upgrade_relay_immediately_including_first_announcement() {
    for previously_known in [false, true] {
        let mut tasks = Tasks(vec![]);
        let url = relay(&mut tasks).await;
        let hi = Identity::generate();
        let hk = Arc::new(StaticKey::generate());
        let wi = Arc::new(Identity::generate());
        let wk = Arc::new(StaticKey::generate());
        let hub_id: MachineId = hi.machine_id().parse().unwrap();
        let worker_id: MachineId = wi.machine_id().parse().unwrap();
        let public = wk.public_key();
        let expected = worker_id.clone();
        let auth: navarch_transport::RelayAuthorization =
            Arc::new(move |id| (id == &expected).then_some(public));
        let mut relay_listener = RelayListener::connect(&url, &hi, hk.clone(), auth.clone())
            .await
            .unwrap();
        let mut direct_listener = navarch_transport::DirectListener::bind(
            "127.0.0.1:0",
            hub_id.clone(),
            hk.clone(),
            auth,
        )
        .await
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let endpoint_path = temp.path().join("endpoints.json");
        let state = EndpointState::load(&endpoint_path).unwrap();
        if previously_known {
            let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            state
                .update(vec![refused.local_addr().unwrap().to_string()])
                .unwrap();
            drop(refused);
        }
        let connector = PreferredConnector::new(
            RelayConnector {
                url,
                identity: wi,
                static_key: wk,
                peer: hub_id.clone(),
                peer_static: hk.public_key(),
            },
            state.clone(),
            true,
        );
        let (_worker, mut incoming, _reliable) =
            navarch_transport::reliable(worker_id.clone(), Box::new(connector), 64);
        let (_, mut relay_link) = relay_listener.accept().await.unwrap();
        let address = direct_listener.local_addr().to_string();
        let announcement = Envelope {
            v: 1,
            id: uuid::Uuid::new_v4(),
            from: hub_id,
            to: worker_id.clone(),
            body: Body::HubEndpoints {
                direct: vec![address.clone()],
            },
        };
        relay_link
            .send(serde_json::to_vec(&announcement).unwrap().into())
            .await
            .unwrap();
        // Drive reliable delivery; the new address must trigger before the 300s timer.
        let upgrade = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                tokio::select! {
                    accepted = direct_listener.accept() => break accepted.unwrap(),
                    event = incoming.recv() => {
                        if let Some(navarch_transport::ReliableEvent::Message(delivery)) = event {
                            delivery.complete(true);
                        }
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(upgrade.0, worker_id);
        assert_eq!(
            EndpointState::load(&endpoint_path)
                .unwrap()
                .addresses()
                .unwrap(),
            vec![address]
        );
    }
}
