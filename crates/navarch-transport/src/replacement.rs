//! Single-slot authenticated link replacement preserving the reliable outbox.
use crate::*;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
struct Slot {
    link: Mutex<Option<Box<dyn Link>>>,
    changed: watch::Sender<u64>,
}
/// Sender of authenticated replacement links. Only the newest waiting link is retained.
#[derive(Clone)]
pub struct ReplacementSender(Arc<Slot>);
/// Connector that interrupts an older live link when a replacement arrives.
pub struct ReplacementConnector {
    slot: Arc<Slot>,
    changed: watch::Receiver<u64>,
}
/// Create a bounded, single-slot link replacement channel.
pub fn replacement_channel() -> (ReplacementSender, ReplacementConnector) {
    let (changed, rx) = watch::channel(0);
    let slot = Arc::new(Slot {
        link: Mutex::new(None),
        changed,
    });
    (
        ReplacementSender(slot.clone()),
        ReplacementConnector { slot, changed: rx },
    )
}
impl ReplacementSender {
    /// Replace any waiting link and wake the reliable connection immediately.
    pub fn replace(&self, link: Box<dyn Link>) -> Result<(), LinkError> {
        let mut slot = self.0.link.lock().map_err(|_| LinkError::Closed)?;
        *slot = Some(link);
        // Publish the link and its generation under one lock. Otherwise a
        // connector could take the new link before this notification, causing
        // that link to invalidate itself instead of replacing the previous one.
        self.0.changed.send_modify(|v| *v = v.wrapping_add(1));
        Ok(())
    }
    /// Whether the connection task has stopped.
    pub fn is_closed(&self) -> bool {
        self.0.changed.receiver_count() == 0
    }
}
#[async_trait]
impl Connector for ReplacementConnector {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        loop {
            {
                let mut slot = self.slot.link.lock().map_err(|_| LinkError::Closed)?;
                self.changed.borrow_and_update();
                if let Some(link) = slot.take() {
                    return Ok(Box::new(Replaceable {
                        inner: link,
                        changed: self.changed.clone(),
                    }));
                }
            }
            self.changed
                .changed()
                .await
                .map_err(|_| LinkError::Closed)?;
        }
    }
}
struct Replaceable {
    inner: Box<dyn Link>,
    changed: watch::Receiver<u64>,
}
#[async_trait]
impl Link for Replaceable {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        self.inner.send(frame).await
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        tokio::select! { result=self.inner.recv()=>result, _=self.changed.changed()=>Err(LinkError::Closed) }
    }
    fn peer(&self) -> &MachineId {
        self.inner.peer()
    }
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn newest_authenticated_link_replaces_live_link_without_self_invalidating() {
        let a: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let b: MachineId = "77777777777777777777777774".parse().unwrap();
        let (sender, mut connector) = replacement_channel();
        let (first, _) = memory_pair(a.clone(), b.clone());
        sender.replace(Box::new(first)).unwrap();
        let mut link = connector.connect().await.unwrap();
        for n in 0..1000u32 {
            let (next, mut peer) = memory_pair(a.clone(), b.clone());
            sender.replace(Box::new(next)).unwrap();
            assert!(link.recv().await.is_err());
            link = connector.connect().await.unwrap();
            peer.send(n.to_be_bytes().to_vec().into()).await.unwrap();
            assert_eq!(link.recv().await.unwrap().as_ref(), n.to_be_bytes());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrently_published_link_does_not_observe_its_own_replacement() {
        let a: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let b: MachineId = "77777777777777777777777774".parse().unwrap();
        let (sender, mut connector) = replacement_channel();
        for n in 0..1000u32 {
            let (next, mut peer) = memory_pair(a.clone(), b.clone());
            let sender = sender.clone();
            let publishing = tokio::spawn(async move { sender.replace(Box::new(next)).unwrap() });
            let mut link = connector.connect().await.unwrap();
            publishing.await.unwrap();
            // No later replacement exists, so this receive must see the frame
            // even when publication and generation observation were concurrent.
            peer.send(n.to_be_bytes().to_vec().into()).await.unwrap();
            assert_eq!(link.recv().await.unwrap().as_ref(), n.to_be_bytes());
        }
    }
}
