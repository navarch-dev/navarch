//! Shared nonce exchange, Noise KK authentication and bounded record assembly.
use crate::{
    relay_socket::{Guard, protocol},
    *,
};
use navarch_crypto::{NoiseChannel, NoiseHandshake, StaticKey};
use rand::RngCore;
use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};
use tokio::sync::mpsc;

#[async_trait]
pub(crate) trait ChannelIo {
    fn send(&self, data: &[u8]) -> Result<(), LinkError>;
    async fn recv(&mut self) -> Result<Vec<u8>, LinkError>;
    fn peer(&self) -> &MachineId;
    fn local(&self) -> &MachineId;
    fn initial_nonce(&self) -> Option<[u8; 32]> {
        None
    }
}
/// End-to-end Noise link, with background I/O for cancellation-safe full duplex.
pub struct EncryptedLink {
    peer: MachineId,
    kind: &'static str,
    output: mpsc::UnboundedSender<Bytes>,
    input: mpsc::Receiver<Result<Bytes, LinkError>>,
    _task: Guard,
}
impl EncryptedLink {
    pub(crate) async fn handshake<I, F>(
        mut io: I,
        key: &StaticKey,
        remote: F,
        initiator: bool,
        kind: &'static str,
    ) -> Result<Self, LinkError>
    where
        I: ChannelIo + Send + 'static,
        F: FnOnce() -> Option<[u8; 32]> + Send,
    {
        let channel = tokio::time::timeout(HANDSHAKE_WINDOW, async {
            // A fresh responder nonce prevents replayed KK first messages from
            // creating an authenticated link, even after process restart.
            let mut own = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut own);
            let (worker_nonce, hub_nonce) = if initiator {
                if let Some(initial) = io.initial_nonce() {
                    own = initial;
                } else {
                    io.send(&[vec![3], own.to_vec()].concat())?;
                }
                (own, nonce(&io.recv().await?, 4)?)
            } else {
                let worker = nonce(&io.recv().await?, 3)?;
                io.send(&[vec![4], own.to_vec()].concat())?;
                (worker, own)
            };
            let (worker, hub) = if initiator {
                (io.local(), io.peer())
            } else {
                (io.peer(), io.local())
            };
            let prologue = prologue(worker, hub, &worker_nonce, &hub_nonce);
            // Send the same clear nonce challenge to every syntactically valid
            // hello and consume the first KK frame before rejecting an unknown
            // ID. Otherwise the prelude itself would reveal paired machine IDs.
            let first = if initiator {
                None
            } else {
                Some(io.recv().await?)
            };
            // Resolve authorization at the Noise boundary, so revocation while
            // waiting for the first frame cannot use a previously captured key.
            let remote = remote().ok_or_else(protocol)?;
            let mut hs = NoiseHandshake::with_prologue(initiator, key, &remote, &prologue)
                .map_err(|_| protocol())?;
            if initiator {
                io.send(&[vec![0], hs.write().map_err(|_| protocol())?].concat())?;
            }
            let frame = if let Some(frame) = first {
                frame
            } else {
                io.recv().await?
            };
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
        let peer = io.peer().clone();
        let (output, rx) = mpsc::unbounded_channel();
        let (tx, input) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            if let Err(e) = run(io, channel, rx, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });
        Ok(Self {
            peer,
            kind,
            output,
            input,
            _task: Guard(vec![task]),
        })
    }
}
#[async_trait]
impl Link for EncryptedLink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if frame.len() > MAX_FRAME {
            return Err(protocol());
        }
        self.output.send(frame).map_err(|_| LinkError::Closed)
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        self.input.recv().await.ok_or(LinkError::Closed)?
    }
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn peer(&self) -> &MachineId {
        &self.peer
    }
}
pub(crate) fn prologue(
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
async fn run<I: ChannelIo + Send>(
    mut io: I,
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
pub(crate) fn remember(peer: &MachineId, message: &[u8]) -> bool {
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
    use navarch_crypto::Identity;
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
