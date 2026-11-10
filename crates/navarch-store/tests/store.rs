//! File and memory stores share the same record and concurrency contracts.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_proto::*;
use navarch_store::*;
use std::sync::Arc;
fn card() -> TaskCard {
    TaskCard {
        id: CardId::new(),
        title: "test".into(),
        project: Some("navarch".into()),
        state: "running".into(),
        applied_events: vec![],
        permissions: vec![],
        steps: vec![Step {
            task_id: TaskId::new(),
            created_at: 1,
            role: None,
            agent: "fake".into(),
            machine: "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
            session: Some("s".into()),
            adapter_session_id: None,
            cwd: None,
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
        static_public_key: None,
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
            task_id: card.steps[0].task_id,
            session_id: "s".into(),
        },
    };
    card.log.push(CardLog {
        time: 0,
        kind: "accepted".into(),
        step: card.steps[0].task_id,
        summary: "Turn accepted".into(),
    });
    store.put_task(card.clone()).unwrap();
    assert_eq!(store.task(card.id).unwrap(), Some(card.clone()));
    assert_eq!(store.index().unwrap().tasks[0].id, card.id);
    assert_eq!(store.rebuild_index().unwrap(), store.index().unwrap());
    let event = InboxEvent::worker(event, None, "failed", "test", 0);
    let event = store.append_inbox(event).unwrap().unwrap();
    assert!(store.append_inbox(event.clone()).unwrap().is_none());
    assert_eq!(store.inbox(false).unwrap(), vec![event.clone()]);
    assert_eq!(store.inbox(true).unwrap(), vec![event]);
    assert!(store.inbox(true).unwrap().is_empty());
    card.steps[0].state = "finished".into();
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
                        static_public_key: None,
                        display_name: i.to_string(),
                        state: MachineState::Active,
                        capabilities: None,
                    })
                    .unwrap();
                store
                    .append_inbox(InboxEvent::worker(
                        Envelope {
                            v: 1,
                            id: uuid::Uuid::new_v4(),
                            from: id.clone(),
                            to: id,
                            body: Body::Failed {
                                task_id: c.steps[0].task_id,
                                error: "test".into(),
                            },
                        },
                        None,
                        "failed",
                        "test",
                        0,
                    ))
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

#[test]
fn card_state_is_derived_with_running_waiting_and_latest_terminal_precedence() {
    let mut c = card();
    let mut next = c.steps[0].clone();
    next.task_id = TaskId::new();
    next.state = "failed".into();
    c.steps.push(next);
    c.derive_state();
    assert_eq!(c.state, "running");
    c.steps[0].state = "waiting".into();
    c.derive_state();
    assert_eq!(c.state, "waiting");
    c.steps[0].state = "finished".into();
    c.derive_state();
    assert_eq!(c.state, "failed");
    c.steps[1].state = "cancelled".into();
    c.derive_state();
    assert_eq!(c.state, "cancelled");
    let store = MemoryStore::default();
    c.state = "wrong".into();
    store.put_task(c.clone()).unwrap();
    assert_eq!(store.task(c.id).unwrap().unwrap().state, "cancelled");
}
#[test]
fn legacy_cards_are_preserved_and_ignored_without_hiding_current_cards() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileStore::open(temp.path()).unwrap();
    let current = card();
    store.put_task(current.clone()).unwrap();
    let old_card = card();
    let id = old_card.id;
    let mut legacy: toml::Value = toml::from_str(&toml::to_string(&old_card).unwrap()).unwrap();
    legacy["steps"][0].as_table_mut().unwrap().remove("task_id");
    legacy["log"] = toml::Value::try_from(vec![Envelope {
        v: 1,
        id: uuid::Uuid::new_v4(),
        from: data_id(0),
        to: data_id(0),
        body: Body::Probe,
    }])
    .unwrap();
    let text = toml::to_string(&legacy).unwrap();
    let path = temp.path().join(format!("tasks/{id}.toml"));
    std::fs::write(&path, &text).unwrap();
    let reopened = FileStore::open(temp.path()).unwrap();
    assert_eq!(reopened.tasks().unwrap().0, vec![current]);
    assert_eq!(reopened.index().unwrap().corrupt.len(), 1);
    assert!(reopened.task(id).unwrap().is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}
#[test]
fn public_keys_write_hex_and_read_legacy_arrays() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileStore::open(temp.path()).unwrap();
    let machine = Machine {
        id: data_id(0),
        public_key: [171; 32],
        static_public_key: None,
        display_name: "test".into(),
        state: MachineState::Active,
        capabilities: None,
    };
    let mut legacy = serde_json::to_value(&machine).unwrap();
    assert_eq!(legacy["public_key"], "ab".repeat(32));
    legacy["public_key"] = serde_json::json!(vec![171; 32]);
    std::fs::write(
        temp.path().join("machines.json"),
        serde_json::to_vec(&vec![legacy]).unwrap(),
    )
    .unwrap();
    assert_eq!(store.machines().unwrap(), vec![machine.clone()]);
    store.put_machine(machine).unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("machines.json")).unwrap()).unwrap();
    assert_eq!(json[0]["public_key"], "ab".repeat(32));
    for key in [
        serde_json::json!("ff"),
        serde_json::json!("zz".repeat(32)),
        serde_json::json!(vec![0; 31]),
    ] {
        let mut invalid = json[0].clone();
        invalid["public_key"] = key;
        assert!(serde_json::from_value::<Machine>(invalid).is_err());
    }
}

#[test]
fn event_sequences_survive_reads_reopen_and_legacy_migration() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileStore::open(temp.path()).unwrap();
    let envelope = navarch_proto::Envelope {
        v: 1,
        id: uuid::Uuid::new_v4(),
        from: data_id(0),
        to: data_id(1),
        body: navarch_proto::Body::Failed {
            task_id: navarch_proto::TaskId::new(),
            error: "legacy".into(),
        },
    };
    std::fs::write(
        temp.path().join("inbox.jsonl"),
        format!("{}\n", serde_json::json!({"envelope":envelope})),
    )
    .unwrap();
    assert_eq!(store.latest_event_seq().unwrap(), 1);
    let legacy = store.inbox(true).unwrap();
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].event.seq, 1);
    assert_eq!(legacy[0].envelope.as_ref(), Some(&envelope));
    assert!(store.append_inbox(legacy[0].clone()).unwrap().is_none());
    assert_eq!(store.next_event_seq().unwrap(), 2);
    drop(store);
    let store = FileStore::open(temp.path()).unwrap();
    assert!(store.inbox(false).unwrap().is_empty());
    assert_eq!(
        store.events_after(0, true).unwrap(),
        vec![legacy[0].event.clone()]
    );
    assert!(store.events_after(0, false).unwrap().is_empty());
    assert!(store.events_after(1, true).unwrap().is_empty());
    let event = store
        .append_inbox(InboxEvent::worker(
            navarch_proto::Envelope {
                id: uuid::Uuid::new_v4(),
                ..envelope
            },
            None,
            "failed",
            "new",
            0,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(event.event.seq, 3);
}

#[test]
fn event_replay_includes_read_rows_without_changing_unread_batches() {
    let temp = tempfile::tempdir().unwrap();
    for store in [
        &MemoryStore::default() as &dyn Store,
        &FileStore::open(temp.path()).unwrap() as &dyn Store,
    ] {
        let mut events = Vec::new();
        for _ in 0..3 {
            events.push(
                store
                    .append_inbox(InboxEvent::worker(
                        Envelope {
                            v: 1,
                            id: uuid::Uuid::new_v4(),
                            from: data_id(0),
                            to: data_id(1),
                            body: Body::Failed {
                                task_id: TaskId::new(),
                                error: "failed".into(),
                            },
                        },
                        None,
                        "failed",
                        "failed",
                        0,
                    ))
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(
            store.inbox_after(events[0].event.seq, true).unwrap(),
            events[1..]
        );
        assert_eq!(
            store.events_after(events[0].event.seq, true).unwrap(),
            events[1..]
                .iter()
                .map(|e| e.event.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            store
                .events_after(events[0].event.seq, false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.inbox(false).unwrap(), events[..1]);
        assert_eq!(store.events_after(0, true).unwrap().len(), 3);
        assert_eq!(store.inbox(true).unwrap(), events[..1]);
        assert!(store.inbox(false).unwrap().is_empty());
        assert!(
            store
                .events_after(events[2].event.seq, true)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn inbox_batches_fit_control_frames_and_only_returned_entries_are_marked_read() {
    let temp = tempfile::tempdir().unwrap();
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(MemoryStore::default()),
        Box::new(FileStore::open(temp.path()).unwrap()),
    ];
    for store in stores {
        for _ in 0..24 {
            store
                .append_inbox(InboxEvent::worker(
                    navarch_proto::Envelope {
                        v: 1,
                        id: uuid::Uuid::new_v4(),
                        from: data_id(0),
                        to: data_id(1),
                        body: navarch_proto::Body::Finished {
                            task_id: navarch_proto::TaskId::new(),
                            session_id: "s".into(),
                            stop_reason: "end_turn".into(),
                            result: "x".repeat(64 * 1024),
                        },
                    },
                    None,
                    "finished",
                    "Complete",
                    0,
                ))
                .unwrap();
        }
        let first = store.inbox(false).unwrap();
        assert!(first.len() < 24);
        assert!(
            serde_json::to_vec(&serde_json::json!({"status":"ok","value":first}))
                .unwrap()
                .len()
                < navarch_proto::MAX_ENVELOPE_BYTES
        );
        let cursor = first.last().unwrap().event.seq;
        let later = store.inbox_after(cursor, false).unwrap();
        assert_eq!(first.len() + later.len(), 24);
        assert_eq!(store.inbox(true).unwrap(), first);
        assert_eq!(store.inbox(false).unwrap(), later);
        assert_eq!(store.inbox(true).unwrap(), later);
        assert!(store.inbox(false).unwrap().is_empty());
    }
}

#[test]
fn a_large_original_envelope_never_blocks_inbox_replay_or_reading_later_rows() {
    let temp = tempfile::tempdir().unwrap();
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(MemoryStore::default()),
        Box::new(FileStore::open(temp.path()).unwrap()),
    ];
    for (index, store) in stores.into_iter().enumerate() {
        let envelope = navarch_proto::Envelope {
            v: 1,
            id: uuid::Uuid::new_v4(),
            from: data_id(0),
            to: data_id(1),
            body: navarch_proto::Body::Finished {
                task_id: navarch_proto::TaskId::new(),
                session_id: "s".repeat(navarch_proto::MAX_ENVELOPE_BYTES - 700),
                stop_reason: "end_turn".into(),
                result: "Complete".into(),
            },
        };
        // The wire envelope is valid; only its added inbox wrapper exceeds the
        // control budget. The opaque session identifier must not be rewritten.
        assert!(serde_json::to_vec(&envelope).unwrap().len() < navarch_proto::MAX_ENVELOPE_BYTES);
        let original = store
            .append_inbox(InboxEvent::worker(
                envelope.clone(),
                Some(navarch_proto::CardId::new()),
                "finished",
                "Complete",
                0,
            ))
            .unwrap()
            .unwrap();
        assert!(
            serde_json::to_vec(&original).unwrap().len() > navarch_proto::MAX_ENVELOPE_BYTES - 1024
        );
        let later = store
            .append_inbox(InboxEvent::worker(
                navarch_proto::Envelope {
                    id: uuid::Uuid::new_v4(),
                    body: navarch_proto::Body::Failed {
                        task_id: navarch_proto::TaskId::new(),
                        error: "Later event".into(),
                    },
                    ..envelope.clone()
                },
                None,
                "failed",
                "Later event",
                0,
            ))
            .unwrap()
            .unwrap();
        let batch = store.inbox(false).unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].event, original.event);
        assert!(batch[0].envelope.is_none());
        assert!(batch[0].envelope_omitted);
        assert_eq!(batch[1], later);
        assert!(
            serde_json::to_vec(&serde_json::json!({"status":"ok","value":batch}))
                .unwrap()
                .len()
                < navarch_proto::MAX_ENVELOPE_BYTES
        );
        assert_eq!(store.inbox(false).unwrap(), batch);
        assert!(store.append_inbox(original.clone()).unwrap().is_none());
        assert_eq!(
            store.inbox_after(original.event.seq, true).unwrap(),
            vec![later]
        );
        assert_eq!(store.inbox(true).unwrap(), vec![batch[0].clone()]);
        assert!(store.inbox(false).unwrap().is_empty());
        if index == 1 {
            let stored = std::fs::read_to_string(temp.path().join("inbox.jsonl")).unwrap();
            let row: serde_json::Value =
                serde_json::from_str(stored.lines().next().unwrap()).unwrap();
            assert_eq!(row["envelope"], serde_json::to_value(envelope).unwrap());
            assert!(row.get("envelope_omitted").is_none());
            assert_eq!(row["read"], true);
        }
    }
}
