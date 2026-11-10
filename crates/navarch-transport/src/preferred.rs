//! Authenticated endpoint persistence and relay-to-direct migration.
use crate::*;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Endpoints {
    addresses: Vec<String>,
    path: Option<PathBuf>,
}
/// The last authenticated hub endpoints, shared by a connector and reliable receiver.
#[derive(Clone, Default)]
pub struct EndpointState(Arc<Mutex<Endpoints>>, Arc<tokio::sync::Notify>);
impl EndpointState {
    /// Load a worker-owned endpoint state file; a missing file means no known addresses.
    pub fn load(path: &Path) -> Result<Self, LinkError> {
        let addresses = match std::fs::read(path) {
            Ok(bytes) if bytes.len() <= 1024 => serde_json::from_slice::<Vec<String>>(&bytes)
                .map_err(|_| LinkError::Protocol("invalid endpoint state".into()))?,
            Ok(_) => return Err(LinkError::Protocol("oversize endpoint state".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        navarch_proto::validate_direct_endpoints(&addresses)
            .map_err(|e| LinkError::Protocol(e.into()))?;
        Ok(Self(
            Arc::new(Mutex::new(Endpoints {
                addresses,
                path: Some(path.to_owned()),
            })),
            Arc::default(),
        ))
    }
    /// Return the most recently authenticated addresses.
    pub fn addresses(&self) -> Result<Vec<String>, LinkError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| LinkError::Closed)?
            .addresses
            .clone())
    }
    async fn wait_for_retry(&self) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(300)) => {},
            _ = self.1.notified() => {},
        }
    }
    /// Validate and atomically persist endpoints received inside the Noise channel.
    /// Wake the relay upgrade task only when at least one address is newly learned.
    pub fn update(&self, addresses: Vec<String>) -> Result<(), LinkError> {
        navarch_proto::validate_direct_endpoints(&addresses)
            .map_err(|e| LinkError::Protocol(e.into()))?;
        let mut state = self.0.lock().map_err(|_| LinkError::Closed)?;
        if state.addresses == addresses {
            return Ok(());
        }
        if let Some(path) = &state.path {
            let parent = path
                .parent()
                .ok_or_else(|| LinkError::Protocol("missing endpoint parent".into()))?;
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(
                &serde_json::to_vec(&addresses)
                    .map_err(|_| LinkError::Protocol("invalid endpoints".into()))?,
            )?;
            temp.as_file().sync_all()?;
            temp.persist(path).map_err(|e| LinkError::Io(e.error))?;
        }
        let added = addresses
            .iter()
            .any(|address| !state.addresses.contains(address));
        state.addresses = addresses;
        if added {
            self.1.notify_one();
        }
        Ok(())
    }
}
/// Worker connector that prefers up to three direct addresses, then uses the relay.
pub struct PreferredConnector {
    /// Existing authenticated relay connector.
    pub relay: RelayConnector,
    /// Authenticated endpoint state, persisted in the worker directory.
    pub endpoints: EndpointState,
    /// Whether direct connections and upgrades are enabled.
    pub direct: bool,
    replacement: Arc<Mutex<Option<Box<dyn Link>>>>,
}
impl PreferredConnector {
    /// Construct the reconnect and live-upgrade path.
    pub fn new(relay: RelayConnector, endpoints: EndpointState, direct: bool) -> Self {
        Self {
            relay,
            endpoints,
            direct,
            replacement: Default::default(),
        }
    }
    fn direct_connector(&self) -> Result<DirectConnector, LinkError> {
        Ok(DirectConnector {
            address: String::new(),
            local: self
                .relay
                .identity
                .machine_id()
                .parse()
                .map_err(|_| LinkError::Protocol("invalid identity".into()))?,
            static_key: self.relay.static_key.clone(),
            peer: self.relay.peer.clone(),
            peer_static: self.relay.peer_static,
        })
    }
}
async fn try_direct(
    mut connector: DirectConnector,
    endpoints: &EndpointState,
) -> Result<Box<dyn Link>, LinkError> {
    for address in endpoints.addresses()?.into_iter().take(3) {
        connector.address = address;
        // The two-second budget includes authentication: a listener accepting
        // TCP but never answering must not prevent relay fallback.
        if let Ok(Ok(link)) =
            tokio::time::timeout(Duration::from_secs(2), connector.connect()).await
        {
            return Ok(link);
        }
    }
    Err(LinkError::Closed)
}
#[async_trait]
impl Connector for PreferredConnector {
    fn endpoint_state(&self) -> Option<EndpointState> {
        Some(self.endpoints.clone())
    }
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        if let Some(link) = self
            .replacement
            .lock()
            .map_err(|_| LinkError::Closed)?
            .take()
        {
            return Ok(link);
        }
        let direct = self.direct_connector()?;
        if self.direct
            && let Ok(link) = try_direct(direct.clone(), &self.endpoints).await
        {
            return Ok(link);
        }
        let link = self.relay.connect().await?;
        if !self.direct {
            return Ok(link);
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let endpoints = self.endpoints.clone();
        let task = tokio::spawn(async move {
            loop {
                endpoints.wait_for_retry().await;
                if let Ok(link) = try_direct(direct.clone(), &endpoints).await {
                    let _ = tx.send(link);
                    break;
                }
            }
        });
        Ok(Box::new(Upgradable {
            inner: link,
            upgrade: rx,
            replacement: self.replacement.clone(),
            task,
        }))
    }
}
struct Upgradable {
    inner: Box<dyn Link>,
    upgrade: tokio::sync::oneshot::Receiver<Box<dyn Link>>,
    replacement: Arc<Mutex<Option<Box<dyn Link>>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Upgradable {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[async_trait]
impl Link for Upgradable {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        self.inner.send(frame).await
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        tokio::select! {
            result=self.inner.recv()=>result,
            result=&mut self.upgrade=>{if let Ok(link)=result {*self.replacement.lock().map_err(|_|LinkError::Closed)?=Some(link);}Err(LinkError::Closed)}
        }
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

    #[tokio::test(start_paused = true)]
    async fn new_addresses_wake_retry_and_periodic_retry_remains_afterwards() {
        let state = EndpointState::default();
        let first = vec!["127.0.0.1:7391".into()];
        state.update(first.clone()).unwrap();
        // A permit is retained even if the upgrade task hasn't started waiting yet.
        let now = tokio::time::Instant::now();
        state.wait_for_retry().await;
        assert_eq!(now.elapsed(), Duration::ZERO);
        state.update(first.clone()).unwrap();
        let now = tokio::time::Instant::now();
        state.wait_for_retry().await;
        assert_eq!(now.elapsed(), Duration::from_secs(300));
        state
            .update(vec!["127.0.0.1:7392".into(), first[0].clone()])
            .unwrap();
        let now = tokio::time::Instant::now();
        state.wait_for_retry().await;
        assert_eq!(now.elapsed(), Duration::ZERO);
        // Removing an address does not trigger an immediate attempt.
        state.update(first).unwrap();
        let now = tokio::time::Instant::now();
        state.wait_for_retry().await;
        assert_eq!(now.elapsed(), Duration::from_secs(300));
    }
}
