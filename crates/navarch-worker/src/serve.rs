use crate::{Worker, WorkerError, WorkerStreams};
use navarch_proto::{Body, MachineId};
use navarch_transport::{Connector, ReliableEvent, reliable};
/// Connect the existing Worker runtime to a hub. Pending output survives hub
/// restarts; permission answers always use the M1 policy-first permission path.
/// Dropping this future stops its connection task.
pub async fn serve(
    worker: Worker,
    mut streams: WorkerStreams,
    local: MachineId,
    connector: Box<dyn Connector>,
) -> Result<(), WorkerError> {
    let (outbound, mut incoming, _connection) = reliable(local, connector, 4096);
    loop {
        tokio::select! {
            message = streams.messages.recv() => {
                let Some(message) = message else { return Ok(()); };
                let task_id = message.worker_task_id();
                if let Err(error) = outbound.send(message) {
                    if !matches!(error, navarch_transport::LinkError::Oversized { .. }) { return Err(error.into()); }
                    tracing::error!(%error, "dropping undeliverable worker event");
                    if let Some(task_id) = task_id {
                        outbound.send(Body::Failed { task_id, error: error.to_string() })?;
                    }
                }
            }
            Some(_) = streams.events.recv() => {},
            event = incoming.recv() => match event {
                Some(ReliableEvent::Connected(_)) => {
                    outbound.send(Body::ProbeReport { capabilities: worker.probe().await })?;
                }
                Some(ReliableEvent::Disconnected(_)) => {},
                Some(ReliableEvent::Message(delivery)) => {
                    // Rejections are emitted by Worker itself; they still count as
                    // applied commands and must not create another session on retry.
                    if worker.handle(delivery.envelope.body.clone()).await.is_err() {
                        tracing::warn!("worker refused hub command");
                    }
                    delivery.complete(true);
                }
                None => return Err(navarch_transport::LinkError::Closed.into()),
            }
        }
    }
}
