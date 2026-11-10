//! Hub runtime and state-free MCP adapter. No MCP types cross this public API.
#![forbid(unsafe_code)]
pub mod control;
mod daemon;
mod events;
mod init;
mod mcp;
mod requests;
mod runtime;
pub use daemon::serve;
pub use init::init;
pub use mcp::serve_mcp;
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
