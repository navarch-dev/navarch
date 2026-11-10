use crate::*;
use futures_util::{SinkExt, StreamExt, stream::SplitStream};
use navarch_crypto::machine_id;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use tokio::{
    net::UnixStream,
    sync::{mpsc, watch},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Local peer introduction. Unix file permissions authorize this unencrypted link.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalHello {
    /// Ed25519 public key from which the address is derived.
    pub public_key: [u8; 32],
    /// Worker or hub display name.
    pub display_name: String,
}
impl LocalHello {
    /// Derive and validate the peer's address.
    pub fn machine_id(&self) -> Result<MachineId, LinkError> {
        machine_id(&self.public_key)
            .parse()
            .map_err(|e: navarch_proto::ParseMachineIdError| LinkError::Protocol(e.to_string()))
    }
}
fn framed(stream: UnixStream) -> Framed<UnixStream, LengthDelimitedCodec> {
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME)
        .new_framed(stream)
}
async fn hello(
    stream: UnixStream,
    local: &LocalHello,
) -> Result<(LocalLink, LocalHello), LinkError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut io = framed(stream);
        io.send(Bytes::from(
            serde_json::to_vec(local).map_err(|e| LinkError::Protocol(e.to_string()))?,
        ))
        .await?;
        let frame = io.next().await.ok_or(LinkError::Closed)??;
        let remote: LocalHello =
            serde_json::from_slice(&frame).map_err(|e| LinkError::Protocol(e.to_string()))?;
        Ok((LocalLink::new(io, remote.machine_id()?), remote))
    })
    .await
    .map_err(|_| LinkError::Protocol("local introduction timed out".into()))?
}
/// Unix stream with u32 big-endian length-prefixed, bounded frames.
pub struct LocalLink {
    read: SplitStream<Framed<UnixStream, LengthDelimitedCodec>>,
    output: mpsc::UnboundedSender<Bytes>,
    failed: watch::Receiver<Option<String>>,
    writer: tokio::task::JoinHandle<()>,
    peer: MachineId,
}
impl Drop for LocalLink {
    fn drop(&mut self) {
        self.writer.abort();
    }
}
impl LocalLink {
    fn new(io: Framed<UnixStream, LengthDelimitedCodec>, peer: MachineId) -> Self {
        let (mut write, read) = io.split();
        let (output, mut frames) = mpsc::unbounded_channel::<Bytes>();
        let (error, failed) = watch::channel(None);
        // Keep socket writes independent from reads. If both peers send a maximum
        // frame simultaneously, neither may wait for the other to start reading.
        let writer = tokio::spawn(async move {
            while let Some(frame) = frames.recv().await {
                if let Err(e) = write.send(frame).await {
                    let _ = error.send(Some(e.to_string()));
                    return;
                }
            }
        });
        Self {
            read,
            output,
            failed,
            writer,
            peer,
        }
    }
    /// Connect and exchange local public identity information.
    pub async fn connect(path: &Path, local: &LocalHello) -> Result<Self, LinkError> {
        Ok(hello(UnixStream::connect(path).await?, local).await?.0)
    }
}
#[async_trait]
impl Link for LocalLink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        if frame.len() > MAX_FRAME {
            return Err(LinkError::Protocol("frame exceeds 1 MiB".into()));
        }
        // Successful send queues the frame; reliable acknowledgements establish
        // remote receipt. Any writer failure also wakes the receive side.
        self.output.send(frame).map_err(|_| LinkError::Closed)
    }
    async fn recv(&mut self) -> Result<Bytes, LinkError> {
        tokio::select! {
            frame = self.read.next() => Ok(frame.ok_or(LinkError::Closed)??.freeze()),
            _ = self.failed.changed() => Err(LinkError::Protocol(self.failed.borrow().clone().unwrap_or_else(|| "socket writer stopped".into()))),
        }
    }
    fn peer(&self) -> &MachineId {
        &self.peer
    }
}
/// Listener for framed worker links.
pub struct LocalListener {
    listener: PrivateListener,
    local: LocalHello,
}
impl LocalListener {
    /// Bind the hub's worker socket with mode 0600.
    pub fn bind(path: &Path, local: LocalHello) -> Result<Self, LinkError> {
        Ok(Self {
            listener: PrivateListener::bind(path)?,
            local,
        })
    }
    /// Accept and exchange identity introductions.
    pub async fn accept(&self) -> Result<(LocalLink, LocalHello), LinkError> {
        hello(self.listener.accept().await?, &self.local).await
    }
}
