use crate::*;
use tokio::sync::mpsc;
/// Cancellation-safe in-memory frame transport for offline tests.
pub struct MemoryLink {
    peer: MachineId,
    tx: mpsc::UnboundedSender<Bytes>,
    rx: mpsc::UnboundedReceiver<Bytes>,
}
/// Pair links with each other's machine addresses.
pub fn memory_pair(a: MachineId, b: MachineId) -> (MemoryLink, MemoryLink) {
    let (at, ar) = mpsc::unbounded_channel();
    let (bt, br) = mpsc::unbounded_channel();
    (
        MemoryLink {
            peer: b,
            tx: at,
            rx: br,
        },
        MemoryLink {
            peer: a,
            tx: bt,
            rx: ar,
        },
    )
}
#[async_trait]
impl Link for MemoryLink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if frame.len() > MAX_FRAME {
            return Err(LinkError::Protocol("frame exceeds 1 MiB".into()));
        }
        self.tx.send(frame).map_err(|_| LinkError::Closed)
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        self.rx.recv().await.ok_or(LinkError::Closed)
    }
    fn peer(&self) -> &MachineId {
        &self.peer
    }
}
