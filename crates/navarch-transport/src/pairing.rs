use crate::{
    LinkError, RelaySocket,
    relay_socket::{decode, encode, protocol},
};
use navarch_crypto::{PairCode, PairInfo, PairRole, Pairing};
use navarch_proto::relay::{RelayRequest, RelayResponse};
use std::time::Duration;
/// Reserve a single-use hub room before displaying its code.
pub async fn open_pair_room(socket: &mut RelaySocket, room: &str) -> Result<(), LinkError> {
    socket
        .send(RelayRequest::RoomOpen { room: room.into() })
        .await?;
    match socket.recv().await? {
        RelayResponse::RoomOpened => Ok(()),
        RelayResponse::RoomBusy => Err(LinkError::RoomBusy),
        _ => Err(protocol()),
    }
}
/// Reserve a fresh code, trying each busy room at most once. Never retry a
/// pairing attempt: retries here happen only before a room has been reserved.
pub async fn reserve_pair_room(socket: &mut RelaySocket) -> Result<PairCode, LinkError> {
    reserve(socket, PairCode::generate).await
}
async fn reserve(
    socket: &mut RelaySocket,
    mut generate: impl FnMut() -> PairCode,
) -> Result<PairCode, LinkError> {
    let mut tried = std::collections::HashSet::new();
    while tried.len() < 10000 {
        let code = generate();
        if !tried.insert(code.room().to_owned()) {
            continue;
        }
        match open_pair_room(socket, code.room()).await {
            Ok(()) => return Ok(code),
            Err(LinkError::RoomBusy) => {
                // Respect both relay direction budgets even with many collisions.
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(LinkError::RoomBusy)
}
/// Drive a pure pairing state machine over a reserved room. One failure sends
/// RoomFail and never retries. The worker joins; the hub must reserve first.
pub async fn pair_room(
    socket: &mut RelaySocket,
    room: &str,
    role: PairRole,
    mut pairing: Pairing,
) -> Result<PairInfo, LinkError> {
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(600), async {
        if role == PairRole::Worker {
            socket
                .send(RelayRequest::RoomJoin { room: room.into() })
                .await?;
        }
        if !matches!(socket.recv().await?, RelayResponse::RoomReady { .. }) {
            return Err(protocol());
        }
        if role == PairRole::Hub {
            let message = pairing.start(0).map_err(|_| protocol())?;
            socket
                .send(RelayRequest::RoomData {
                    room: room.into(),
                    step: 1,
                    payload: encode(&message),
                })
                .await?;
        }
        loop {
            let RelayResponse::RoomData { step, payload } = socket.recv().await? else {
                return Err(protocol());
            };
            let bytes = decode(&payload)?;
            if bytes.first() != Some(&step) {
                return Err(protocol());
            }
            let output = pairing
                .receive(&bytes, start.elapsed().as_secs())
                .map_err(|_| protocol())?;
            if let Some(bytes) = output {
                socket
                    .send(RelayRequest::RoomData {
                        room: room.into(),
                        step: bytes[0],
                        payload: encode(&bytes),
                    })
                    .await?;
            }
            if let Ok(peer) = pairing.peer() {
                return Ok(peer.clone());
            }
        }
    })
    .await
    .map_err(|_| protocol())
    .and_then(|r| r);
    if result.is_err() {
        let _ = socket
            .send(RelayRequest::RoomFail { room: room.into() })
            .await;
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use navarch_crypto::Identity;
    use tokio_tungstenite::tungstenite::Message;
    #[tokio::test]
    async fn allocation_retries_a_different_busy_room_but_rejects_other_responses() {
        for busy in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket
                    .send(Message::Text(
                        serde_json::to_string(&RelayResponse::Challenge {
                            challenge: encode(&[0; 32]),
                        })
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket.next().await.unwrap().unwrap();
                socket
                    .send(Message::Text(
                        serde_json::to_string(&RelayResponse::Authenticated)
                            .unwrap()
                            .into(),
                    ))
                    .await
                    .unwrap();
                for room in if busy {
                    vec!["1234", "5678"]
                } else {
                    vec!["1234"]
                } {
                    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                        panic!()
                    };
                    assert!(
                        matches!(serde_json::from_str::<RelayRequest>(&text).unwrap(), RelayRequest::RoomOpen { room: actual } if actual == room)
                    );
                    let response = if room == "5678" {
                        RelayResponse::RoomOpened
                    } else if busy {
                        RelayResponse::RoomBusy
                    } else {
                        RelayResponse::RoomClosed
                    };
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&response).unwrap().into(),
                        ))
                        .await
                        .unwrap();
                }
            });
            let mut socket = RelaySocket::connect(&url, &Identity::generate())
                .await
                .unwrap();
            let mut codes = ["1234-123456", "1234-654321", "5678-123456"].into_iter();
            let result = reserve(&mut socket, || {
                PairCode::parse(codes.next().unwrap()).unwrap()
            })
            .await;
            if busy {
                assert_eq!(result.unwrap().room(), "5678");
            } else {
                assert!(result.is_err());
            }
            server.await.unwrap();
        }
    }
}
