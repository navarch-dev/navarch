use crate::{
    LinkError, RelaySocket,
    relay_socket::{decode, encode, protocol},
};
use navarch_crypto::{PairInfo, PairRole, Pairing};
use navarch_proto::relay::{RelayRequest, RelayResponse};
use std::time::Duration;
/// Reserve a single-use hub room before displaying its code.
pub async fn open_pair_room(socket: &mut RelaySocket, room: &str) -> Result<(), LinkError> {
    socket
        .send(RelayRequest::RoomOpen { room: room.into() })
        .await?;
    if !matches!(socket.recv().await?, RelayResponse::RoomOpened) {
        return Err(protocol());
    }
    Ok(())
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
