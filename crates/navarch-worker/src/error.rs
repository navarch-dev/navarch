use std::path::PathBuf;
/// Worker configuration, persistence or execution failure.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// Hub link failed.
    #[error(transparent)]
    Transport(#[from] navarch_transport::LinkError),
    /// A file could not be loaded, parsed or written.
    #[error("{}: {detail}", path.display())]
    File {
        /// Relevant file.
        path: PathBuf,
        /// Field or operation and cause.
        detail: String,
    },
    /// Local policy refusal.
    #[error(transparent)]
    Policy(#[from] navarch_policy::PolicyError),
    /// Agent failure.
    #[error("{0}")]
    Driver(#[from] navarch_acp::DriverError),
    /// Runtime refused an invalid operation.
    #[error("{0}")]
    Invalid(String),
}
pub(crate) fn file(path: impl Into<PathBuf>, error: impl std::fmt::Display) -> WorkerError {
    WorkerError::File {
        path: path.into(),
        detail: error.to_string(),
    }
}
