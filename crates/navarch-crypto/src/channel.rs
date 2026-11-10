use crate::{CryptoError, StaticKey};
/// Exact interoperable Noise parameters (KK, X25519, ChaChaPoly, BLAKE2s).
pub const NOISE_PATTERN: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";
/// Strict two-message KK handshake. Errors permanently invalidate the state.
pub struct NoiseHandshake {
    state: Option<snow::HandshakeState>,
    step: u8,
    initiator: bool,
}
impl NoiseHandshake {
    /// Pin both static keys before accepting any handshake bytes.
    pub fn new(initiator: bool, local: &StaticKey, remote: &[u8; 32]) -> Result<Self, CryptoError> {
        Self::with_prologue(initiator, local, remote, &[])
    }
    /// Bind caller-supplied connection freshness and addressing into Noise's
    /// standard prologue, without changing the KK pattern or its primitives.
    pub fn with_prologue(
        initiator: bool,
        local: &StaticKey,
        remote: &[u8; 32],
        prologue: &[u8],
    ) -> Result<Self, CryptoError> {
        if !local.accepts(remote) {
            return Err(CryptoError);
        }
        let secret = local.secret();
        let builder = snow::Builder::new(NOISE_PATTERN.parse().map_err(|_| CryptoError)?)
            .prologue(prologue)
            .map_err(|_| CryptoError)?
            .local_private_key(secret.as_slice())
            .map_err(|_| CryptoError)?
            .remote_public_key(remote)
            .map_err(|_| CryptoError)?;
        let state = if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(|_| CryptoError)?;
        Ok(Self {
            state: Some(state),
            step: 0,
            initiator,
        })
    }
    /// Write the next empty-payload handshake message.
    pub fn write(&mut self) -> Result<Vec<u8>, CryptoError> {
        let expected = if self.initiator { 0 } else { 1 };
        if self.step != expected {
            self.state = None;
            return Err(CryptoError);
        }
        let mut buf = vec![0; 128];
        let result = self
            .state
            .as_mut()
            .ok_or(CryptoError)?
            .write_message(&[], &mut buf);
        match result {
            Ok(n) => {
                buf.truncate(n);
                self.step += 1;
                Ok(buf)
            }
            Err(_) => {
                self.state = None;
                Err(CryptoError)
            }
        }
    }
    /// Read exactly one empty-payload handshake message; reject extra/truncated data.
    pub fn read(&mut self, frame: &[u8]) -> Result<(), CryptoError> {
        let expected = if self.initiator { 1 } else { 0 };
        if self.step != expected || frame.len() != 48 {
            self.state = None;
            return Err(CryptoError);
        }
        let ephemeral: [u8; 32] = frame[..32].try_into().map_err(|_| CryptoError)?;
        if !x25519_dalek::StaticSecret::from([0; 32])
            .diffie_hellman(&x25519_dalek::PublicKey::from(ephemeral))
            .was_contributory()
        {
            self.state = None;
            return Err(CryptoError);
        }
        let result = self
            .state
            .as_mut()
            .ok_or(CryptoError)?
            .read_message(frame, &mut [0; 128]);
        match result {
            Ok(0) => {
                self.step += 1;
                Ok(())
            }
            _ => {
                self.state = None;
                Err(CryptoError)
            }
        }
    }
    /// Enter transport mode only after both handshake messages.
    pub fn finish(mut self) -> Result<NoiseChannel, CryptoError> {
        if self.step != 2 {
            return Err(CryptoError);
        }
        Ok(NoiseChannel {
            state: Some(
                self.state
                    .take()
                    .ok_or(CryptoError)?
                    .into_transport_mode()
                    .map_err(|_| CryptoError)?,
            ),
        })
    }
}
/// Ordered, replay-resistant Noise transport. Any failure destroys the channel.
pub struct NoiseChannel {
    state: Option<snow::TransportState>,
}
impl NoiseChannel {
    /// Encrypt a record (at most 65519 plaintext bytes).
    pub fn seal(&mut self, plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = vec![0; plain.len().saturating_add(16).min(65536)];
        match self
            .state
            .as_mut()
            .ok_or(CryptoError)?
            .write_message(plain, &mut out)
        {
            Ok(n) => {
                out.truncate(n);
                Ok(out)
            }
            Err(_) => {
                self.state = None;
                Err(CryptoError)
            }
        }
    }
    /// Authenticate and decrypt the next record. Snow checks tags in constant time.
    pub fn open(&mut self, cipher: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if cipher.len() < 16 || cipher.len() > 65535 {
            self.state = None;
            return Err(CryptoError);
        }
        let mut out = vec![0; cipher.len()];
        match self
            .state
            .as_mut()
            .ok_or(CryptoError)?
            .read_message(cipher, &mut out)
        {
            Ok(n) => {
                out.truncate(n);
                Ok(out)
            }
            Err(_) => {
                self.state = None;
                Err(CryptoError)
            }
        }
    }
}
