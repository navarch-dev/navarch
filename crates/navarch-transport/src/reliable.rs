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
    /// Validate the encoded envelope before any caller persists a task or queues it.
    pub fn validate(&self, body: &Body) -> Result<(), LinkError> {
        if matches!(body, Body::Ack { .. }) {
            return Err(LinkError::Protocol(
                "acks are managed by reliable delivery".into(),
            ));
        }
        let size = navarch_proto::encoded_envelope_len(body)
            .map_err(|e| LinkError::Protocol(e.to_string()))?;
        if size > MAX_FRAME {
            return Err(LinkError::Oversized {
                size,
                limit: MAX_FRAME,
            });
        }
        Ok(())
    }
    /// Queue a body. Envelope IDs remain stable through retries.
    pub fn send(&self, body: Body) -> Result<(), LinkError> {
        self.validate(&body)?;
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
    resend(link, pending).await?;
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
                if frame.len() > MAX_FRAME {
                    tracing::error!(size = frame.len(), "dropping oversized queued envelope");
                    if let Some(task_id) = envelope.body.worker_task_id() {
                        let failed = Envelope { body: Body::Failed { task_id, error: "Worker event exceeded the 1 MiB envelope limit".into() }, id: Uuid::new_v4(), ..envelope };
                        pending.push_back(failed.clone());
                        send(link, &failed).await?;
                    }
                    continue;
                }
                pending.push_back(envelope.clone());
                send(link, &envelope).await?;
            }
            frame = link.recv() => {
                let envelope: Envelope = serde_json::from_slice(&frame?).map_err(|_| LinkError::Protocol("invalid envelope".into()))?;
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
            _ = retry.tick() => { resend(link, pending).await?; }
        }
    }
}

// Older senders may have retained an envelope that current framing cannot carry.
// Remove it before I/O, so a reconnect can never resurrect a poison entry.
async fn resend(link: &mut dyn Link, pending: &mut VecDeque<Envelope>) -> Result<(), LinkError> {
    let mut cleaned = VecDeque::new();
    while let Some(mut envelope) = pending.pop_front() {
        let size = serde_json::to_vec(&envelope)
            .map_err(|e| LinkError::Protocol(e.to_string()))?
            .len();
        if size > MAX_FRAME {
            tracing::error!(id = %envelope.id, size, "dropping oversized outbox envelope");
            let Some(task_id) = envelope.body.worker_task_id() else {
                continue;
            };
            envelope.id = Uuid::new_v4();
            envelope.body = Body::Failed {
                task_id,
                error: "Worker event exceeded the 1 MiB envelope limit".into(),
            };
        }
        cleaned.push_back(envelope);
    }
    *pending = cleaned;
    for envelope in pending.iter() {
        send(link, envelope).await?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn legacy_oversize_outbox_is_replaced_by_failed_and_survives_reconnect() {
        let a: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let b: MachineId = "77777777777777777777777774".parse().unwrap();
        let task_id = navarch_proto::TaskId::new();
        let bad = Envelope {
            v: 1,
            id: Uuid::new_v4(),
            from: a.clone(),
            to: b.clone(),
            body: Body::Finished {
                task_id,
                session_id: "s".into(),
                stop_reason: "end_turn".into(),
                result: "x".repeat(MAX_FRAME),
            },
        };
        let mut pending = VecDeque::from([
            bad.clone(),
            Envelope {
                id: Uuid::new_v4(),
                body: Body::Dispatch {
                    task_id,
                    agent: "fake".into(),
                    cwd: "/".into(),
                    prompt: "x".repeat(MAX_FRAME),
                    mode: None,
                    model: None,
                    mcp_servers: None,
                },
                ..bad.clone()
            },
        ]);
        let (mut link, mut peer) = memory_pair(a.clone(), b.clone());
        resend(&mut link, &mut pending).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].id, bad.id);
        assert!(matches!(pending[0].body, Body::Failed { task_id: id, .. } if id == task_id));
        let delivered: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
        assert_eq!(delivered, pending[0]);
        // The outbox now contains only a deliverable failure with a stable retry ID.
        let (mut replacement, mut peer) = memory_pair(a, b);
        resend(&mut replacement, &mut pending).await.unwrap();
        let retry: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
        assert_eq!(retry, delivered);
    }
}
