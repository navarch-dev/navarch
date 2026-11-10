//! Real loopback WebSockets, ephemeral ports, no external service.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use navarch_crypto::Identity;
use navarch_proto::{
    MachineId,
    relay::{MAX_RELAY_FRAME, RelayRequest as Q, RelayResponse as R},
};
use navarch_transport::RelaySocket;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
struct Server {
    url: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server() -> Server {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/ws", l.local_addr().unwrap());
    Server {
        url,
        task: tokio::spawn(navarch_relay::serve(l)),
    }
}
async fn client(s: &Server) -> (Identity, RelaySocket) {
    let i = Identity::generate();
    let c = RelaySocket::connect(&s.url, &i).await.unwrap();
    (i, c)
}
async fn next(c: &mut RelaySocket) -> R {
    tokio::time::timeout(Duration::from_secs(5), c.recv())
        .await
        .unwrap()
        .unwrap()
}
fn id(i: &Identity) -> MachineId {
    i.machine_id().parse().unwrap()
}
#[tokio::test]
async fn routes_authenticated_source_offline_and_duplicate_replaces_old() {
    let s = server().await;
    let (a, mut ca) = client(&s).await;
    let (b, mut cb) = client(&s).await;
    let payload = B64.encode(b"opaque");
    ca.send(Q::Route {
        to: id(&b),
        payload: payload.clone(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut cb).await,R::Routed{from,payload:p} if from==id(&a) && p==payload));
    let unknown = id(&Identity::generate());
    ca.send(Q::Route {
        to: unknown.clone(),
        payload: payload.clone(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut ca).await,R::Offline{to,..} if to==unknown));
    let mut replacement = RelaySocket::connect(&s.url, &b).await.unwrap();
    assert!(cb.recv().await.is_err());
    ca.send(Q::Route {
        to: id(&b),
        payload: payload.clone(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut replacement).await,R::Routed{payload:p,..} if p==payload));
}
#[tokio::test]
async fn rooms_exactly_two_ordered_and_failure_tombstones_room() {
    let s = server().await;
    let (_, mut h) = client(&s).await;
    let (_, mut w) = client(&s).await;
    let (_, mut third) = client(&s).await;
    h.send(Q::RoomOpen {
        room: "1000".into(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut h).await, R::RoomOpened));
    w.send(Q::RoomJoin {
        room: "1000".into(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut h).await, R::RoomReady { .. }));
    assert!(matches!(next(&mut w).await, R::RoomReady { .. }));
    h.send(Q::RoomData {
        room: "1000".into(),
        step: 1,
        payload: B64.encode(b"opaque"),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut w).await, R::RoomData { step: 1, .. }));
    w.send(Q::RoomFail {
        room: "1000".into(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut h).await, R::RoomClosed));
    assert!(matches!(next(&mut w).await, R::RoomClosed));
    third
        .send(Q::RoomJoin {
            room: "1000".into(),
        })
        .await
        .unwrap();
    assert!(matches!(next(&mut third).await, R::RoomClosed));
    third
        .send(Q::RoomOpen {
            room: "1000".into(),
        })
        .await
        .unwrap();
    assert!(matches!(next(&mut third).await, R::RoomBusy));
}
#[tokio::test]
async fn room_third_member_and_out_of_order_close_room() {
    for third in [false, true] {
        let s = server().await;
        let (_, mut h) = client(&s).await;
        let (_, mut w) = client(&s).await;
        let (_, mut c) = client(&s).await;
        h.send(Q::RoomOpen {
            room: "2000".into(),
        })
        .await
        .unwrap();
        next(&mut h).await;
        w.send(Q::RoomJoin {
            room: "2000".into(),
        })
        .await
        .unwrap();
        next(&mut h).await;
        next(&mut w).await;
        if third {
            c.send(Q::RoomJoin {
                room: "2000".into(),
            })
            .await
            .unwrap();
        } else {
            w.send(Q::RoomData {
                room: "2000".into(),
                step: 2,
                payload: B64.encode(b"bad"),
            })
            .await
            .unwrap();
        }
        assert!(matches!(next(&mut h).await, R::RoomClosed));
    }
}
#[tokio::test]
async fn room_expiry_notifies_both_and_rejects_late_code() {
    let s = server().await;
    let (_, mut h) = client(&s).await;
    let (_, mut w) = client(&s).await;
    h.send(Q::RoomOpen {
        room: "3000".into(),
    })
    .await
    .unwrap();
    next(&mut h).await;
    w.send(Q::RoomJoin {
        room: "3000".into(),
    })
    .await
    .unwrap();
    next(&mut h).await;
    next(&mut w).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(601)).await;
    tokio::time::resume();
    assert!(matches!(next(&mut h).await, R::RoomClosed));
    assert!(matches!(next(&mut w).await, R::RoomClosed));
    let (_, mut c) = client(&s).await;
    c.send(Q::RoomJoin {
        room: "3000".into(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut c).await, R::RoomClosed));
}
#[tokio::test]
async fn real_websocket_rate_limit() {
    let s = server().await;
    let (_, mut c) = client(&s).await;
    let offline = id(&Identity::generate());
    let spinner = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    tokio::time::pause();
    for _ in 0..61 {
        if c.send(Q::Route {
            to: offline.clone(),
            payload: String::new(),
        })
        .await
        .is_err()
        {
            break;
        }
    } // All sends are queued before time resumes; read responses while holding virtual time.
    let mut count = 0;
    while let Ok(R::Offline { .. }) = c.recv().await {
        count += 1;
    }
    spinner.abort();
    tokio::time::resume();
    assert!(count <= 60, "rate limit did not close connection");
}
#[tokio::test]
async fn real_websocket_byte_limit_and_frame_limit() {
    let s = server().await;
    let (_, mut c) = client(&s).await;
    let offline = id(&Identity::generate());
    let payload = B64.encode(vec![7; 700000]);
    for round in 0..18 {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::resume();
        c.send(Q::Route {
            to: offline.clone(),
            payload: payload.clone(),
        })
        .await
        .unwrap();
        if round == 17 {
            assert!(c.recv().await.is_err());
        } else {
            assert!(matches!(next(&mut c).await, R::Offline { .. }));
        }
    }
    use futures_util::{SinkExt, StreamExt};
    let (mut raw, _) = tokio_tungstenite::connect_async(&s.url).await.unwrap();
    raw.next().await.unwrap().unwrap();
    if raw
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "x".repeat(MAX_RELAY_FRAME + 1).into(),
        ))
        .await
        .is_ok()
    {
        assert!(matches!(
            raw.next().await,
            Some(Err(_)) | None | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
        ));
    }
}
#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn log_capture() -> Arc<Mutex<Vec<u8>>> {
    static BYTES: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    BYTES
        .get_or_init(|| {
            let bytes = Arc::new(Mutex::new(vec![]));
            let capture = Capture(bytes.clone());
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || capture.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            bytes
        })
        .clone()
}
async fn logged_lifecycle() -> (String, String) {
    let bytes = log_capture();
    let s = server().await;
    let (a, mut ca) = client(&s).await;
    let (b, mut cb) = client(&s).await;
    let secret = "DO-NOT-LOG-THIS-OPAQUE-PAYLOAD-73928";
    let payload = B64.encode(secret);
    ca.send(Q::Route {
        to: id(&b),
        payload: payload.clone(),
    })
    .await
    .unwrap();
    next(&mut cb).await;
    // Exercise room lifecycle and pairing bytes under the same capture.
    ca.send(Q::RoomOpen {
        room: "7392".into(),
    })
    .await
    .unwrap();
    assert!(matches!(next(&mut ca).await, R::RoomOpened));
    cb.send(Q::RoomJoin {
        room: "7392".into(),
    })
    .await
    .unwrap();
    next(&mut ca).await;
    next(&mut cb).await;
    for step in 1..=6 {
        let (sender, receiver) = if step % 2 == 1 {
            (&mut ca, &mut cb)
        } else {
            (&mut cb, &mut ca)
        };
        sender
            .send(Q::RoomData {
                room: "7392".into(),
                step,
                payload: payload.clone(),
            })
            .await
            .unwrap();
        next(receiver).await;
    }
    ca.send(Q::RoomOpen {
        room: "7393".into(),
    })
    .await
    .unwrap();
    next(&mut ca).await;
    ca.send(Q::RoomFail {
        room: "7393".into(),
    })
    .await
    .unwrap();
    next(&mut ca).await;
    // Malformed bytes are rejected without including serde diagnostics in logs.
    ca.send(Q::Route {
        to: id(&b),
        payload: format!("{secret}!"),
    })
    .await
    .unwrap();
    assert!(ca.recv().await.is_err());
    let (_, mut limited) = client(&s).await;
    let offline = id(&Identity::generate());
    let spinner = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    tokio::time::pause();
    for _ in 0..61 {
        if limited
            .send(Q::Route {
                to: offline.clone(),
                payload: String::new(),
            })
            .await
            .is_err()
        {
            break;
        }
    }
    while limited.recv().await.is_ok() {}
    spinner.abort();
    tokio::time::resume();
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("connection_authenticated"));
    assert!(logs.contains(&a.machine_id()));
    assert!(!logs.contains(secret));
    assert!(!logs.contains(&payload));
    assert!(!logs.contains("public_key"));
    assert!(
        !logs.contains(
            &a.public_key()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    );
    assert!(!logs.contains("room=7392"));
    assert!(!logs.contains("room=7393"));
    (logs, a.machine_id())
}
#[tokio::test]
async fn structured_logs_never_contain_raw_or_base64_payload() {
    logged_lifecycle().await;
}
#[tokio::test]
async fn expected_info_lifecycle_lines_include_outcomes_and_directional_counts() {
    let (logs, machine) = logged_lifecycle().await;
    assert_expected_lifecycle_lines(&logs, &machine);
}
#[tokio::test]
async fn challenge_replay_wrong_signature_unauthenticated_routing_and_health() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let s = server().await;
    let identity = Identity::generate();
    let mut prior = None;
    for attempt in 0..3 {
        let (mut raw, _) = tokio_tungstenite::connect_async(&s.url).await.unwrap();
        let Message::Text(text) = raw.next().await.unwrap().unwrap() else {
            panic!()
        };
        let R::Challenge { challenge } = serde_json::from_str(&text).unwrap() else {
            panic!()
        };
        let challenge: [u8; 32] = B64.decode(challenge).unwrap().try_into().unwrap();
        let request = match attempt {
            0 => {
                prior = Some(identity.sign_challenge(&challenge));
                Q::Route {
                    to: id(&identity),
                    payload: String::new(),
                }
            }
            1 => Q::Authenticate {
                public_key: identity.public_key(),
                signature: B64.encode(prior.unwrap()),
            },
            _ => Q::Authenticate {
                public_key: identity.public_key(),
                signature: B64.encode([0; 64]),
            },
        };
        raw.send(Message::Text(
            serde_json::to_string(&request).unwrap().into(),
        ))
        .await
        .unwrap();
        assert!(!matches!(raw.next().await, Some(Ok(Message::Text(_)))));
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = s.url.trim_start_matches("ws://").trim_end_matches("/v1/ws");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut result = String::new();
    stream.read_to_string(&mut result).await.unwrap();
    assert!(result.starts_with("HTTP/1.1 200"));
    assert!(result.ends_with("ok"));
}

#[tokio::test]
async fn multiple_senders_cannot_bypass_recipient_byte_budget() {
    let s = server().await;
    let (_, mut a) = client(&s).await;
    let (_, mut b) = client(&s).await;
    let (peer, mut receiver) = client(&s).await;
    let payload = B64.encode(vec![8; 700000]);
    for n in 0..18 {
        let sender = if n < 9 { &mut a } else { &mut b };
        sender
            .send(Q::Route {
                to: id(&peer),
                payload: payload.clone(),
            })
            .await
            .unwrap();
        if n < 17 {
            assert!(matches!(next(&mut receiver).await, R::Routed { .. }));
        } else {
            assert!(receiver.recv().await.is_err());
        }
    }
}

fn assert_expected_lifecycle_lines(logs: &str, machine: &str) {
    for event in [
        "listening",
        "connection_authenticated",
        "connection_closed",
        "room_opened",
        "room_closed",
        "limit_exceeded",
    ] {
        assert!(
            logs.lines()
                .any(|line| line.contains("INFO") && line.contains(&format!("event=\"{event}\""))),
            "missing event {event}: {logs}"
        );
    }
    assert!(logs.lines().any(|line| line.contains("event=\"listening\"") && line.contains("address=127.0.0.1:")));
    let closed = logs
        .lines()
        .find(|line| line.contains("event=\"connection_closed\"") && line.contains(machine))
        .unwrap();
    for field in [
        "reason=\"invalid_request\"",
        "incoming_frames=",
        "incoming_bytes=",
        "outgoing_frames=",
        "outgoing_bytes=",
    ] {
        assert!(closed.contains(field), "missing {field}: {closed}");
    }
    for outcome in ["completed", "failed"] {
        assert!(
            logs.lines()
                .any(|line| line.contains("event=\"room_closed\"")
                    && line.contains(machine)
                    && line.contains(&format!("outcome=\"{outcome}\"")))
        );
    }
}
