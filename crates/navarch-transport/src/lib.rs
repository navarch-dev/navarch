//! Local framing and shared reliable envelope delivery. RelayLink belongs to M3.
#![forbid(unsafe_code)]
mod local;
mod memory;
mod reconnect;
mod reliable;
mod socket;
use async_trait::async_trait;
use bytes::Bytes;
pub use local::{LocalHello, LocalLink, LocalListener};
pub use memory::{MemoryLink, memory_pair};
use navarch_proto::MachineId;
pub use reconnect::{Connector, LocalConnector, ReconnectingLink};
pub use reliable::{Delivery, ReliableEvent, ReliableHandle, ReliableTask, reliable};
pub use socket::{PrivateListener, validate_socket_path};

/// Maximum frame payload length.
pub const MAX_FRAME: usize = navarch_proto::MAX_ENVELOPE_BYTES;
/// Link, framing, or envelope failure.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// Underlying socket failure.
    #[error("link I/O")]
    Io(#[from] std::io::Error),
    /// Invalid or oversized input.
    #[error("link protocol: {0}")]
    Protocol(String),
    /// An encoded envelope cannot fit in a frame and was not queued.
    #[error(
        "encoded envelope is {size} bytes and exceeds the 1 MiB frame limit ({limit} bytes); shorten the prompt or payload"
    )]
    Oversized {
        /// Encoded byte count.
        size: usize,
        /// Maximum allowed bytes.
        limit: usize,
    },
    /// Peer or owner stopped.
    #[error("link closed")]
    Closed,
}
/// Bidirectional framed link. `recv` must be cancellation safe.
#[async_trait]
pub trait Link: Send {
    /// Send one frame, bounded to 1 MiB.
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError>;
    /// Receive one complete frame.
    async fn recv(&mut self) -> Result<Bytes, LinkError>;
    /// Address of the connected peer.
    fn peer(&self) -> &MachineId;
}
