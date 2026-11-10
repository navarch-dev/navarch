//! File and memory stores share the same record and concurrency contracts.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_proto::*;
use navarch_store::*;
use std::sync::Arc;
fn card() -> TaskCard {
    TaskCard {
        id: TaskId::new(),
        title: "test".into(),
        project: Some("navarch".into()),
        state: "running".into(),
        steps: vec![Step {
            role: None,
            agent: "fake".into(),
            machine: "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
            session: Some("s".into()),
            state: "running".into(),
            result_summary: String::new(),
        }],
        log: vec![],
    }
}
fn round_trip(store: &dyn Store) {
    let machine = Machine {
        id: "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
        public_key: [0; 32],
        display_name: "local".into(),
        state: MachineState::Active,
        capabilities: Some(Capabilities::default()),
    };
    store.put_machine(machine.clone()).unwrap();
    assert_eq!(store.machines().unwrap(), vec![machine]);
    let projects = Projects {
        projects: [(
            "navarch".into(),
            Project {
                aliases: vec!["nav".into()],
                repositories: vec!["navarch".into()],
            },
        )]
        .into(),
    };
    store.put_projects(projects.clone()).unwrap();
    assert_eq!(store.projects().unwrap(), projects);
    let routes = Routes {
        routes: [("code".into(), "dev".into())].into(),
    };
    store.put_routes(routes.clone()).unwrap();
    assert_eq!(store.routes().unwrap(), routes);
    let role = Role {
        agent: "fake".into(),
        model: Some("m".into()),
        mode: None,
        preferences: "# Keep changes small\n".into(),
    };
    store.put_role("dev", role.clone()).unwrap();
    assert_eq!(store.role("dev").unwrap(), Some(role));
    assert!(
        store
            .put_role(
                "../escape",
                Role {
                    agent: "f".into(),
                    model: None,
                    mode: None,
                    preferences: String::new()
                }
            )
            .is_err()
    );
    assert!(store.role("missing").unwrap().is_none());
    let mut card = card();
    let event = Envelope {
        v: PROTOCOL_VERSION,
        id: uuid::Uuid::new_v4(),
        from: card.steps[0].machine.clone(),
        to: card.steps[0].machine.clone(),
        body: Body::Accepted {
            task_id: card.id,
            session_id: "s".into(),
        },
    };
    card.log.push(event.clone());
    store.put_task(card.clone()).unwrap();
    assert_eq!(store.task(card.id).unwrap(), Some(card.clone()));
    assert_eq!(store.index().unwrap().tasks[0].id, card.id);
    assert_eq!(store.rebuild_index().unwrap(), store.index().unwrap());
    let event = InboxEvent { envelope: event };
    store.append_inbox(event.clone()).unwrap();
    store.append_inbox(event.clone()).unwrap();
    assert_eq!(store.inbox(false).unwrap(), vec![event.clone()]);
    assert_eq!(store.inbox(true).unwrap(), vec![event]);
    assert!(store.inbox(true).unwrap().is_empty());
    card.state = "finished".into();
    store.put_task(card).unwrap();
    assert!(store.index().unwrap().tasks.is_empty());
}
#[test]
fn file_round_trips_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    round_trip(&FileStore::open(temp.path()).unwrap());
    let store = FileStore::open(temp.path()).unwrap();
    assert_eq!(store.tasks().unwrap().0.len(), 1);
    assert_eq!(store.role("dev").unwrap().unwrap().agent, "fake");
}
#[test]
fn memory_round_trips() {
    round_trip(&MemoryStore::default());
}
#[test]
fn corrupt_card_is_reported_and_index_is_rebuilt() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileStore::open(temp.path()).unwrap();
    let good = card();
    store.put_task(good.clone()).unwrap();
    std::fs::write(temp.path().join("tasks/index.toml"), "broken").unwrap();
    std::fs::write(temp.path().join("tasks/bad.toml"), "broken card").unwrap();
    let index = store.rebuild_index().unwrap();
    assert_eq!(index.tasks.len(), 1);
    assert_eq!(index.tasks[0].id, good.id);
    assert_eq!(index.corrupt.len(), 1);
    assert!(index.corrupt[0].contains("bad.toml"));
    assert_eq!(store.tasks().unwrap().1.len(), 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("tasks/bad.toml")).unwrap(),
        "broken card"
    );
}
fn concurrent(stores: Vec<Arc<dyn Store>>) {
    let count = stores.len();
    std::thread::scope(|s| {
        for (i, store) in stores.iter().enumerate() {
            s.spawn(move || {
                let mut c = card();
                c.title = format!("task {i}");
                store.put_task(c.clone()).unwrap();
                let public_key = [i as u8; 32];
                let id = data_id(i);
                store
                    .put_machine(Machine {
                        id: id.clone(),
                        public_key,
                        display_name: i.to_string(),
                        state: MachineState::Active,
                        capabilities: None,
                    })
                    .unwrap();
                store
                    .append_inbox(InboxEvent {
                        envelope: Envelope {
                            v: 1,
                            id: uuid::Uuid::new_v4(),
                            from: id.clone(),
                            to: id,
                            body: Body::Failed {
                                task_id: c.id,
                                error: "test".into(),
                            },
                        },
                    })
                    .unwrap();
            });
        }
    });
    assert_eq!(stores[0].tasks().unwrap().0.len(), count);
    assert_eq!(stores[0].index().unwrap().tasks.len(), count);
    assert_eq!(stores[0].machines().unwrap().len(), count);
    assert_eq!(stores[0].inbox(true).unwrap().len(), count);
}
fn data_id(i: usize) -> MachineId {
    let c = (b'a' + i as u8) as char;
    format!("{}a", c.to_string().repeat(25)).parse().unwrap()
}
#[test]
fn independent_file_handles_serialize_concurrent_writers() {
    let temp = tempfile::tempdir().unwrap();
    concurrent(
        (0..12)
            .map(|_| Arc::new(FileStore::open(temp.path()).unwrap()) as Arc<dyn Store>)
            .collect(),
    );
}
#[test]
fn memory_concurrent_writers() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    concurrent(vec![store; 12]);
}
