//! Hub runtime and state-free MCP adapter. No MCP types cross this public API.
#![forbid(unsafe_code)]
pub mod control;
mod daemon;
mod direct;
pub use direct::{DirectConfig, RemoteDirect};
mod events;
mod init;
mod mcp;
mod requests;
mod runtime;
pub use daemon::{serve, serve_with_links, serve_with_relay};
mod remote;
pub use init::init;
pub use mcp::{serve_mcp, serve_mcp_with_channel};
/// Bounded event hint; persistent inbox and task cards remain authoritative.
pub use navarch_store::HubEvent;
pub use remote::RemoteRelay;
pub use runtime::Hub;
/// Hub validation, persistence, or connection failure.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// Filesystem or control stream error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Invalid JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Persistent state error.
    #[error(transparent)]
    Store(#[from] navarch_store::StoreError),
    /// Worker link error.
    #[error(transparent)]
    Link(#[from] navarch_transport::LinkError),
    /// Invalid request or unavailable service.
    #[error("{0}")]
    Invalid(String),
}

mod sessions;
