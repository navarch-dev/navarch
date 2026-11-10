//! Bounded TCP listener and connector sharing the relay's Noise protocol.
use crate::{
    channel::{ChannelIo, EncryptedLink, remember},
    relay_socket::{Guard, protocol},
    *,
};
use futures_util::{SinkExt, StreamExt};
use navarch_crypto::StaticKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
};
use tokio_util::codec::LengthDelimitedCodec;

const MAGIC: &[u8] = b"NAVARCH-DIRECT/1\n";
const WINDOW: Duration = Duration::from_secs(10);
/// Direct channel using the same nonce exchange, Noise handshake and record codec as relay.
pub type DirectLink = EncryptedLink;
#[derive(Serialize, Deserialize)]
struct Hello {
    machine: MachineId,
    nonce: [u8; 32],
}
struct TcpIo {
    output: mpsc::UnboundedSender<Vec<u8>>,
    input: mpsc::Receiver<Vec<u8>>,
    local: MachineId,
    peer: MachineId,
    nonce: Option<[u8; 32]>,
    _guard: Guard,
}
#[async_trait]
impl ChannelIo for TcpIo {
    fn send(&self, data: &[u8]) -> Result<(), LinkError> {
        self.output
            .send(data.to_vec())
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
    fn initial_nonce(&self) -> Option<[u8; 32]> {
        self.nonce
    }
}
fn framed(stream: TcpStream) -> tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec> {
    // LengthDelimitedCodec defaults to a u32 big-endian length prefix, as LocalLink does.
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME)
        .new_framed(stream)
}
fn io(
    framed: tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec>,
    local: MachineId,
    peer: MachineId,
    initial: Option<[u8; 32]>,
    first: Option<Vec<u8>>,
) -> TcpIo {
    let (mut write, mut read) = framed.split();
    let (output, mut frames) = mpsc::unbounded_channel::<Vec<u8>>();
    let (tx, input) = mpsc::channel(64);
    if let Some(first) = first {
        let _ = tx.try_send(first);
    }
    let writer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if frame.len() > MAX_FRAME || write.send(Bytes::from(frame)).await.is_err() {
                break;
            }
        }
    });
    let reader = tokio::spawn(async move {
        while let Some(Ok(frame)) = read.next().await {
            if tx.send(frame.to_vec()).await.is_err() {
                break;
            }
        }
    });
    TcpIo {
        output,
        input,
        local,
        peer,
        nonce: initial,
        _guard: Guard(vec![reader, writer]),
    }
}
/// Authenticated worker-side TCP connector.
#[derive(Clone)]
pub struct DirectConnector {
    /// Socket address supplied by the paired hub (no discovery).
    pub address: String,
    /// Worker machine ID.
    pub local: MachineId,
    /// Worker Noise static key.
    pub static_key: Arc<StaticKey>,
    /// Pinned hub machine ID.
    pub peer: MachineId,
    /// Pinned hub Noise static public key.
    pub peer_static: [u8; 32],
}
#[async_trait]
impl Connector for DirectConnector {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        tokio::time::timeout(WINDOW, async {
            let mut stream = TcpStream::connect(&self.address).await?;
            stream.write_all(MAGIC).await?;
            let mut nonce = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut nonce);
            let mut framed = framed(stream);
            framed
                .send(Bytes::from(
                    serde_json::to_vec(&Hello {
                        machine: self.local.clone(),
                        nonce,
                    })
                    .map_err(|_| protocol())?,
                ))
                .await?;
            let io = io(
                framed,
                self.local.clone(),
                self.peer.clone(),
                Some(nonce),
                None,
            );
            Ok(Box::new(
                DirectLink::handshake(
                    io,
                    &self.static_key,
                    || Some(self.peer_static),
                    true,
                    "direct",
                )
                .await?,
            ) as Box<dyn Link>)
        })
        .await
        .map_err(|_| protocol())?
    }
}
#[derive(Default)]
struct Attempts {
    sources: HashMap<IpAddr, std::collections::VecDeque<tokio::time::Instant>>,
}
impl Attempts {
    fn allow(&mut self, source: IpAddr) -> bool {
        let now = tokio::time::Instant::now();
        self.sources.retain(|_, times| {
            while times
                .front()
                .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(60))
            {
                times.pop_front();
            }
            !times.is_empty()
        });
        if !self.sources.contains_key(&source) && self.sources.len() >= 4096 {
            return false;
        }
        let times = self.sources.entry(source).or_default();
        if times.len() >= 10 {
            return false;
        }
        times.push_back(now);
        true
    }
}
struct Admission {
    attempts: Attempts,
    permits: Arc<Semaphore>,
}
impl Admission {
    fn new() -> Self {
        Self {
            attempts: Attempts::default(),
            permits: Arc::new(Semaphore::new(64)),
        }
    }
    fn admit(&mut self, source: IpAddr) -> Option<tokio::sync::OwnedSemaphorePermit> {
        if !self.attempts.allow(source) {
            return None;
        }
        self.permits.clone().try_acquire_owned().ok()
    }
}
// Keep the accept task alive across resource exhaustion and other transient errors.
async fn accept_with_retry<T, F, Fut>(mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    loop {
        match accept().await {
            Ok(connection) => return connection,
            Err(error) => {
                tracing::warn!(%error, "direct listener accept failed; retrying");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
/// Hub listener. Unknown and inactive IDs are silently dropped before Noise processing.
pub struct DirectListener {
    address: SocketAddr,
    input: mpsc::Receiver<(MachineId, DirectLink)>,
    _guard: Guard,
}
impl DirectListener {
    /// Bind explicitly, limiting unauthenticated handshakes to 64 and sources to 10/minute.
    pub async fn bind(
        address: &str,
        local: MachineId,
        key: Arc<StaticKey>,
        authorize: RelayAuthorization,
    ) -> Result<Self, LinkError> {
        let listener = TcpListener::bind(address).await?;
        let address = listener.local_addr()?;
        let (tx, input) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            let mut admission = Admission::new();
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (mut stream, source) = accept_with_retry(|| listener.accept()).await;
                let Some(permit) = admission.admit(source.ip()) else {
                    continue;
                };
                let local = local.clone();
                let key = key.clone();
                let authorize = authorize.clone();
                let tx = tx.clone();
                tasks.spawn(async move {
                    let result = tokio::time::timeout(WINDOW, async {
                        let mut magic = [0; MAGIC.len()];
                        stream.read_exact(&mut magic).await?;
                        if magic != MAGIC {
                            return Err(protocol());
                        }
                        let mut framed = framed(stream);
                        let first = framed.next().await.ok_or(LinkError::Closed)??;
                        let hello: Hello =
                            serde_json::from_slice(&first).map_err(|_| protocol())?;
                        let nonce = [vec![3], hello.nonce.to_vec()].concat();
                        if !remember(&hello.machine, &nonce) {
                            return Err(protocol());
                        }
                        let io = io(framed, local, hello.machine.clone(), None, Some(nonce));
                        let link = DirectLink::handshake(
                            io,
                            &key,
                            || authorize(&hello.machine),
                            false,
                            "direct",
                        )
                        .await?;
                        Ok::<_, LinkError>((hello.machine, link))
                    })
                    .await;
                    drop(permit);
                    if let Ok(Ok(value)) = result {
                        let _ = tx.try_send(value);
                    }
                });
                while tasks.try_join_next().is_some() {}
            }
        });
        Ok(Self {
            address,
            input,
            _guard: Guard(vec![task]),
        })
    }
    /// Address actually bound, including an OS-assigned port.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    /// Receive the next fully authenticated link.
    pub async fn accept(&mut self) -> Result<(MachineId, DirectLink), LinkError> {
        self.input.recv().await.ok_or(LinkError::Closed)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use navarch_crypto::{Identity, NoiseHandshake};
    #[tokio::test(start_paused = true)]
    async fn transient_accept_errors_back_off_and_keep_accepting() {
        let mut attempts = 0;
        let start = tokio::time::Instant::now();
        let accepted = accept_with_retry(|| {
            attempts += 1;
            std::future::ready(if attempts <= 2 {
                Err(std::io::Error::from_raw_os_error(24)) // EMFILE
            } else {
                Ok("connection")
            })
        })
        .await;
        assert_eq!(accepted, "connection");
        assert_eq!(attempts, 3);
        assert_eq!(start.elapsed(), Duration::from_millis(200));
    }
    async fn setup(authorize: bool) -> (DirectListener, DirectConnector) {
        let hi = Identity::generate();
        let wi = Identity::generate();
        let hk = Arc::new(StaticKey::generate());
        let wk = Arc::new(StaticKey::generate());
        let local = hi.machine_id().parse().unwrap();
        let peer: MachineId = wi.machine_id().parse().unwrap();
        let expected = peer.clone();
        let public = wk.public_key();
        let listener = DirectListener::bind(
            "127.0.0.1:0",
            local,
            hk.clone(),
            Arc::new(move |id| {
                if authorize && id == &expected {
                    Some(public)
                } else {
                    None
                }
            }),
        )
        .await
        .unwrap();
        let connector = DirectConnector {
            address: listener.local_addr().to_string(),
            local: peer,
            static_key: wk,
            peer: hi.machine_id().parse().unwrap(),
            peer_static: hk.public_key(),
        };
        (listener, connector)
    }
    #[tokio::test]
    async fn direct_handshake_chunked_maximum_frames_and_full_duplex() {
        let (mut listener, mut connector) = setup(true).await;
        let (worker, hub) = tokio::join!(connector.connect(), listener.accept());
        let mut worker = worker.unwrap();
        let (id, mut hub) = hub.unwrap();
        assert_eq!(id, connector.local);
        assert_eq!(worker.kind(), "direct");
        let (a, b) = tokio::join!(
            worker.send(vec![3; MAX_FRAME].into()),
            hub.send(vec![4; MAX_FRAME].into())
        );
        a.unwrap();
        b.unwrap();
        let (a, b) = tokio::join!(worker.recv(), hub.recv());
        assert_eq!(a.unwrap().as_ref(), vec![4; MAX_FRAME]);
        assert_eq!(b.unwrap().as_ref(), vec![3; MAX_FRAME]);
        assert!(worker.send(vec![0; MAX_FRAME + 1].into()).await.is_err());
    }
    async fn raw(
        address: SocketAddr,
        machine: MachineId,
        nonce: [u8; 32],
    ) -> tokio_util::codec::Framed<TcpStream, LengthDelimitedCodec> {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(MAGIC).await.unwrap();
        let mut io = framed(stream);
        io.send(Bytes::from(
            serde_json::to_vec(&Hello { machine, nonce }).unwrap(),
        ))
        .await
        .unwrap();
        io
    }
    #[tokio::test]
    async fn unauthorized_ids_have_same_nonce_and_failure_behavior_as_invalid_active_handshake() {
        // Unknown, pending and revoked all select None in the hub's authorization closure.
        for active in [false, true] {
            let (mut listener, connector) = setup(active).await;
            let mut io = raw(listener.local_addr(), connector.local.clone(), [7; 32]).await;
            let nonce = io.next().await.unwrap().unwrap();
            assert_eq!(nonce.len(), 33);
            assert_eq!(nonce[0], 4);
            // No premature EOF is allowed: that would distinguish paired IDs.
            assert!(
                tokio::time::timeout(Duration::from_millis(20), io.next())
                    .await
                    .is_err()
            );
            io.send(Bytes::from(vec![0; 49])).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), io.next())
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn revocation_during_nonce_exchange_is_checked_before_noise() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let hi = Identity::generate();
        let wi = Identity::generate();
        let hk = Arc::new(StaticKey::generate());
        let wk = StaticKey::generate();
        let hub: MachineId = hi.machine_id().parse().unwrap();
        let worker: MachineId = wi.machine_id().parse().unwrap();
        let expected = worker.clone();
        let worker_static = wk.public_key();
        let active = Arc::new(AtomicBool::new(true));
        let authorized = active.clone();
        let mut listener = DirectListener::bind(
            "127.0.0.1:0",
            hub.clone(),
            hk.clone(),
            Arc::new(move |machine| {
                (machine == &expected && authorized.load(Ordering::SeqCst)).then_some(worker_static)
            }),
        )
        .await
        .unwrap();
        let mut io = raw(listener.local_addr(), worker.clone(), [8; 32]).await;
        let challenge = io.next().await.unwrap().unwrap();
        let nonce: [u8; 32] = challenge[1..].try_into().unwrap();
        let prologue = crate::channel::prologue(&worker, &hub, &[8; 32], &nonce);
        let mut handshake =
            NoiseHandshake::with_prologue(true, &wk, &hk.public_key(), &prologue).unwrap();
        let first = [vec![0], handshake.write().unwrap()].concat();
        // This genuine first KK frame would authenticate if the listener reused
        // authorization captured when the clear hello arrived.
        active.store(false, Ordering::SeqCst);
        io.send(Bytes::from(first)).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), io.next())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn recorded_first_kk_message_fails_under_fresh_connection_nonces() {
        let (mut listener, connector) = setup(true).await;
        let mut io = raw(listener.local_addr(), connector.local.clone(), [5; 32]).await;
        let nonce = io.next().await.unwrap().unwrap();
        let hub_nonce: [u8; 32] = nonce[1..].try_into().unwrap();
        let prologue =
            crate::channel::prologue(&connector.local, &connector.peer, &[5; 32], &hub_nonce);
        let mut hs = NoiseHandshake::with_prologue(
            true,
            &connector.static_key,
            &connector.peer_static,
            &prologue,
        )
        .unwrap();
        let first = [vec![0], hs.write().unwrap()].concat();
        io.send(Bytes::from(first.clone())).await.unwrap();
        let response = io.next().await.unwrap().unwrap();
        hs.read(&response[1..]).unwrap();
        let _authenticated = listener.accept().await.unwrap();
        drop(io);
        let mut retry = raw(listener.local_addr(), connector.local, [6; 32]).await;
        let challenge = retry.next().await.unwrap().unwrap();
        assert_ne!(&challenge[1..], &hub_nonce);
        retry.send(Bytes::from(first)).await.unwrap();
        assert!(retry.next().await.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn wrong_magic_and_oversize_first_frame_are_dropped() {
        let (mut listener, connector) = setup(true).await;
        let mut bad = TcpStream::connect(&connector.address).await.unwrap();
        bad.write_all(b"NOT-A-DIRECT-LNK!").await.unwrap();
        assert!(bad.read_u8().await.is_err());
        let mut bad = TcpStream::connect(&connector.address).await.unwrap();
        bad.write_all(MAGIC).await.unwrap();
        bad.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(bad.read_u8().await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn deadline_includes_magic_and_hello() {
        let (listener, connector) = setup(true).await;
        let mut stream = TcpStream::connect(&connector.address).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(11)).await;
        assert!(stream.read_u8().await.is_err());
        drop(listener);
    }
    #[tokio::test(start_paused = true)]
    async fn source_limit_expires_and_tracking_memory_is_bounded() {
        let mut attempts = Attempts::default();
        let ip = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            assert!(attempts.allow(ip));
        }
        assert!(!attempts.allow(ip));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(attempts.allow(ip));
        for n in 0..4096u32 {
            attempts.allow(IpAddr::V4(std::net::Ipv4Addr::from(n + 0x0a000000)));
        }
        assert_eq!(attempts.sources.len(), 4096);
        assert!(!attempts.allow("192.0.2.1".parse().unwrap()));
    }
    #[test]
    fn unauthenticated_connection_cap_is_shared_across_source_addresses() {
        let mut admission = Admission::new();
        let mut pending = Vec::new();
        for n in 1..=64 {
            pending.push(
                admission
                    .admit(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, n)))
                    .unwrap(),
            );
        }
        assert!(admission.admit("192.0.2.1".parse().unwrap()).is_none());
        pending.pop();
        assert!(admission.admit("192.0.2.1".parse().unwrap()).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn source_budget_is_rolling_across_the_first_attempts_expiry() {
        let mut attempts = Attempts::default();
        let ip = "127.0.0.1".parse().unwrap();
        assert!(attempts.allow(ip));
        tokio::time::advance(Duration::from_secs(59)).await;
        for _ in 0..9 {
            assert!(attempts.allow(ip));
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(attempts.allow(ip));
        assert!(!attempts.allow(ip));
    }
}
