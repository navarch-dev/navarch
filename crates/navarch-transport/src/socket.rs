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
