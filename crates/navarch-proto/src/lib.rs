//! Shared identifiers for Navarch.

#![forbid(unsafe_code)]

use std::{fmt, str::FromStr};

use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// A machine's canonical lowercase, unpadded base32 identifier (16 bytes).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MachineId(String);

/// A string is not a canonical machine identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("machine ID must be the lowercase, unpadded base32 encoding of 16 bytes")]
pub struct ParseMachineIdError;

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for MachineId {
    type Err = ParseMachineIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 26
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
        {
            return Err(ParseMachineIdError);
        }

        // The decoder also rejects nonzero trailing bits in the last symbol.
        let decoded = BASE32_NOPAD
            .decode(value.to_ascii_uppercase().as_bytes())
            .map_err(|_| ParseMachineIdError)?;
        if decoded.len() != 16 {
            return Err(ParseMachineIdError);
        }

        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for MachineId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A task identifier, represented as a UUID and serialized as a string.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(Uuid);

/// A string is not a UUID task identifier.
#[derive(Debug, thiserror::Error)]
#[error("task ID must be a UUID")]
pub struct ParseTaskIdError(#[from] uuid::Error);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TaskId {
    type Err = ParseTaskIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.parse()?))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_round_trips() {
        for bytes in [[0; 16], [255; 16], *b"navarch-machine!"] {
            let text = BASE32_NOPAD.encode(&bytes).to_ascii_lowercase();
            let id: MachineId = text.parse().unwrap();
            assert_eq!(id.to_string(), text);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<MachineId>(&json).unwrap(), id);
        }
    }

    #[test]
    fn machine_id_rejects_invalid_strings_in_parsing_and_serde() {
        for text in [
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "AAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaa0",
            "aaaaaaaaaaaaaaaaaaaaaaaaa1",
            "aaaaaaaaaaaaaaaaaaaaaaaaa8",
            "aaaaaaaaaaaaaaaaaaaaaaaaa=",
            "aaaaaaaaaaaaaaaaaaaaaaaaab", // Nonzero unused trailing bits.
            "aaaaaaaaaaaaaaaaaaaaaaaaa ",
            "aaaaaaaaaaaaaaaaaaaaaaaaé",
        ] {
            assert!(text.parse::<MachineId>().is_err(), "accepted {text:?}");
            let json = serde_json::to_string(text).unwrap();
            assert!(serde_json::from_str::<MachineId>(&json).is_err());
        }
        assert!(serde_json::from_str::<MachineId>("42").is_err());
    }

    #[test]
    fn task_id_round_trips() {
        let text = "67e55044-10b1-426f-9247-bb680e5fe0c8";
        let id: TaskId = text.parse().unwrap();
        assert_eq!(id.to_string(), text);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<TaskId>(&json).unwrap(), id);
    }

    #[test]
    fn task_id_rejects_invalid_strings_in_parsing_and_serde() {
        for text in ["", "task-1", "67e55044-10b1-426f-9247-bb680e5fe0cg"] {
            assert!(text.parse::<TaskId>().is_err());
            let json = serde_json::to_string(text).unwrap();
            assert!(serde_json::from_str::<TaskId>(&json).is_err());
        }
        assert!(serde_json::from_str::<TaskId>("42").is_err());
    }
}

mod messages;
pub use messages::*;

impl TaskId {
    /// Generate a random task identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// A user-facing work card, distinct from an individual wire turn.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CardId(Uuid);
impl CardId {
    /// Generate a new card identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for CardId {
    fn default() -> Self {
        Self::new()
    }
}
impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for CardId {
    type Err = uuid::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}
/// Collapse whitespace and bound human-facing summaries to 200 characters.
pub fn short_summary(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}
/// Serde compatibility for public keys: write lowercase hex, read hex or legacy arrays.
pub mod public_key {
    use serde::{Deserialize, Deserializer, Serializer};
    /// Serialize a 32-byte public key as lowercase hex.
    pub fn serialize<S: Serializer>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write;
        let mut text = String::with_capacity(64);
        for byte in key {
            let _ = write!(text, "{byte:02x}");
        }
        serializer.serialize_str(&text)
    }
    /// Deserialize current hex strings or legacy byte arrays.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Key {
            Hex(String),
            Array([u8; 32]),
        }
        match Key::deserialize(deserializer)? {
            Key::Array(key) => Ok(key),
            Key::Hex(text) => {
                let mut key = [0; 32];
                if text.len() != 64 || !text.is_ascii() {
                    return Err(serde::de::Error::custom(
                        "public key must be 64 hex characters",
                    ));
                }
                for (i, byte) in key.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                        .map_err(serde::de::Error::custom)?;
                }
                Ok(key)
            }
        }
    }
}
