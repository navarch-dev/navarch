use crate::channel::{ChannelIo, EncryptedLink, remember};
use crate::{
    relay_socket::{Guard, decode, encode, protocol},
    *,
};
use navarch_crypto::{Identity, StaticKey};
/// Relay channel using the shared Noise and record implementation.
pub type RelayLink = EncryptedLink;
use navarch_proto::relay::{RelayRequest, RelayResponse};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;

struct PeerIo {
    output: mpsc::UnboundedSender<RelayRequest>,
    input: mpsc::Receiver<Vec<u8>>,
    peer: MachineId,
    local: MachineId,
    _guard: Arc<Guard>,
}
#[async_trait]
impl ChannelIo for PeerIo {
    fn send(&self, data: &[u8]) -> Result<(), LinkError> {
        self.output
            .send(RelayRequest::Route {
                to: self.peer.clone(),
                payload: encode(data),
            })
            .map_err(|_| LinkError::Closed)
    }
    async fn recv(&mut self) -> Result<Vec<u8>, LinkError> {
        self.input.recv().await.ok_or(LinkError::Closed)
    }
    fn peer(&self) -> &MachineId {
        &self.peer
    }
    fn local(&self) -> &MachineId {
        &self.local
    }
}
/// Worker connector. Each replacement connection authenticates again and runs fresh KK.
pub struct RelayConnector {
    /// WebSocket endpoint ending in /v1/ws.
    pub url: String,
    /// Local Ed25519 identity.
    pub identity: Arc<Identity>,
    /// Local X25519 key.
    pub static_key: Arc<StaticKey>,
    /// Paired hub ID.
    pub peer: MachineId,
    /// Pinned hub Noise public key.
    pub peer_static: [u8; 32],
}
#[async_trait]
impl Connector for RelayConnector {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        let socket = RelaySocket::connect(&self.url, &self.identity).await?;
        let (output, mut incoming, guard) = socket.spawn();
        let (tx, input) = mpsc::channel(64);
        let peer = self.peer.clone();
        let router = tokio::spawn(async move {
            while let Some(Ok(message)) = incoming.recv().await {
                match message {
                    RelayResponse::Routed { from, payload } if from == peer => {
                        let Ok(data) = decode(&payload) else {
                            break;
                        };
                        if tx.send(data).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        // spawn() returns the sole guard owner; extend it with the router so
        // dropping a link cancels all three tasks without a sentinel task.
        let mut guard = Arc::try_unwrap(guard).map_err(|_| protocol())?;
        guard.0.push(router);
        let guard = Arc::new(guard);
        let io = PeerIo {
            output,
            input,
            peer: self.peer.clone(),
            local: self.identity.machine_id().parse().map_err(|_| protocol())?,
            _guard: guard,
        };
        Ok(Box::new(
            RelayLink::handshake(
                io,
                &self.static_key,
                || Some(self.peer_static),
                true,
                "relay",
            )
            .await?,
        ))
    }
}
/// Authorization called before every routed frame, including handshakes and acks.
pub type RelayAuthorization = Arc<dyn Fn(&MachineId) -> Option<[u8; 32]> + Send + Sync>;
/// Multiplexes one hub relay identity into separately authenticated Noise links.
pub struct RelayListener {
    input: mpsc::Receiver<(MachineId, RelayLink)>,
    _task: Guard,
}
impl RelayListener {
    /// Connect once. The daemon recreates this listener after relay disconnection.
    pub async fn connect(
        url: &str,
        identity: &Identity,
        key: Arc<StaticKey>,
        authorize: RelayAuthorization,
    ) -> Result<Self, LinkError> {
        let socket = RelaySocket::connect(url, identity).await?;
        let local = identity
            .machine_id()
            .parse::<MachineId>()
            .map_err(|_| protocol())?;
        let (output, mut incoming, guard) = socket.spawn();
        let (tx, input) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            let _keep = guard;
            let mut peers: HashMap<MachineId, mpsc::Sender<Vec<u8>>> = HashMap::new();
            let mut tasks = tokio::task::JoinSet::new();
            while let Some(Ok(RelayResponse::Routed { from, payload })) = incoming.recv().await {
                let Some(remote) = authorize(&from) else {
                    peers.remove(&from);
                    continue;
                };
                let Ok(data) = decode(&payload) else {
                    peers.remove(&from);
                    continue;
                };
                if data.first() == Some(&3) {
                    if data.len() != 33 || !remember(&from, &data) {
                        peers.remove(&from);
                        continue;
                    }
                    let (frames, rx) = mpsc::channel(64);
                    let _ = frames.try_send(data);
                    peers.insert(from.clone(), frames);
                    let io = PeerIo {
                        output: output.clone(),
                        input: rx,
                        peer: from.clone(),
                        local: local.clone(),
                        _guard: Arc::new(Guard(vec![])),
                    };
                    let key = key.clone();
                    let tx = tx.clone();
                    tasks.spawn(async move {
                        if let Ok(link) =
                            RelayLink::handshake(io, &key, || Some(remote), false, "relay").await
                        {
                            let _ = tx.send((from, link)).await;
                        }
                    });
                } else if let Some(peer) = peers.get(&from)
                    && peer.try_send(data).is_err()
                {
                    peers.remove(&from);
                }
                while tasks.try_join_next().is_some() {}
            }
        });
        Ok(Self {
            input,
            _task: Guard(vec![task]),
        })
    }
    /// Wait for an active peer's completed Noise handshake.
    pub async fn accept(&mut self) -> Result<(MachineId, RelayLink), LinkError> {
        self.input.recv().await.ok_or(LinkError::Closed)
    }
}
