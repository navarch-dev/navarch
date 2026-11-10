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
    let mcp = Mcp {
        socket: socket.clone(),
    };
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
            assert_eq!(tools.len(), 10);
            for tool in tools {
                assert!(tool["description"].as_str().unwrap().len() > 40);
                assert_eq!(tool["inputSchema"]["type"], "object");
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
    service.abort();
    let _ = service.await;
    daemon.abort();
    let _ = daemon.await;
    assert!(!socket.exists());
    assert!(!temp.path().join("hub-worker.sock").exists());
    let error = Mcp { socket }
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
