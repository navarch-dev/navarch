//! Machine identity. Pairing and encrypted channels belong to M3.
#![forbid(unsafe_code)]

use blake2::{Blake2s256, Digest};
use data_encoding::BASE32_NOPAD;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use std::{fs, io::Write, path::Path};

/// Identity key loading or creation failed.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Filesystem operation failed.
    #[error("identity key: {0}")]
    Io(#[from] std::io::Error),
    /// The key file has an invalid length.
    #[error("identity.key must contain exactly 32 secret bytes")]
    InvalidKey,
}

/// Derive canonical machine ID text from an Ed25519 public key.
/// The proto crate's MachineId is constructed by callers in a higher layer.
pub fn machine_id(public_key: &[u8; 32]) -> String {
    let digest = Blake2s256::digest(public_key);
    BASE32_NOPAD.encode(&digest[..16]).to_ascii_lowercase()
}

/// Private machine identity; debug output never includes secret material.
pub struct Identity(SigningKey);
impl Identity {
    /// Generate an identity using the operating system random source.
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut OsRng))
    }
    /// Public Ed25519 key.
    pub fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    /// Canonical machine identifier text.
    pub fn machine_id(&self) -> String {
        machine_id(&self.public_key())
    }
    /// Load or create `identity.key` under an already resolved home directory.
    /// Creation is atomic and concurrent callers converge on the same identity.
    pub fn load_or_create(home: &Path) -> Result<Self, IdentityError> {
        fs::create_dir_all(home)?;
        let path = home.join("identity.key");
        match fs::read(&path) {
            Ok(bytes) => return Self::load(&path, bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let identity = Self::generate();
        // A private, same-directory temporary file is published using a hard link:
        // unlike rename, hard_link cannot replace another creator's key.
        let mut temp = tempfile::NamedTempFile::new_in(home)?;
        temp.write_all(&identity.0.to_bytes())?;
        temp.as_file().sync_all()?;
        match fs::hard_link(temp.path(), &path) {
            Ok(()) => Self::load(&path, fs::read(&path)?),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::load(&path, fs::read(&path)?)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn load(path: &Path, bytes: Vec<u8>) -> Result<Self, IdentityError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let secret: [u8; 32] = bytes.try_into().map_err(|_| IdentityError::InvalidKey)?;
        Ok(Self(SigningKey::from_bytes(&secret)))
    }
}
