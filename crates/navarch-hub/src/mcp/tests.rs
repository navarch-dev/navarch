#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if control::call(path, &Request::ListWorkers).await.is_ok() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn rmcp_handshake_tool_discovery_forwarding_and_errors() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("hub.sock");
    let hello = navarch_transport::LocalHello {
        public_key: navarch_crypto::Identity::generate().public_key(),
        display_name: "hub".into(),
    };
    let store = Arc::new(navarch_store::MemoryStore::default());
    use navarch_store::Store;
    store
        .put_machine(navarch_store::Machine {
            id: hello.machine_id().unwrap(),
            public_key: [171; 32],
            static_public_key: None,
            display_name: "test".into(),
            state: navarch_store::MachineState::Active,
            capabilities: None,
        })
        .unwrap();
    let home = temp.path().to_owned();
    let daemon = tokio::spawn(async move {
        crate::serve(&home, hello, store).await.unwrap();
    });
    ready(&socket).await;
    let (client, server) = tokio::io::duplex(65536);
    let mcp = Mcp::new(&socket, false);
    let service = tokio::spawn(async move {
        let running = mcp.serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    let (read, mut write) = tokio::io::split(client);
    let mut reader = BufReader::new(read);
    write.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline-test","version":"1"}}})).as_bytes()).await.unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let init: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "navarch");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    for (id, method, params) in [
        (2, "tools/list", json!({})),
        (
            3,
            "tools/call",
            json!({"name":"list_workers","arguments":{}}),
        ),
        (
            5,
            "tools/call",
            json!({"name":"list_sessions","arguments":{"agent":"fake","cwd":"/work","limit":7}}),
        ),
        (
            4,
            "tools/call",
            json!({"name":"get_task","arguments":{"task_id":"00000000-0000-4000-8000-000000000001"}}),
        ),
    ] {
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        if id == 2 {
            let tools = response["result"]["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 11);
            for tool in tools {
                assert!(tool["description"].as_str().unwrap().len() > 40);
                assert_eq!(tool["inputSchema"]["type"], "object");
                if tool["name"] == "list_sessions" {
                    let properties = &tool["inputSchema"]["properties"];
                    assert_eq!(properties["agent"]["type"], "string");
                    assert_eq!(properties["cwd"]["type"], "string");
                    assert_eq!(properties["limit"]["type"], "integer");
                    assert_eq!(properties["limit"]["default"], 50);
                }
                if tool["name"] == "get_task" {
                    assert_eq!(
                        tool["inputSchema"]["properties"]["include_log"]["default"],
                        false
                    );
                }
                if tool["name"] == "dispatch" || tool["name"] == "send_to_session" {
                    assert!(tool["inputSchema"]["properties"].get("card_id").is_some());
                }
            }
        } else if id == 3 {
            assert_eq!(response["result"]["isError"], false);
            let workers: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(workers[0]["machine"]["public_key"], "ab".repeat(32));
        } else if id == 5 {
            assert_eq!(response["result"]["isError"], false);
            let list: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(list["sessions"], json!([]));
            assert_eq!(list["stale"], true);
            assert_eq!(list["truncated"], false);
        } else {
            assert_eq!(response["result"]["isError"], true);
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("unknown task")
            );
        }
    }
    let oversize = Mcp::new(&socket, false).invoke("dispatch", serde_json::from_value(json!({
        "machine":"aaaaaaaaaaaaaaaaaaaaaaaaaa","agent":"fake","cwd":"/work","prompt":"x".repeat(navarch_transport::MAX_FRAME)
    })).unwrap()).await;
    assert_eq!(oversize.is_error, Some(true));
    assert!(
        serde_json::to_string(&oversize)
            .unwrap()
            .contains("shorten the prompt or payload")
    );
    service.abort();
    let _ = service.await;
    daemon.abort();
    let _ = daemon.await;
    assert!(!socket.exists());
    assert!(!temp.path().join("hub-worker.sock").exists());
    let error = Mcp::new(&socket, false)
        .invoke("list_workers", Default::default())
        .await;
    assert_eq!(error.is_error, Some(true));
    assert!(
        serde_json::to_string(&error)
            .unwrap()
            .contains("start `navarch hub serve`")
    );
}
#[tokio::test]
async fn daemon_control_handles_multiple_lines_malformed_input_and_private_sockets() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().to_owned();
    let socket = home.join("hub.sock");
    let hello = navarch_transport::LocalHello {
        public_key: navarch_crypto::Identity::generate().public_key(),
        display_name: "hub".into(),
    };
    let daemon = tokio::spawn(async move {
        crate::serve(
            &home,
            hello,
            Arc::new(navarch_store::MemoryStore::default()),
        )
        .await
        .unwrap();
    });
    ready(&socket).await;
    for name in ["hub.sock", "hub-worker.sock"] {
        assert_eq!(
            std::fs::metadata(temp.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(b"{broken}\n{\"type\":\"list_workers\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(matches!(
        serde_json::from_str::<control::Response>(&line).unwrap(),
        control::Response::Error { .. }
    ));
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert!(matches!(
        serde_json::from_str::<control::Response>(&line).unwrap(),
        control::Response::Ok { .. }
    ));
    daemon.abort();
    let _ = daemon.await;
}

async fn mcp_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn channel_wire_shape_filtering_reconnect_and_disabled_silence() {
    for channel in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("hub.sock");
        // MCP initializes while the daemon is absent.
        let mcp = Mcp::new(&socket, channel);
        let (client, server) = tokio::io::duplex(65536);
        let service = tokio::spawn(async move {
            let running = mcp.serve(server).await.unwrap();
            running.waiting().await.unwrap();
        });
        let (read, mut write) = tokio::io::split(client);
        let mut reader = BufReader::new(read);
        write.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"channel-test","version":"1"}}})).as_bytes()).await.unwrap();
        let init = mcp_line(&mut reader).await;
        assert_eq!(
            init["result"]["capabilities"]["experimental"]["claude/channel"],
            json!({})
        );
        // rmcp echoes a known client revision despite its older default.
        assert_eq!(init["result"]["protocolVersion"], "2026-07-28");
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        if !channel {
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
                )
                .await
                .unwrap();
            assert_eq!(mcp_line(&mut reader).await["id"], 2);
        } else {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let (read, mut output) = stream.into_split();
            let mut requests = BufReader::new(read);
            let request = mcp_line(&mut requests).await;
            assert_eq!(request, json!({"type":"subscribe_live"}));
            output
                .write_all(b"{\"kind\":\"ready\",\"seq\":40}\n")
                .await
                .unwrap();
            // Drop before any event is delivered: retain the baseline on reconnect.
            drop(output);
            drop(requests);
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut output) = stream.into_split();
            let mut requests = BufReader::new(read);
            assert_eq!(
                mcp_line(&mut requests).await,
                json!({"type":"subscribe","after_seq":40})
            );
            let machine: navarch_proto::MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
            for (index, kind) in [
                "worker_online",
                "finished",
                "failed",
                "permission_ask",
                "rejected",
                "machine_pending",
            ]
            .into_iter()
            .enumerate()
            {
                let event = crate::HubEvent {
                    seq: index as u64 + 41,
                    kind: kind.into(),
                    card_id: Some(navarch_proto::CardId::new()),
                    step: Some(navarch_proto::TaskId::new()),
                    machine: machine.clone(),
                    summary: "agent output".into(),
                    time: 1,
                };
                output
                    .write_all(format!("{}\n", serde_json::to_string(&event).unwrap()).as_bytes())
                    .await
                    .unwrap();
                if kind == "worker_online" {
                    continue;
                }
                let notification = mcp_line(&mut reader).await;
                assert_eq!(notification["jsonrpc"], "2.0");
                assert_eq!(notification["method"], "notifications/claude/channel");
                assert!(notification.get("id").is_none());
                let params = notification["params"].as_object().unwrap();
                assert_eq!(params.len(), 2);
                assert!(
                    params["content"]
                        .as_str()
                        .unwrap()
                        .contains(&machine.to_string())
                );
                if kind != "machine_pending" {
                    let content = params["content"].as_str().unwrap();
                    assert!(content.contains(&event.summary));
                    assert!(content.contains(if kind == "permission_ask" {
                        "answer_permission"
                    } else {
                        "get_task"
                    }));
                }
                let meta = params["meta"].as_object().unwrap();
                assert_eq!(meta.len(), 5);
                assert_eq!(meta["kind"], kind);
                assert_eq!(meta["seq"], event.seq.to_string());
                assert_eq!(meta["card_id"], event.card_id.unwrap().to_string());
                for (key, value) in meta {
                    assert!(value.is_string());
                    assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                }
                // No client acknowledgement is sent; subsequent notifications still arrive.
            }
            drop(output);
            drop(requests);
            let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let (read, mut output) = stream.into_split();
            let mut requests = BufReader::new(read);
            assert_eq!(
                mcp_line(&mut requests).await,
                json!({"type":"subscribe","after_seq":46})
            );
            let event = crate::HubEvent {
                seq: 47,
                kind: "finished".into(),
                card_id: None,
                step: None,
                machine,
                summary: "after restart".into(),
                time: 1,
            };
            output
                .write_all(format!("{}\n", serde_json::to_string(&event).unwrap()).as_bytes())
                .await
                .unwrap();
            assert_eq!(mcp_line(&mut reader).await["params"]["meta"]["seq"], "47");
        }
        service.abort();
        let _ = service.await;
    }
}

#[tokio::test]
async fn control_subscription_replay_live_and_sequence_after_daemon_restart() {
    use navarch_store::Store;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().to_owned();
    let socket = home.join("hub.sock");
    let hello = navarch_transport::LocalHello {
        public_key: navarch_crypto::Identity::generate().public_key(),
        display_name: "hub".into(),
    };
    let mut last = 0;
    for generation in 0..2 {
        let store = Arc::new(navarch_store::FileStore::open(home.join("hub")).unwrap());
        let h = home.clone();
        let intro = hello.clone();
        let state = store.clone();
        let daemon = tokio::spawn(async move { crate::serve(&h, intro, state).await.unwrap() });
        ready(&socket).await;
        let (baseline, mut live) = control::subscribe_live(&socket).await.unwrap();
        assert_eq!(baseline, last);
        let mut subscription = control::subscribe(&socket, Some(last)).await.unwrap();
        for index in 0..2 {
            control::call(
                &socket,
                &Request::RegisterPaired {
                    peer: navarch_crypto::PairInfo {
                        identity: navarch_crypto::Identity::generate().public_key(),
                        static_key: navarch_crypto::StaticKey::generate().public_key(),
                        display_name: format!("worker-{generation}-{index}"),
                    },
                },
            )
            .await
            .unwrap();
            let event = tokio::time::timeout(Duration::from_secs(3), subscription.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let control::StreamEvent::Event(event) = event else {
                panic!("expected event");
            };
            assert_eq!(event.kind, "machine_pending");
            let control::StreamEvent::Event(live_event) = live.recv().await.unwrap().unwrap()
            else {
                panic!("expected live event");
            };
            assert_eq!(live_event, event);
            assert!(event.seq > last);
            last = event.seq;
        }
        let unread = store.inbox(false).unwrap();
        let before_last = unread[unread.len() - 2].event.seq;
        let mut replay = control::subscribe(&socket, Some(before_last))
            .await
            .unwrap();
        let control::StreamEvent::Event(event) = replay.recv().await.unwrap().unwrap() else {
            panic!("expected replay");
        };
        assert_eq!(event.seq, last);
        assert_eq!(store.inbox(false).unwrap(), unread);
        assert_eq!(
            control::call(
                &socket,
                &Request::WaitForEvents {
                    timeout_secs: Some(0),
                    after_seq: Some(before_last)
                }
            )
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
            1
        );
        store.inbox(true).unwrap();
        daemon.abort();
        let _ = daemon.await;
    }
}

#[tokio::test]
async fn wait_tool_and_control_return_only_compact_events_while_read_inbox_keeps_envelope() {
    use navarch_store::Store;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("hub.sock");
    let hello = navarch_transport::LocalHello {
        public_key: navarch_crypto::Identity::generate().public_key(),
        display_name: "hub".into(),
    };
    let machine = hello.machine_id().unwrap();
    let envelope = navarch_proto::Envelope {
        v: 1,
        id: uuid::Uuid::new_v4(),
        from: machine.clone(),
        to: machine,
        body: navarch_proto::Body::Finished {
            task_id: navarch_proto::TaskId::new(),
            session_id: "session".into(),
            stop_reason: "end_turn".into(),
            result: "large outcome".repeat(1000),
        },
    };
    let store = Arc::new(navarch_store::MemoryStore::default());
    let entry = store
        .append_inbox(navarch_store::InboxEvent::worker(
            envelope.clone(),
            Some(navarch_proto::CardId::new()),
            "finished",
            "bounded outcome",
            1,
        ))
        .unwrap()
        .unwrap();
    let home = temp.path().to_owned();
    let daemon = tokio::spawn(async move { crate::serve(&home, hello, store).await.unwrap() });
    ready(&socket).await;
    let request = Request::WaitForEvents {
        timeout_secs: Some(0),
        after_seq: None,
    };
    let expected = json!([entry.event]);
    assert_eq!(control::call(&socket, &request).await.unwrap(), expected);
    let mcp = Mcp::new(&socket, false);
    let result = mcp
        .invoke(
            "wait_for_events",
            serde_json::from_value(json!({"timeout_secs":0})).unwrap(),
        )
        .await;
    let serialized = serde_json::to_value(result).unwrap();
    let events: Value =
        serde_json::from_str(serialized["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(events, expected);
    assert_eq!(events[0].as_object().unwrap().len(), 7);
    let inbox = control::call(&socket, &Request::ReadInbox).await.unwrap();
    assert_eq!(
        inbox[0]["envelope"],
        serde_json::to_value(envelope).unwrap()
    );
    daemon.abort();
}
