use crate::{
    relay_socket::{Guard, decode, encode, protocol},
    *,
};
use navarch_crypto::{Identity, NoiseChannel, NoiseHandshake, StaticKey};
use navarch_proto::relay::{RelayRequest, RelayResponse};
use rand::RngCore;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;

struct PeerIo {
    output: mpsc::UnboundedSender<RelayRequest>,
    input: mpsc::Receiver<Vec<u8>>,
    peer: MachineId,
    local: MachineId,
    _guard: Arc<Guard>,
}
impl PeerIo {
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
}
/// End-to-end Noise link, with background I/O for cancellation-safe full duplex.
pub struct RelayLink {
    peer: MachineId,
    output: mpsc::UnboundedSender<Bytes>,
    input: mpsc::Receiver<Result<Bytes, LinkError>>,
    _task: Guard,
}
impl RelayLink {
    async fn handshake(
        mut io: PeerIo,
        key: &StaticKey,
        remote: &[u8; 32],
        initiator: bool,
    ) -> Result<Self, LinkError> {
        let channel = tokio::time::timeout(HANDSHAKE_WINDOW, async {
            // A fresh responder nonce prevents replayed KK first messages from
            // creating an authenticated link, even after process restart.
            let mut own = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut own);
            let (worker_nonce, hub_nonce) = if initiator {
                io.send(&[vec![3], own.to_vec()].concat())?;
                (own, nonce(&io.recv().await?, 4)?)
            } else {
                let worker = nonce(&io.recv().await?, 3)?;
                io.send(&[vec![4], own.to_vec()].concat())?;
                (worker, own)
            };
            let (worker, hub) = if initiator {
                (&io.local, &io.peer)
            } else {
                (&io.peer, &io.local)
            };
            let prologue = prologue(worker, hub, &worker_nonce, &hub_nonce);
            let mut hs = NoiseHandshake::with_prologue(initiator, key, remote, &prologue)
                .map_err(|_| protocol())?;
            if initiator {
                io.send(&[vec![0], hs.write().map_err(|_| protocol())?].concat())?;
            }
            let frame = io.recv().await?;
            if frame.first() != Some(&u8::from(initiator)) {
                return Err(protocol());
            }
            hs.read(&frame[1..]).map_err(|_| protocol())?;
            if !initiator {
                io.send(&[vec![1], hs.write().map_err(|_| protocol())?].concat())?;
            }
            hs.finish().map_err(|_| protocol())
        })
        .await
        .map_err(|_| protocol())??;
        let peer = io.peer.clone();
        let (output, rx) = mpsc::unbounded_channel();
        let (tx, input) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            if let Err(e) = run(io, channel, rx, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });
        Ok(Self {
            peer,
            output,
            input,
            _task: Guard(vec![task]),
        })
    }
}
#[async_trait]
impl Link for RelayLink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if frame.len() > MAX_FRAME {
            return Err(protocol());
        }
        self.output.send(frame).map_err(|_| LinkError::Closed)
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        self.input.recv().await.ok_or(LinkError::Closed)?
    }
    fn peer(&self) -> &MachineId {
        &self.peer
    }
}
fn prologue(
    worker: &MachineId,
    hub: &MachineId,
    worker_nonce: &[u8; 32],
    hub_nonce: &[u8; 32],
) -> Vec<u8> {
    [
        b"navarch-channel-v1\0worker\0hub\0".as_slice(),
        worker.to_string().as_bytes(),
        hub.to_string().as_bytes(),
        worker_nonce,
        hub_nonce,
    ]
    .concat()
}
fn nonce(frame: &[u8], kind: u8) -> Result<[u8; 32], LinkError> {
    if frame.len() != 33 || frame[0] != kind {
        return Err(protocol());
    }
    frame[1..].try_into().map_err(|_| protocol())
}
const CHUNK: usize = 48 * 1024;
async fn run(
    mut io: PeerIo,
    mut channel: NoiseChannel,
    mut output: mpsc::UnboundedReceiver<Bytes>,
    input: &mpsc::Sender<Result<Bytes, LinkError>>,
) -> Result<(), LinkError> {
    let mut assembly = Assembly::default();
    loop {
        tokio::select! {
            frame=output.recv()=>{
                let frame=frame.ok_or(LinkError::Closed)?;
                // Empty Link frames are represented by a header-only Noise record.
                let chunks=frame.len().max(1).div_ceil(CHUNK);
                for index in 0..chunks {
                    let offset=index*CHUNK; let end=(offset+CHUNK).min(frame.len());
                    let plain=[(frame.len() as u32).to_be_bytes().as_slice(),(offset as u32).to_be_bytes().as_slice(),&frame[offset..end]].concat();
                    let cipher=channel.seal(&plain).map_err(|_|protocol())?;
                    io.send(&[vec![2],cipher].concat())?;
                }
            },
            cipher=io.recv()=>{
                let cipher=cipher?;if cipher.first()!=Some(&2){return Err(protocol());}
                let plain=channel.open(&cipher[1..]).map_err(|_|protocol())?;
                if let Some(frame)=assembly.push(&plain)? {input.send(Ok(Bytes::from(frame))).await.map_err(|_|LinkError::Closed)?;}
            },
        }
    }
}
#[derive(Default)]
struct Assembly {
    total: Option<usize>,
    bytes: Vec<u8>,
}
impl Assembly {
    fn push(&mut self, plain: &[u8]) -> Result<Option<Vec<u8>>, LinkError> {
        if plain.len() < 8 || plain.len() > CHUNK + 8 {
            return Err(protocol());
        }
        let total = u32::from_be_bytes(plain[..4].try_into().map_err(|_| protocol())?) as usize;
        let offset = u32::from_be_bytes(plain[4..8].try_into().map_err(|_| protocol())?) as usize;
        let data = &plain[8..];
        if total > MAX_FRAME
            || offset != self.bytes.len()
            || self.total.is_some_and(|n| n != total)
            || offset + data.len() > total
            || (total > 0 && data.is_empty())
            || (offset + data.len() < total && data.len() != CHUNK)
        {
            return Err(protocol());
        }
        self.total = Some(total);
        self.bytes.extend_from_slice(data);
        if self.bytes.len() == total {
            self.total = None;
            Ok(Some(std::mem::take(&mut self.bytes)))
        } else {
            Ok(None)
        }
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
            RelayLink::handshake(io, &self.static_key, &self.peer_static, true).await?,
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
                        if let Ok(link) = RelayLink::handshake(io, &key, &remote, false).await {
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

// This cache is an extra short-term replay guard. Fresh two-sided prologues
// authenticate every handshake independently of cache eviction or process restart.
const HANDSHAKE_WINDOW: Duration = Duration::from_secs(10);
const HISTORY_CAP: usize = 65536;
type NonceKey = (MachineId, [u8; 32]);
#[derive(Default)]
struct History {
    seen: HashSet<NonceKey>,
    oldest: VecDeque<(tokio::time::Instant, NonceKey)>,
}
impl History {
    fn remember(&mut self, peer: &MachineId, message: &[u8], now: tokio::time::Instant) -> bool {
        let Ok(nonce) = nonce(message, 3) else {
            return false;
        };
        while self
            .oldest
            .front()
            .is_some_and(|(time, _)| now.duration_since(*time) >= HANDSHAKE_WINDOW)
        {
            self.evict_oldest();
        }
        let key = (peer.clone(), nonce);
        if self.seen.contains(&key) {
            return false;
        }
        if self.seen.len() >= HISTORY_CAP {
            self.evict_oldest();
        }
        self.seen.insert(key.clone());
        self.oldest.push_back((now, key));
        true
    }
    fn evict_oldest(&mut self) {
        if let Some((_, key)) = self.oldest.pop_front() {
            self.seen.remove(&key);
        }
    }
}
// Retain the extra guard across hub relay reconnects without lifetime exhaustion.
fn remember(peer: &MachineId, message: &[u8]) -> bool {
    static HISTORY: std::sync::OnceLock<std::sync::Mutex<History>> = std::sync::OnceLock::new();
    let Ok(mut history) = HISTORY.get_or_init(Default::default).lock() else {
        return false;
    };
    history.remember(peer, message, tokio::time::Instant::now())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn record(total: usize, offset: usize, data: &[u8]) -> Vec<u8> {
        [
            (total as u32).to_be_bytes().as_slice(),
            (offset as u32).to_be_bytes().as_slice(),
            data,
        ]
        .concat()
    }
    #[test]
    fn reassembly_rejects_truncation_gap_overlap_interleaving_oversize_and_empty_fragments() {
        for bad in [
            vec![],
            vec![0; 7],
            record(MAX_FRAME + 1, 0, b"x"),
            record(3, 1, b"ab"),
            record(3, 0, b"abcd"),
            record(3, 0, b""),
            record(3, 0, b"a"),
            record(CHUNK + 1, 0, &vec![1; CHUNK + 1]),
        ] {
            assert!(Assembly::default().push(&bad).is_err());
        }
        for bad in [
            record(CHUNK + 1, 0, b"x"),
            record(CHUNK + 2, CHUNK, b"x"),
            record(CHUNK + 1, CHUNK - 1, b"xx"),
        ] {
            let mut a = Assembly::default();
            assert!(
                a.push(&record(CHUNK + 1, 0, &vec![0; CHUNK]))
                    .unwrap()
                    .is_none()
            );
            assert!(a.push(&bad).is_err());
        }
        assert_eq!(
            Assembly::default().push(&record(0, 0, b"")).unwrap(),
            Some(vec![])
        );
    }
    #[test]
    fn freshness_prelude_rejects_wrong_order_truncation_and_extra_bytes() {
        for kind in [3, 4] {
            let frame = [vec![kind], vec![9; 32]].concat();
            assert_eq!(nonce(&frame, kind).unwrap(), [9; 32]);
            for n in 0..frame.len() {
                assert!(nonce(&frame[..n], kind).is_err());
            }
            assert!(nonce(&frame, kind ^ 1).is_err());
            assert!(nonce(&[frame, vec![0]].concat(), kind).is_err());
        }
    }
    #[test]
    fn handshake_replays_are_remembered_across_listener_reconnects() {
        let id = Identity::generate().machine_id().parse().unwrap();
        assert!(remember(&id, &[3; 33]));
        assert!(!remember(&id, &[3; 33]));
        assert!(remember(&id, &[vec![3], vec![4; 32]].concat()));
    }
    fn request_nonce(n: u64) -> Vec<u8> {
        let mut frame = vec![3; 33];
        frame[1..9].copy_from_slice(&n.to_be_bytes());
        frame
    }
    #[test]
    fn one_hundred_thousand_sequential_handshakes_with_advancing_time() {
        let id = Identity::generate().machine_id().parse().unwrap();
        let mut history = History::default();
        let hub_id = Identity::generate().machine_id().parse().unwrap();
        let worker_key = StaticKey::generate();
        let hub_key = StaticKey::generate();
        let worker_public = worker_key.public_key();
        let hub_public = hub_key.public_key();
        let start = tokio::time::Instant::now();
        for n in 0..100000 {
            let now = start + Duration::from_millis(n * 10);
            let frame = request_nonce(n);
            assert!(history.remember(&id, &frame, now));
            assert!(!history.remember(&id, &frame, now));
            assert!(history.seen.len() <= 1000);
            let mut hub_nonce = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut hub_nonce);
            let context = prologue(&id, &hub_id, &nonce(&frame, 3).unwrap(), &hub_nonce);
            let mut worker =
                NoiseHandshake::with_prologue(true, &worker_key, &hub_public, &context).unwrap();
            let mut hub =
                NoiseHandshake::with_prologue(false, &hub_key, &worker_public, &context).unwrap();
            hub.read(&worker.write().unwrap()).unwrap();
            worker.read(&hub.write().unwrap()).unwrap();
            let mut worker = worker.finish().unwrap();
            let mut hub = hub.finish().unwrap();
            assert_eq!(
                hub.open(&worker.seal(b"confirmed handshake").unwrap())
                    .unwrap(),
                b"confirmed handshake"
            );
        }
        assert_eq!(history.seen.len(), history.oldest.len());
    }
    #[test]
    fn replay_window_and_oldest_first_capacity_eviction() {
        let id = Identity::generate().machine_id().parse().unwrap();
        let now = tokio::time::Instant::now();
        let mut history = History::default();
        assert!(history.remember(&id, &request_nonce(0), now));
        assert!(!history.remember(
            &id,
            &request_nonce(0),
            now + HANDSHAKE_WINDOW - Duration::from_nanos(1)
        ));
        // Rejected replays do not extend retention.
        assert!(history.remember(&id, &request_nonce(0), now + HANDSHAKE_WINDOW));
        let now = now + HANDSHAKE_WINDOW;
        for n in 1..=HISTORY_CAP as u64 {
            assert!(history.remember(&id, &request_nonce(n), now));
        }
        assert_eq!(history.seen.len(), HISTORY_CAP);
        assert!(
            !history
                .seen
                .contains(&(id.clone(), nonce(&request_nonce(0), 3).unwrap()))
        );
        assert!(!history.remember(&id, &request_nonce(1), now));
        assert!(history.remember(&id, &request_nonce(0), now));
        assert!(
            !history
                .seen
                .contains(&(id, nonce(&request_nonce(1), 3).unwrap()))
        );
    }
}
