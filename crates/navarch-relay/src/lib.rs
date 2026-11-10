//! Opaque, authenticated, memory-only WebSocket forwarding server.
#![forbid(unsafe_code)]
mod rooms;
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::StreamExt;
use navarch_proto::{
    MachineId,
    relay::{MAX_RELAY_FRAME, RelayRequest, RelayResponse},
};
use rand::{RngCore, rngs::OsRng};
use rooms::Room;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc},
    time::Instant,
};
type Output = mpsc::Sender<Option<RelayResponse>>;
struct Connection {
    token: u64,
    output: Output,
    stop: tokio::sync::watch::Sender<bool>,
}
#[derive(Default)]
struct Registry {
    connections: HashMap<MachineId, Connection>,
    rooms: HashMap<String, Room>,
    next: u64,
}
#[derive(Clone, Default)]
struct Relay(Arc<Mutex<Registry>>);
/// Serve `/v1/ws` and `/healthz` on a caller-bound listener.
pub async fn serve(listener: tokio::net::TcpListener) -> std::io::Result<()> {
    tracing::info!(event = "listening", address = %listener.local_addr()?, "relay listening");
    let state = Relay::default();
    axum::serve(
        listener,
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/v1/ws", get(upgrade))
            .with_state(state),
    )
    .await
}
async fn upgrade(State(state): State<Relay>, ws: WebSocketUpgrade) -> axum::response::Response {
    ws.max_message_size(MAX_RELAY_FRAME)
        .max_frame_size(MAX_RELAY_FRAME)
        .on_upgrade(move |socket| connection(state, socket))
}
#[derive(Clone, Copy)]
enum CloseReason {
    Closed,
    Invalid,
    Limit,
    Replaced,
    Overloaded,
}
impl CloseReason {
    fn label(self) -> &'static str {
        match self {
            Self::Closed => "socket_closed",
            Self::Invalid => "invalid_request",
            Self::Limit => "limit_exceeded",
            Self::Replaced => "replaced",
            Self::Overloaded => "output_closed",
        }
    }
}
fn limit(direction: &'static str, kind: &'static str) -> CloseReason {
    tracing::info!(
        event = "limit_exceeded",
        direction,
        limit = kind,
        "relay limit exceeded"
    );
    CloseReason::Limit
}
async fn send(
    socket: &mut WebSocket,
    response: RelayResponse,
    limits: &mut Limits,
) -> Result<(), CloseReason> {
    let text = serde_json::to_string(&response).map_err(|_| CloseReason::Invalid)?;
    if !limits.accept(text.len(), Instant::now()) {
        return Err(limit("outgoing", "frame_rate_or_bytes"));
    }
    let size = text.len();
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| CloseReason::Closed)?;
    limits.record(size);
    Ok(())
}
async fn request(socket: &mut WebSocket, limits: &mut Limits) -> Result<RelayRequest, CloseReason> {
    match socket.next().await {
        Some(Ok(Message::Text(text))) => {
            limits.record(text.len());
            if !limits.accept(text.len(), Instant::now()) {
                return Err(limit("incoming", "frame_rate_or_bytes"));
            }
            serde_json::from_str(&text).map_err(|_| CloseReason::Invalid)
        }
        Some(Err(error)) => {
            // Inspect only the error type, never format errors carrying peer data.
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            while let Some(error) = source {
                if matches!(
                    error.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
                    Some(tokio_tungstenite::tungstenite::Error::Capacity(_))
                ) {
                    return Err(limit("incoming", "frame_size"));
                }
                source = error.source();
            }
            Err(CloseReason::Closed)
        }
        Some(Ok(Message::Close(_))) | None => Err(CloseReason::Closed),
        _ => Err(CloseReason::Invalid),
    }
}
async fn connection(state: Relay, mut socket: WebSocket) {
    let mut outgoing = Limits::default();
    let mut challenge = [0; 32];
    OsRng.fill_bytes(&mut challenge);
    if send(
        &mut socket,
        RelayResponse::Challenge {
            challenge: B64.encode(challenge),
        },
        &mut outgoing,
    )
    .await
    .is_err()
    {
        return;
    }
    let mut limits = Limits::default();
    let auth =
        tokio::time::timeout(Duration::from_secs(10), request(&mut socket, &mut limits)).await;
    let Ok(Ok(RelayRequest::Authenticate {
        public_key,
        signature,
    })) = auth
    else {
        return;
    };
    let Ok(sig) = B64.decode(signature) else {
        return;
    };
    if navarch_crypto::verify_challenge(&public_key, &challenge, &sig).is_err() {
        return;
    }
    let Ok(id) = navarch_crypto::machine_id(&public_key).parse::<MachineId>() else {
        return;
    };
    let (tx, mut rx) = mpsc::channel(64);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let token = {
        let mut registry = state.0.lock().await;
        registry.next += 1;
        let token = registry.next;
        if let Some(old) = registry.connections.insert(
            id.clone(),
            Connection {
                token,
                output: tx.clone(),
                stop,
            },
        ) {
            registry.close_for(&id);
            let _ = old.stop.send(true);
        }
        token
    };
    tracing::info!(event = "connection_authenticated", machine = %id, "relay authenticated");
    let reason = match send(&mut socket, RelayResponse::Authenticated, &mut outgoing).await {
        Err(reason) => reason,
        Ok(()) => loop {
            let expires = state
                .0
                .lock()
                .await
                .rooms
                .values()
                .map(|r| if r.closed { r.reusable } else { r.expires })
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(600));
            tokio::select! {
                _ = stopped.changed() => break CloseReason::Replaced,
                _ = tokio::time::sleep_until(expires) => {
                    state.0.lock().await.expire_rooms(Instant::now());
                },
                message = request(&mut socket, &mut limits) => {
                    let message = match message { Ok(message) => message, Err(reason) => break reason };
                    let mut registry = state.0.lock().await;
                    if registry.connections.get(&id).is_none_or(|c| c.token != token) {
                        break CloseReason::Replaced;
                    }
                    if !registry.handle(&id, &tx, message) { break CloseReason::Invalid; }
                },
                output = rx.recv() => match output {
                    Some(Some(response)) => {
                        if let Err(reason) = send(&mut socket, response, &mut outgoing).await { break reason; }
                    },
                    _ => break CloseReason::Overloaded,
                },
            }
        },
    };
    let mut registry = state.0.lock().await;
    registry.disconnect(&id, token);
    tracing::info!(event = "connection_closed", machine = %id, reason = reason.label(),
        incoming_frames = limits.count_frames, incoming_bytes = limits.count_bytes,
        outgoing_frames = outgoing.count_frames, outgoing_bytes = outgoing.count_bytes,
        "relay connection closed");
}
fn emit(out: &Output, response: RelayResponse) -> bool {
    if out.try_send(Some(response)).is_err() {
        limit("outgoing", "queue_capacity");
        return false;
    }
    true
}
impl Registry {
    fn disconnect(&mut self, id: &MachineId, token: u64) {
        if self.connections.get(id).is_some_and(|c| c.token == token) {
            self.connections.remove(id);
            self.close_for(id);
        }
    }
    fn handle(&mut self, id: &MachineId, out: &Output, message: RelayRequest) -> bool {
        match message {
            RelayRequest::Route { to, payload } => {
                if B64.decode(&payload).is_err() {
                    return false;
                }
                let response = RelayResponse::Routed {
                    from: id.clone(),
                    payload,
                };
                // Adding `from` must not turn a legal input into an oversized output.
                if serde_json::to_vec(&response).map_or(true, |b| b.len() > MAX_RELAY_FRAME) {
                    limit("outgoing", "frame_size");
                    return false;
                }
                if let Some(peer) = self.connections.get(&to) {
                    emit(&peer.output, response)
                } else {
                    emit(
                        out,
                        RelayResponse::Offline {
                            error: navarch_proto::relay::OfflineError::Offline,
                            to,
                        },
                    )
                }
            }
            RelayRequest::Authenticate { .. } => false,
            request => self.room_request(id, out, request, Instant::now()),
        }
    }
}
#[derive(Default)]
struct Limits {
    count_frames: u64,
    count_bytes: u64,
    frames: VecDeque<Instant>,
    bytes: VecDeque<(Instant, usize)>,
    total: usize,
}
impl Limits {
    fn record(&mut self, size: usize) {
        self.count_frames += 1;
        self.count_bytes += size as u64;
    }
    fn accept(&mut self, size: usize, now: Instant) -> bool {
        while self
            .frames
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
        {
            self.frames.pop_front();
        }
        while self
            .bytes
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) >= Duration::from_secs(60))
        {
            if let Some((_, n)) = self.bytes.pop_front() {
                self.total -= n;
            }
        }
        if size > MAX_RELAY_FRAME
            || self.frames.len() >= 60
            || self.total.saturating_add(size) > 16 * 1024 * 1024
        {
            return false;
        }
        self.frames.push_back(now);
        self.bytes.push_back((now, size));
        self.total += size;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_frame_rate_and_rolling_byte_limits() {
        let now = Instant::now();
        let mut limits = Limits::default();
        assert!(!limits.accept(MAX_RELAY_FRAME + 1, now));
        for _ in 0..60 {
            assert!(limits.accept(1, now));
        }
        assert!(!limits.accept(1, now));
        assert!(limits.accept(1, now + Duration::from_secs(1)));
        let mut limits = Limits::default();
        for _ in 0..16 {
            assert!(limits.accept(MAX_RELAY_FRAME, now));
        }
        assert!(!limits.accept(1, now + Duration::from_secs(59)));
        assert!(limits.accept(MAX_RELAY_FRAME, now + Duration::from_secs(60)));
    }
}
