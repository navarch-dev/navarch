use crate::CryptoError;
use blake2::{
    Blake2sMac256,
    digest::{KeyInit as MacKeyInit, Mac},
};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use rand::{Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

/// Ten-minute pairing code. Debug intentionally omitted.
pub struct PairCode {
    room: String,
    secret: Zeroizing<String>,
}
impl PairCode {
    /// Generate an unbiased code from the OS RNG.
    pub fn generate() -> Self {
        Self {
            room: format!("{:04}", OsRng.gen_range(0..10000)),
            secret: Zeroizing::new(format!("{:06}", OsRng.gen_range(0..1000000))),
        }
    }
    /// Parse exactly four room digits, a dash, and six password digits.
    pub fn parse(text: &str) -> Result<Self, CryptoError> {
        if text.len() != 11
            || text.as_bytes()[4] != b'-'
            || !text
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 4 || b.is_ascii_digit())
        {
            return Err(CryptoError);
        }
        Ok(Self {
            room: text[..4].into(),
            secret: Zeroizing::new(text[5..].into()),
        })
    }
    /// Public routing room.
    pub fn room(&self) -> &str {
        &self.room
    }
    /// Explicit presentation to the user; never pass to logging.
    pub fn display(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{}-{}", self.room, self.secret.as_str()))
    }
}
/// Ordered pairing role.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PairRole {
    /// Hub, SPAKE2 side A.
    Hub,
    /// Worker, SPAKE2 side B.
    Worker,
}
/// Authenticated introduction. Only returned after mutual key confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairInfo {
    /// Ed25519 identity key, lowercase hex.
    #[serde(with = "key_hex")]
    pub identity: [u8; 32],
    /// X25519 Noise static key, lowercase hex.
    #[serde(with = "key_hex")]
    pub static_key: [u8; 32],
    /// Human display name (at most 200 UTF-8 bytes).
    pub display_name: String,
}
impl PairInfo {
    /// Check key encodings, contributory X25519 keys and bounded display names.
    pub fn validate(&self) -> Result<(), CryptoError> {
        validate(self)
    }
}
// Crypto cannot depend on proto; public key encoding stays canonical here too.
mod key_hex {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(key: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&key.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        if s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(serde::de::Error::custom("invalid public key"));
        }
        let mut key = [0; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(serde::de::Error::custom)?;
        }
        Ok(key)
    }
}
/// Single-use, fail-closed SPAKE2 pairing state machine. Time is caller supplied
/// monotonic seconds; every transition enforces the original ten-minute deadline.
pub struct Pairing {
    role: PairRole,
    step: u8,
    started: u64,
    room: String,
    spake: Option<Spake2<Ed25519Group>>,
    key: Option<Zeroizing<Vec<u8>>>,
    a: Vec<u8>,
    b: Vec<u8>,
    local: PairInfo,
    remote: Option<PairInfo>,
    done: bool,
}
impl Pairing {
    /// Create a side. Hub's first output comes from `start`; worker waits for it.
    pub fn new(
        role: PairRole,
        code: &PairCode,
        local: PairInfo,
        now: u64,
    ) -> Result<Self, CryptoError> {
        validate(&local)?;
        let a = Identity::new(format!("navarch-pair-v1:hub:{}", code.room).as_bytes());
        let b = Identity::new(format!("navarch-pair-v1:worker:{}", code.room).as_bytes());
        let pw = Password::new(code.secret.as_bytes());
        let (spake, msg) = match role {
            PairRole::Hub => Spake2::start_a(&pw, &a, &b),
            PairRole::Worker => Spake2::start_b(&pw, &a, &b),
        };
        Ok(Self {
            role,
            step: if role == PairRole::Hub { 0 } else { 1 },
            started: now,
            room: code.room.clone(),
            spake: Some(spake),
            key: None,
            a: if role == PairRole::Hub {
                msg.clone()
            } else {
                vec![]
            },
            b: if role == PairRole::Worker {
                msg
            } else {
                vec![]
            },
            local,
            remote: None,
            done: false,
        })
    }
    /// Emit the hub's first SPAKE2 message exactly once.
    pub fn start(&mut self, now: u64) -> Result<Vec<u8>, CryptoError> {
        if self.step != 0 || self.expired(now) {
            self.fail();
            return Err(CryptoError);
        }
        self.step = 2;
        Ok(packet(1, &self.a))
    }
    /// Consume exactly the expected message, returning at most one outbound message.
    /// Any error permanently destroys this attempt; callers must close the room.
    pub fn receive(&mut self, frame: &[u8], now: u64) -> Result<Option<Vec<u8>>, CryptoError> {
        let result = self.receive_inner(frame, now);
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn expired(&self, now: u64) -> bool {
        now < self.started || now - self.started >= 600
    }
    fn fail(&mut self) {
        self.step = 255;
        self.done = false;
        self.key = None;
        self.spake = None;
        self.remote = None;
    }
    fn receive_inner(&mut self, frame: &[u8], now: u64) -> Result<Option<Vec<u8>>, CryptoError> {
        if self.expired(now) || frame.len() < 2 || frame.len() > 4096 || frame[0] != self.step {
            return Err(CryptoError);
        }
        let data = &frame[1..];
        let output = match self.step {
            1 | 2 => {
                if data.len() != 33 {
                    return Err(CryptoError);
                }
                if self.role == PairRole::Hub {
                    self.b = data.to_vec();
                } else {
                    self.a = data.to_vec();
                }
                self.key = Some(Zeroizing::new(
                    self.spake
                        .take()
                        .ok_or(CryptoError)?
                        .finish(data)
                        .map_err(|_| CryptoError)?,
                ));
                if self.role == PairRole::Worker {
                    self.step = 3;
                    Some(packet(2, &self.b))
                } else {
                    self.step = 4;
                    Some(packet(3, &self.encrypt(0)?))
                }
            }
            3 | 4 => {
                let nonce = if self.step == 3 { 0 } else { 1 };
                let plain = Zeroizing::new(
                    self.cipher()?
                        .decrypt(
                            Nonce::from_slice(&nonce_bytes(nonce)),
                            Payload {
                                msg: data,
                                aad: &self.context(),
                            },
                        )
                        .map_err(|_| CryptoError)?,
                );
                let remote: PairInfo = serde_json::from_slice(&plain).map_err(|_| CryptoError)?;
                validate(&remote)?;
                self.remote = Some(remote);
                if self.role == PairRole::Worker {
                    self.step = 5;
                    Some(packet(4, &self.encrypt(1)?))
                } else {
                    self.step = 6;
                    Some(packet(5, &self.confirm(b"hub")?))
                }
            }
            5 | 6 => {
                let role = if self.step == 5 {
                    b"hub".as_slice()
                } else {
                    b"worker".as_slice()
                };
                self.mac(role)?
                    .verify_slice(data)
                    .map_err(|_| CryptoError)?;
                let out = if self.role == PairRole::Worker {
                    Some(packet(6, &self.confirm(b"worker")?))
                } else {
                    None
                };
                self.done = true;
                self.step = 254;
                self.key = None;
                out
            }
            _ => return Err(CryptoError),
        };
        Ok(output)
    }
    fn context(&self) -> Vec<u8> {
        [
            b"navarch-pair-v1\0hub\0worker\0".as_slice(),
            self.room.as_bytes(),
            &self.a,
            &self.b,
        ]
        .concat()
    }
    fn cipher(&self) -> Result<ChaCha20Poly1305, CryptoError> {
        let mut mac =
            <Blake2sMac256 as MacKeyInit>::new_from_slice(self.key.as_ref().ok_or(CryptoError)?)
                .map_err(|_| CryptoError)?;
        Mac::update(&mut mac, b"navarch-pair-v1:encryption");
        let key = Zeroizing::new(<[u8; 32]>::from(mac.finalize().into_bytes()));
        <ChaCha20Poly1305 as KeyInit>::new_from_slice(key.as_slice()).map_err(|_| CryptoError)
    }
    fn encrypt(&self, nonce: u8) -> Result<Vec<u8>, CryptoError> {
        let plain = Zeroizing::new(serde_json::to_vec(&self.local).map_err(|_| CryptoError)?);
        self.cipher()?
            .encrypt(
                Nonce::from_slice(&nonce_bytes(nonce)),
                Payload {
                    msg: &plain,
                    aad: &self.context(),
                },
            )
            .map_err(|_| CryptoError)
    }
    fn mac(&self, role: &[u8]) -> Result<Blake2sMac256, CryptoError> {
        let mut mac =
            <Blake2sMac256 as MacKeyInit>::new_from_slice(self.key.as_ref().ok_or(CryptoError)?)
                .map_err(|_| CryptoError)?;
        Mac::update(&mut mac, b"navarch-pair-v1:confirmation\0");
        Mac::update(&mut mac, role);
        Mac::update(&mut mac, &self.context());
        let remote = self.remote.as_ref().ok_or(CryptoError)?;
        let (h, w) = if self.role == PairRole::Hub {
            (&self.local, remote)
        } else {
            (remote, &self.local)
        };
        // Length-prefix both canonical introductions, including both pairs of keys.
        for info in [h, w] {
            let bytes = serde_json::to_vec(info).map_err(|_| CryptoError)?;
            Mac::update(&mut mac, &(bytes.len() as u32).to_be_bytes());
            Mac::update(&mut mac, &bytes);
        }
        Ok(mac)
    }
    fn confirm(&self, role: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Ok(self.mac(role)?.finalize().into_bytes().to_vec())
    }
    /// Return the authenticated peer only after confirmation succeeded.
    pub fn peer(&self) -> Result<&PairInfo, CryptoError> {
        if !self.done {
            return Err(CryptoError);
        }
        self.remote.as_ref().ok_or(CryptoError)
    }
}
fn packet(step: u8, data: &[u8]) -> Vec<u8> {
    [vec![step], data.to_vec()].concat()
}
fn nonce_bytes(n: u8) -> [u8; 12] {
    let mut b = [0; 12];
    b[11] = n;
    b
}
fn validate(info: &PairInfo) -> Result<(), CryptoError> {
    if info.display_name.is_empty()
        || info.display_name.len() > 200
        || info.display_name.chars().any(char::is_control)
        || !x25519_dalek::StaticSecret::from([0; 32])
            .diffie_hellman(&x25519_dalek::PublicKey::from(info.static_key))
            .was_contributory()
        || ed25519_dalek::VerifyingKey::from_bytes(&info.identity).map_or(true, |key| key.is_weak())
    {
        return Err(CryptoError);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn info() -> PairInfo {
        PairInfo {
            identity: crate::Identity::generate().public_key(),
            static_key: crate::StaticKey::generate().public_key(),
            display_name: "test".into(),
        }
    }
    #[test]
    fn confirmation_binds_every_transcript_component() {
        for component in 0..9 {
            let code = PairCode::parse("1234-123456").unwrap();
            let mut h = Pairing::new(PairRole::Hub, &code, info(), 0).unwrap();
            let mut w = Pairing::new(PairRole::Worker, &code, info(), 0).unwrap();
            let m = h.start(0).unwrap();
            let m = w.receive(&m, 0).unwrap().unwrap();
            let m = h.receive(&m, 0).unwrap().unwrap();
            let m = w.receive(&m, 0).unwrap().unwrap();
            let m = h.receive(&m, 0).unwrap().unwrap();
            // The adversary changes the worker's transcript view after the
            // encrypted exchange. The hub's valid confirmation must fail.
            match component {
                0 => w.room = "4321".into(),
                1 => w.a[1] ^= 1,
                2 => w.b[1] ^= 1,
                3 => w.local.identity = crate::Identity::generate().public_key(),
                4 => w.local.static_key = crate::StaticKey::generate().public_key(),
                5 => w.remote.as_mut().unwrap().identity = crate::Identity::generate().public_key(),
                6 => {
                    w.remote.as_mut().unwrap().static_key =
                        crate::StaticKey::generate().public_key()
                }
                7 => w.role = PairRole::Hub,
                _ => w.local.display_name = "changed name".into(),
            }
            assert!(w.receive(&m, 0).is_err());
            assert!(w.peer().is_err());
        }
    }
}
