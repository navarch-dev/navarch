//! Deterministic adversarial delivery tests, using bounded in-memory frames.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use bytes::Bytes;
use navarch_crypto::Identity;
use navarch_proto::*;
use navarch_transport::*;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::{collections::HashSet, time::Duration};
use tokio::sync::mpsc;
use uuid::Uuid;
struct Queue(mpsc::UnboundedReceiver<Box<dyn Link>>);
#[async_trait]
impl Connector for Queue {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        self.0.recv().await.ok_or(LinkError::Closed)
    }
}
struct Chaos {
    link: MemoryLink,
    rng: StdRng,
    held: Option<Bytes>,
}
#[async_trait]
impl Link for Chaos {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        match self.rng.gen_range(0..5) {
            0 => Ok(()), // silent drop (including acknowledgements)
            1 if self.held.is_none() => {
                self.held = Some(frame);
                Ok(())
            }
            _ => {
                self.link.send(frame.clone()).await?;
                if self.rng.gen_bool(0.5) {
                    self.link.send(frame).await?;
                }
                if let Some(held) = self.held.take() {
                    self.link.send(held).await?;
                }
                Ok(())
            }
        }
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        self.link.recv().await
    }
    fn peer(&self) -> &MachineId {
        self.link.peer()
    }
}
fn id() -> MachineId {
    Identity::generate().machine_id().parse().unwrap()
}
async fn next(rx: &mut mpsc::UnboundedReceiver<ReliableEvent>) -> ReliableEvent {
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test(start_paused = true)]
async fn property_drop_reorder_and_duplicate() {
    for seed in 0..24 {
        let a = id();
        let b = id();
        let (at, ar) = mpsc::unbounded_channel();
        let (bt, br) = mpsc::unbounded_channel();
        let (aout, mut ain, _atask) = reliable(a.clone(), Box::new(Queue(ar)), 128);
        let (bout, mut bin, _btask) = reliable(b.clone(), Box::new(Queue(br)), 128);
        let (al, bl) = memory_pair(a.clone(), b.clone());
        at.send(Box::new(Chaos {
            link: al,
            rng: StdRng::seed_from_u64(seed),
            held: None,
        }) as Box<dyn Link>)
            .ok()
            .unwrap();
        bt.send(Box::new(Chaos {
            link: bl,
            rng: StdRng::seed_from_u64(seed + 100),
            held: None,
        }) as Box<dyn Link>)
            .ok()
            .unwrap();
        assert!(matches!(next(&mut ain).await, ReliableEvent::Connected(_)));
        assert!(matches!(next(&mut bin).await, ReliableEvent::Connected(_)));
        let tasks: HashSet<_> = (0..12).map(|_| TaskId::new()).collect();
        for task_id in &tasks {
            aout.send(Body::Cancel { task_id: *task_id }).unwrap();
        }
        let mut received = HashSet::new();
        while received.len() < tasks.len() {
            if let ReliableEvent::Message(delivery) = next(&mut bin).await {
                let Body::Cancel { task_id } = delivery.envelope.body else {
                    panic!("wrong body")
                };
                assert!(received.insert(task_id), "duplicate at seed {seed}");
                delivery.complete(true);
            }
        }
        assert_eq!(received, tasks);
        // Bidirectional traffic exercises ack tracking independently on each side.
        bout.send(Body::Probe).unwrap();
        loop {
            if let ReliableEvent::Message(d) = next(&mut ain).await {
                assert_eq!(d.envelope.body, Body::Probe);
                d.complete(true);
                break;
            }
        }
    }
}
#[tokio::test]
async fn unacknowledged_envelope_keeps_id_across_reconnect_and_waits_for_apply() {
    let a = id();
    let b = id();
    let (tx, rx) = mpsc::unbounded_channel();
    let (out, mut events, _task) = reliable(a.clone(), Box::new(Queue(rx)), 8);
    let (first, mut peer) = memory_pair(a.clone(), b.clone());
    tx.send(Box::new(first) as Box<dyn Link>).ok().unwrap();
    next(&mut events).await;
    out.send(Body::Probe).unwrap();
    let envelope: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
    drop(peer);
    assert!(matches!(
        next(&mut events).await,
        ReliableEvent::Disconnected(_)
    ));
    let (second, mut peer) = memory_pair(a, b.clone());
    tx.send(Box::new(second) as Box<dyn Link>).ok().unwrap();
    next(&mut events).await;
    let resend: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
    assert_eq!(resend, envelope);
    let incoming = Envelope {
        v: 1,
        id: Uuid::new_v4(),
        from: b,
        to: envelope.from.clone(),
        body: Body::Probe,
    };
    peer.send(Bytes::from(serde_json::to_vec(&incoming).unwrap()))
        .await
        .unwrap();
    let ReliableEvent::Message(d) = next(&mut events).await else {
        panic!("no delivery")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(10), peer.recv())
            .await
            .is_err()
    );
    d.complete(true);
    let ack: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
    assert_eq!(ack.body, Body::Ack { of: incoming.id });
    peer.send(Bytes::from(serde_json::to_vec(&incoming).unwrap()))
        .await
        .unwrap();
    let ack: Envelope = serde_json::from_slice(&peer.recv().await.unwrap()).unwrap();
    assert_eq!(ack.body, Body::Ack { of: incoming.id });
    assert!(events.try_recv().is_err());
}
#[tokio::test]
async fn invalid_version_or_address_disconnects_without_delivery() {
    for version in [1, 2] {
        let a = id();
        let b = id();
        let (tx, rx) = mpsc::unbounded_channel();
        let (_out, mut events, _task) = reliable(a.clone(), Box::new(Queue(rx)), 2);
        let (link, mut peer) = memory_pair(a.clone(), b.clone());
        tx.send(Box::new(link) as Box<dyn Link>).ok().unwrap();
        next(&mut events).await;
        let envelope = Envelope {
            v: version,
            id: Uuid::new_v4(),
            from: if version == 1 { id() } else { b },
            to: a,
            body: Body::Probe,
        };
        peer.send(Bytes::from(serde_json::to_vec(&envelope).unwrap()))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut events).await,
            ReliableEvent::Disconnected(_)
        ));
    }
}
#[tokio::test]
async fn dedup_cache_is_bounded() {
    let a = id();
    let b = id();
    let (tx, rx) = mpsc::unbounded_channel();
    let (_out, mut events, _task) = reliable(a.clone(), Box::new(Queue(rx)), 2);
    let (link, mut peer) = memory_pair(a.clone(), b.clone());
    tx.send(Box::new(link) as Box<dyn Link>).ok().unwrap();
    next(&mut events).await;
    let envelopes: Vec<_> = (0..3)
        .map(|_| Envelope {
            v: 1,
            id: Uuid::new_v4(),
            from: b.clone(),
            to: a.clone(),
            body: Body::Probe,
        })
        .collect();
    for envelope in envelopes.iter().chain(std::iter::once(&envelopes[0])) {
        peer.send(Bytes::from(serde_json::to_vec(envelope).unwrap()))
            .await
            .unwrap();
        let ReliableEvent::Message(d) = next(&mut events).await else {
            panic!("no delivery")
        };
        assert_eq!(d.envelope.id, envelope.id);
        d.complete(true);
        peer.recv().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn reconnection_backoff_grows_with_jitter_and_caps_at_sixty_seconds() {
    use std::sync::{Arc, Mutex};
    struct Failing {
        attempts: Arc<Mutex<Vec<tokio::time::Instant>>>,
        link: Option<MemoryLink>,
        failures: usize,
    }
    #[async_trait]
    impl Connector for Failing {
        async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
            self.attempts
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            if self.failures > 0 {
                self.failures -= 1;
                return Err(LinkError::Closed);
            }
            Ok(Box::new(self.link.take().unwrap()))
        }
    }
    let attempts = Arc::new(Mutex::new(vec![]));
    let (link, _peer) = memory_pair(id(), id());
    let mut reconnect = ReconnectingLink::new(Box::new(Failing {
        attempts: attempts.clone(),
        link: Some(link),
        failures: 8,
    }));
    let _connected = reconnect.connect().await;
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 9);
    for (i, pair) in attempts.windows(2).enumerate() {
        let min = (1u64 << i).min(60);
        let max = (min * 1500).min(60_000);
        let actual = (pair[1] - pair[0]).as_millis();
        assert!(
            actual >= u128::from(min * 1000) && actual <= u128::from(max),
            "interval {i}: {actual}"
        );
    }
}
