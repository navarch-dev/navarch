//! Hub runtime and local control API.
#![forbid(unsafe_code)]
pub mod control;
mod daemon;
mod direct;
pub use direct::{DirectConfig, RemoteDirect};
mod events;
mod init;
mod requests;
mod runtime;
pub use daemon::{serve, serve_with_links, serve_with_relay};
mod remote;
pub use init::init;
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
    /// The control daemon is absent or disconnected.
    #[error("{0}")]
    Unavailable(String),
    /// Invalid request or unavailable service.
    #[error("{0}")]
    Invalid(String),
}

mod sessions;
