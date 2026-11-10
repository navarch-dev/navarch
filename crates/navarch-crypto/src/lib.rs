//! Machine identity, pairing and Noise. Only key file helpers perform I/O.
#![forbid(unsafe_code)]

use blake2::{Blake2s256, Digest};
use data_encoding::BASE32_NOPAD;
use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;
mod channel;
mod keys;
mod pairing;
pub use channel::{NOISE_PATTERN, NoiseChannel, NoiseHandshake};
pub use keys::StaticKey;
pub use pairing::{PairCode, PairInfo, PairRole, Pairing};

/// Invalid, expired or unauthenticated cryptographic message. Contents are never exposed.
#[derive(Debug, thiserror::Error)]
#[error("cryptographic protocol rejected message")]
pub struct CryptoError;

/// Identity key loading or creation failed.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Filesystem operation failed.
    #[error("identity key I/O")]
    Io(#[from] std::io::Error),
    /// The key file is not a private, regular, 32-byte secret file.
    #[error("secret key file must be regular, contain exactly 32 bytes, and have mode 0600")]
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
        let generated = Self::generate();
        let bytes = private_key(
            home,
            "identity.key",
            &Zeroizing::new(generated.0.to_bytes()),
        )?;
        Ok(Self(SigningKey::from_bytes(&bytes)))
    }
    /// Sign a domain-separated relay challenge.
    pub fn sign_challenge(&self, challenge: &[u8; 32]) -> [u8; 64] {
        self.0.sign(&auth_message(challenge)).to_bytes()
    }
}
fn auth_message(challenge: &[u8; 32]) -> Vec<u8> {
    [b"navarch-relay-auth-v1".as_slice(), challenge.as_slice()].concat()
}
/// Verify a relay signature using dalek's strict verification.
pub fn verify_challenge(
    public: &[u8; 32],
    challenge: &[u8; 32],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(public).map_err(|_| CryptoError)?;
    let signature = ed25519_dalek::Signature::from_slice(signature).map_err(|_| CryptoError)?;
    key.verify_strict(&auth_message(challenge), &signature)
        .map_err(|_| CryptoError)
}
fn private_key(
    home: &Path,
    name: &str,
    generated: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
    fs::create_dir_all(home)?;
    let path = home.join(name);
    if !path.exists() {
        // NamedTempFile uses 0600 at creation, before secret bytes are written.
        let mut temp = tempfile::NamedTempFile::new_in(home)?;
        temp.write_all(generated)?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.error.into()),
        }
    }
    // Check the opened descriptor, not a path that could change between check
    // and read. NOFOLLOW rejects symlinks without altering their targets.
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = options.open(&path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != 32 {
        return Err(IdentityError::InvalidKey);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o777 != 0o600 {
            return Err(IdentityError::InvalidKey);
        }
    }
    let mut secret = Zeroizing::new([0; 32]);
    file.read_exact(secret.as_mut())?;
    if file.read(&mut [0; 1])? != 0 {
        return Err(IdentityError::InvalidKey);
    }
    Ok(secret)
}
