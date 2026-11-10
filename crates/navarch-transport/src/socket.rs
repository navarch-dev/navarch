use crate::LinkError;
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::net::{UnixListener, UnixStream};
/// A private Unix listener with stale socket recovery and ownership-aware cleanup.
/// The containing directory must be accessible only by the current user (0700).
pub struct PrivateListener {
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
}
impl PrivateListener {
    /// Bind a 0600 socket. Never replace a live listener, symlink, or regular file.
    pub fn bind(path: &Path) -> Result<Self, LinkError> {
        validate_socket_path(path)?;
        if let Ok(meta) = fs::symlink_metadata(path) {
            if !meta.file_type().is_socket() {
                return Err(LinkError::Protocol(format!(
                    "refusing to replace {}",
                    path.display()
                )));
            }
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => {
                    return Err(LinkError::Protocol(format!(
                        "socket already in use: {}",
                        path.display()
                    )));
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(path)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let inode = fs::metadata(path)?.ino();
        Ok(Self {
            listener,
            path: path.into(),
            inode,
        })
    }
    /// Accept a raw local stream (for the line-delimited control API).
    pub async fn accept(&self) -> Result<UnixStream, LinkError> {
        Ok(self.listener.accept().await?.0)
    }
}
impl Drop for PrivateListener {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && m.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Validate a Unix socket address before I/O, with an actionable path-length error.
pub fn validate_socket_path(path: &Path) -> Result<(), LinkError> {
    use std::os::unix::{ffi::OsStrExt, net::SocketAddr};
    SocketAddr::from_pathname(path).map(|_| ()).map_err(|error| {
        // Ask std for this platform's SUN_LEN bound; no unsafe libc layout access.
        let limit = (1..=1024).take_while(|len| SocketAddr::from_pathname("x".repeat(*len)).is_ok()).last().unwrap_or(0);
        if path.as_os_str().as_bytes().len() > limit {
            LinkError::Protocol(format!("Unix socket path '{}' is too long ({} bytes; limit {limit} bytes); set NAVARCH_HOME to a shorter directory", path.display(), path.as_os_str().as_bytes().len()))
        } else { LinkError::Protocol(format!("invalid Unix socket path '{}': {error}", path.display())) }
    })
}
