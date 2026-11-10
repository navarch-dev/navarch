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
#[error("task ID must be a UUID: {0}")]
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
