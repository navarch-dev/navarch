use crate::*;
use navarch_proto::{Body, Envelope, PROTOCOL_VERSION};
use std::collections::{HashSet, VecDeque};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Received envelope awaiting application. Acknowledge only after durable application.
pub struct Delivery {
    /// Validated, addressed envelope.
    pub envelope: Envelope,
    applied: oneshot::Sender<bool>,
}
impl Delivery {
    /// Confirm successful application, or request redelivery by returning false.
    pub fn complete(self, applied: bool) {
        let _ = self.applied.send(applied);
    }
}
/// Connection lifecycle and incoming application messages.
pub enum ReliableEvent {
    /// A fresh connection is ready; callers should send a fresh ProbeReport.
    Connected(MachineId),
    /// The current link failed; pending envelopes are retained.
    Disconnected(MachineId),
    /// A nonduplicate envelope requiring application.
    Message(Box<Delivery>),
}
/// Outbound reliable message queue, retained across link replacements.
#[derive(Clone)]
pub struct ReliableHandle(mpsc::UnboundedSender<Body>);
impl ReliableHandle {
    /// Queue a body. Envelope IDs remain stable through retries.
    pub fn send(&self, body: Body) -> Result<(), LinkError> {
        self.0.send(body).map_err(|_| LinkError::Closed)
    }
}
/// Running delivery task; dropping it closes its link and stops reconnect attempts.
pub struct ReliableTask(tokio::task::JoinHandle<Result<(), LinkError>>);
impl Drop for ReliableTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start shared ack tracking, reconnect/resend and bounded-ID deduplication.
/// A delivery is acknowledged after its consumer confirms application. The last
/// `dedup_capacity` IDs are retained; older duplicate IDs may be delivered again.
/// Delivery is at least once, unordered; durable consumers must be idempotent.
pub fn reliable(
    local: MachineId,
    connector: Box<dyn Connector>,
    dedup_capacity: usize,
) -> (
    ReliableHandle,
    mpsc::UnboundedReceiver<ReliableEvent>,
    ReliableTask,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (events, incoming) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(
        local,
        ReconnectingLink::new(connector),
        rx,
        events,
        dedup_capacity.max(1),
    ));
    (ReliableHandle(tx), incoming, ReliableTask(task))
}
async fn send(link: &mut dyn Link, envelope: &Envelope) -> Result<(), LinkError> {
    link.send(Bytes::from(
        serde_json::to_vec(envelope).map_err(|e| LinkError::Protocol(e.to_string()))?,
    ))
    .await
}
async fn run(
    local: MachineId,
    mut reconnect: ReconnectingLink,
    mut output: mpsc::UnboundedReceiver<Body>,
    events: mpsc::UnboundedSender<ReliableEvent>,
    capacity: usize,
) -> Result<(), LinkError> {
    let mut pending: VecDeque<Envelope> = VecDeque::new();
    let mut seen = HashSet::new();
    let mut order = VecDeque::new();
    let mut expected_peer = None;
    loop {
        let mut link = tokio::select! { link = reconnect.connect() => link, () = events.closed() => return Ok(()) };
        let peer = link.peer().clone();
        if expected_peer.as_ref().is_some_and(|p| p != &peer) {
            return Err(LinkError::Protocol(
                "reconnected to a different machine".into(),
            ));
        }
        expected_peer = Some(peer.clone());
        if events.send(ReliableEvent::Connected(peer.clone())).is_err() {
            return Ok(());
        }
        let result = connected(
            &local,
            &peer,
            &mut *link,
            &mut output,
            &events,
            &mut pending,
            &mut seen,
            &mut order,
            capacity,
        )
        .await;
        if events.send(ReliableEvent::Disconnected(peer)).is_err() {
            return Ok(());
        }
        match result {
            Err(LinkError::Closed) if output.is_closed() => return Ok(()),
            Err(error) => tracing::warn!(%error,"reliable link disconnected"),
            Ok(()) => return Ok(()),
        }
    }
}
// All connection-local activity runs in one task: cancellation of recv never
// discards a partial frame, and a replacement link always gets the entire outbox.
#[allow(clippy::too_many_arguments)]
async fn connected(
    local: &MachineId,
    peer: &MachineId,
    link: &mut dyn Link,
    output: &mut mpsc::UnboundedReceiver<Body>,
    events: &mpsc::UnboundedSender<ReliableEvent>,
    pending: &mut VecDeque<Envelope>,
    seen: &mut HashSet<Uuid>,
    order: &mut VecDeque<Uuid>,
    capacity: usize,
) -> Result<(), LinkError> {
    for envelope in pending.iter() {
        send(link, envelope).await?;
    }
    let mut retry = tokio::time::interval(std::time::Duration::from_secs(1));
    retry.tick().await;
    loop {
        tokio::select! {
            () = events.closed() => return Ok(()),
            body = output.recv() => {
                let Some(body) = body else { return Ok(()); };
                if matches!(body, Body::Ack { .. }) { return Err(LinkError::Protocol("acks are managed by reliable delivery".into())); }
                let envelope = Envelope { v: PROTOCOL_VERSION, id: Uuid::new_v4(), from: local.clone(), to: peer.clone(), body };
                // Validate frame size before retaining a permanently unsendable message.
                let frame = serde_json::to_vec(&envelope).map_err(|e| LinkError::Protocol(e.to_string()))?;
                if frame.len() > MAX_FRAME { return Err(LinkError::Protocol("envelope exceeds 1 MiB".into())); }
                pending.push_back(envelope.clone());
                send(link, &envelope).await?;
            }
            frame = link.recv() => {
                let envelope: Envelope = serde_json::from_slice(&frame?).map_err(|e| LinkError::Protocol(e.to_string()))?;
                envelope.check_version().map_err(|e| LinkError::Protocol(e.to_string()))?;
                if envelope.from != *peer || envelope.to != *local { return Err(LinkError::Protocol("envelope address does not match link".into())); }
                if let Body::Ack { of } = envelope.body { pending.retain(|e| e.id != of); continue; }
                let id = envelope.id;
                if !seen.contains(&id) {
                    let (tx, rx) = oneshot::channel();
                    events.send(ReliableEvent::Message(Box::new(Delivery { envelope, applied: tx }))).map_err(|_| LinkError::Closed)?;
                    if !rx.await.unwrap_or(false) { return Err(LinkError::Protocol("consumer did not apply envelope".into())); }
                    seen.insert(id); order.push_back(id);
                    while order.len() > capacity { if let Some(old) = order.pop_front() { seen.remove(&old); } }
                }
                send(link, &Envelope { v: PROTOCOL_VERSION, id: Uuid::new_v4(), from: local.clone(), to: peer.clone(), body: Body::Ack { of: id } }).await?;
            }
            _ = retry.tick() => { for envelope in pending.iter() { send(link, envelope).await?; } }
        }
    }
}
