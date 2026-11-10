use crate::{Hub, HubError};
use navarch_crypto::{Identity, PairInfo, StaticKey};
use navarch_proto::MachineId;
use navarch_store::{Machine, MachineState};
use navarch_transport::{Link, LinkError, RelayListener};
use rand::Rng;
use std::{sync::Arc, time::Duration};

/// Relay identity and endpoint configured by CLI wiring.
pub struct RemoteRelay {
    /// WebSocket URL.
    pub url: String,
    /// Ed25519 identity.
    pub identity: Arc<Identity>,
    /// X25519 static secret.
    pub static_key: Arc<StaticKey>,
}
impl Hub {
    /// Persist an authenticated introduction as pending. Never replace a known
    /// key or resurrect a revoked machine through another pairing attempt.
    pub fn register_paired(&self, peer: PairInfo) -> Result<MachineId, HubError> {
        let _state = self.state()?;
        self.put_paired(peer)
    }
    pub(crate) fn put_paired(&self, peer: PairInfo) -> Result<MachineId, HubError> {
        peer.validate()
            .map_err(|_| HubError::Invalid("invalid paired introduction".into()))?;
        let id: MachineId = navarch_crypto::machine_id(&peer.identity)
            .parse()
            .map_err(|_| HubError::Invalid("invalid paired identity".into()))?;
        if self.store.machines()?.iter().any(|m| m.id == id) {
            return Err(HubError::Invalid(
                "machine already known; pairing cannot replace its keys".into(),
            ));
        }
        self.store.put_machine(Machine {
            id: id.clone(),
            public_key: peer.identity,
            static_public_key: Some(peer.static_key),
            display_name: peer.display_name,
            state: MachineState::Pending,
            capabilities: None,
        })?;
        self.publish_inbox(navarch_store::InboxEvent {
            id: uuid::Uuid::new_v4(),
            envelope: None,
            envelope_omitted: false,
            event: navarch_store::HubEvent {
                seq: 0,
                kind: "machine_pending".into(),
                card_id: None,
                step: None,
                machine: id.clone(),
                summary: "New paired machine awaits approval".into(),
                time: crate::runtime::timestamp() / 1_000_000,
            },
        })?;
        Ok(id)
    }
    pub(crate) fn require_active(&self, id: &MachineId) -> Result<(), HubError> {
        if self.machine(id)?.state != MachineState::Active {
            return Err(HubError::Invalid("inactive machine".into()));
        }
        Ok(())
    }
    /// Maintain the hub's one relay connection and feed replacements to reliable
    /// delivery. Only known active keys reach a Noise responder.
    pub async fn serve_relay(&self, local: MachineId, remote: RemoteRelay) -> Result<(), HubError> {
        let mut tasks = tokio::task::JoinSet::new();
        let mut delay = 1;
        loop {
            let hub = self.clone();
            let authorize = Arc::new(move |id: &MachineId| {
                hub.machine(id)
                    .ok()
                    .filter(|m| m.state == MachineState::Active)
                    .and_then(|m| m.static_public_key)
            });
            if let Ok(mut listener) = RelayListener::connect(
                &remote.url,
                &remote.identity,
                remote.static_key.clone(),
                authorize,
            )
            .await
            {
                delay = 1;
                while let Ok((id, link)) = listener.accept().await {
                    if self.require_active(&id).is_err() {
                        continue;
                    }
                    self.admit_link(local.clone(), id, Box::new(link), &mut tasks)?;
                    while tasks.try_join_next().is_some() {}
                }
            }
            let millis = rand::rngs::OsRng.gen_range(delay * 1000..=(delay * 1500).min(60000));
            tokio::time::sleep(Duration::from_millis(millis)).await;
            delay = (delay * 2).min(60);
        }
    }
}
pub(crate) struct ActiveConnector {
    pub hub: Hub,
    pub inner: Box<dyn navarch_transport::Connector>,
}
#[async_trait::async_trait]
impl navarch_transport::Connector for ActiveConnector {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        let link = self.inner.connect().await?;
        let link = ActiveLink {
            hub: self.hub.clone(),
            inner: link,
        };
        link.active()?;
        Ok(Box::new(link))
    }
}
struct ActiveLink {
    hub: Hub,
    inner: Box<dyn Link>,
}
impl ActiveLink {
    fn active(&self) -> Result<(), LinkError> {
        self.hub
            .require_active(self.inner.peer())
            .map_err(|_| LinkError::Protocol("inactive machine".into()))
    }
}
#[async_trait::async_trait]
impl Link for ActiveLink {
    async fn send(&mut self, frame: bytes::Bytes) -> Result<(), LinkError> {
        self.active()?;
        self.inner.send(frame).await
    }
    async fn recv(&mut self) -> Result<bytes::Bytes, LinkError> {
        self.active()?;
        let frame = self.inner.recv().await?;
        self.active()?;
        Ok(frame)
    }
    fn peer(&self) -> &MachineId {
        self.inner.peer()
    }
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
}
