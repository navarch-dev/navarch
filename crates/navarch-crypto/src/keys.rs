use crate::{IdentityError, private_key};
use rand::rngs::OsRng;
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// X25519 static secret; the underlying dalek type zeroizes on drop.
pub struct StaticKey(StaticSecret);
impl StaticKey {
    /// Generate with operating system randomness.
    pub fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng))
    }
    pub(crate) fn accepts(&self, remote: &[u8; 32]) -> bool {
        self.0
            .diffie_hellman(&PublicKey::from(*remote))
            .was_contributory()
    }
    /// Public Noise key.
    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.0).to_bytes()
    }
    /// Secret copy, erased on drop, for the Noise builder.
    pub(crate) fn secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }
    /// Atomically load or create `noise.key` with mode 0600.
    pub fn load_or_create(home: &Path) -> Result<Self, IdentityError> {
        let generated = Self::generate();
        let bytes = private_key(home, "noise.key", &generated.secret())?;
        Ok(Self(StaticSecret::from(*bytes)))
    }
}
