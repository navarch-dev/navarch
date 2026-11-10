//! Bounded relay control messages. Payloads are opaque base64 strings.
use crate::MachineId;
use serde::{Deserialize, Serialize};
/// Maximum encoded WebSocket message, including all JSON and base64 overhead.
pub const MAX_RELAY_FRAME: usize = 1024 * 1024;
/// Client requests. No payload Debug implementation is exposed.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayRequest {
    /// Prove possession of the identity key for a fresh connection challenge.
    Authenticate {
        /// Public identity.
        #[serde(with = "crate::public_key")]
        public_key: [u8; 32],
        /// Base64 Ed25519 signature.
        signature: String,
    },
    /// Reserve a room as its hub.
    RoomOpen {
        /// Four decimal digits.
        room: String,
    },
    /// Join an existing room as its worker.
    RoomJoin {
        /// Four decimal digits.
        room: String,
    },
    /// Ordered opaque pairing step, 1 through 6.
    RoomData {
        /// Room number.
        room: String,
        /// External ordering marker.
        step: u8,
        /// Opaque base64 bytes.
        payload: String,
    },
    /// Close an unsuccessful attempt immediately.
    RoomFail {
        /// Room number.
        room: String,
    },
    /// Route opaque encrypted bytes.
    #[serde(untagged)]
    Route {
        /// Destination.
        to: MachineId,
        /// Opaque base64 bytes.
        payload: String,
    },
}
/// Server responses; never format these for logs.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayResponse {
    /// Fresh random nonce.
    Challenge {
        /// Base64 32-byte nonce.
        challenge: String,
    },
    /// Authentication completed.
    Authenticated,
    /// Room reserved, awaiting worker.
    RoomOpened,
    /// Room number is still reserved; the host may allocate a different code.
    RoomBusy,
    /// Exactly two authenticated clients have joined.
    RoomReady {
        /// Remote identity.
        peer: MachineId,
    },
    /// Opaque pairing bytes.
    RoomData {
        /// External ordered step.
        step: u8,
        /// Opaque base64 bytes.
        payload: String,
    },
    /// Failed, expired or unavailable room. No attempt may be restarted.
    RoomClosed,
    /// Routed bytes and authenticated source.
    #[serde(untagged)]
    Routed {
        /// Source ID derived from its authenticated public key.
        from: MachineId,
        /// Opaque base64 bytes.
        payload: String,
    },
    /// Routing failure (no buffering).
    #[serde(untagged)]
    Offline {
        /// Fixed offline error code.
        error: OfflineError,
        /// Unconnected destination.
        to: MachineId,
    },
}

/// The offline routing error code.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineError {
    /// Destination has no registered connection.
    Offline,
}
