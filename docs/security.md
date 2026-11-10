# Security model

Navarch trusts the hub to choose tasks, each worker's local user and policy, the
operating system RNG, the installed binary/dependencies, and the privacy of each
Navarch home directory. It does not trust the relay with plaintext or peer key
selection. Use TLS for the relay outside loopback to protect connection metadata
in transit and authenticate the relay endpoint. TLS does not replace end-to-end
Noise authentication. All identities and static public keys are lowercase hex;
MachineId is lowercase unpadded base32 of the first 16 bytes of BLAKE2s-256 of the
Ed25519 public key.

| Attacker | Can | Cannot |
|---|---|---|
| Malicious relay | Observe machine IDs, rooms, sizes, timing and public SPAKE2 messages; fabricate routing metadata; disconnect, reorder, replay, delay or drop traffic; deny service | Decrypt pairing introductions or channel traffic, forge confirmed pairings without the password, or complete authenticated Noise as a pinned peer without its static secret |
| Someone learning an old pairing code | Attempt another room lookup or cause denial of service | Reopen a tombstoned room or decrypt recorded pairings/channels from the password alone |
| Someone guessing a live code | Make one online guess per honest relay room (six-digit secret, probability 1 in 1000000), close the room on failure | Make offline password guesses against SPAKE2 or gain active worker privileges without approval |
| Compromised hub | Send tasks and answer escalations within the worker's configured policy | Bypass worker root/agent allowlists or execute arbitrary commands through a separate hub execution API |
| Compromised worker | Lie about its work and own capability reports; flood the relay within enforced budgets | Change another worker's cards, impersonate its keys, or reach its sessions through the hub |
| Local account or home-directory compromise | Read keys, public pins, session history and tasks; replace binaries/configuration | Be contained by transport encryption once the endpoint itself is compromised |

Relay authentication prevents identity spoofing at an honest relay. A malicious
relay can forge routing labels or forward authentication challenges; an Ed25519
challenge signature is not proof of end-to-end channel authentication. Noise is
that boundary. The global one-attempt-per-room rule relies on honest room state;
a malicious relay can fork room metadata. Independently, each hub pairing state
permits only one attempt, and any error permanently destroys it. A user must
never retry the same displayed code after a failed attempt; generate a fresh code.

# Pairing transcript

SPAKE2 uses the library's Ed25519 group with hub as A, worker as B and
role/room-specific SPAKE2 identities. The six-digit password never goes to the
relay. Pairing is an ordered six-message state machine:

1. Hub SPAKE2 message (33 bytes).
2. Worker SPAKE2 message (33 bytes).
3. Hub encrypted introduction.
4. Worker encrypted introduction.
5. Hub transcript confirmation.
6. Worker transcript confirmation.

ChaCha20-Poly1305 encrypts introductions using a key derived with keyed BLAKE2s
from the SPAKE2 result and the fixed `navarch-pair-v1:encryption` domain. Nonces
are distinct for the two introductions (last byte 0/1 in a twelve-byte nonce).
AAD contains the protocol domain, both fixed role labels, the four-byte room and
both ordered SPAKE2 messages. It is safe to use these fixed per-role nonces only
because every state machine uses a fresh, single-use SPAKE2 key.

Confirmation is the crate's keyed BLAKE2s-256 MAC over the separate
`navarch-pair-v1:confirmation` domain, the confirming role, that AAD transcript,
and length-prefixed canonical JSON introductions in hub/worker order. Each
introduction includes its Ed25519 public key, X25519 static public key, and name.
Both identities, both static keys, both roles, room and both SPAKE2 messages are
therefore bound before either peer is exposed for persistence. The crate's
`verify_slice` performs constant-time verification. AEAD tags and Noise tags are
verified by their respective crates. No custom cryptographic primitive is used.

Malformed, truncated, replayed, reflected, out-of-order, expired or tampered
pairing messages permanently invalidate the attempt. Each transition checks the
original ten-minute monotonic deadline; the relay also expires the original room.
The code text carries no timestamp, so parsing alone cannot determine expiry.

# Channels, storage and revocation

The exact pattern and parameter string is
`Noise_KK_25519_ChaChaPoly_BLAKE2s`, implemented by `snow`. Each side pins the
other's X25519 static public key. Every reconnection generates new ephemeral
keys. Before KK the worker sends a fresh 32-byte OS-random nonce (record kind 3),
and the hub replies with its own fresh nonce (kind 4). The standard Noise prologue
is `navarch-channel-v1\0worker\0hub\0 || worker ID || hub ID || worker nonce || hub
nonce` (IDs are 26 ASCII bytes each). KK messages use kinds 0/1 and transport
records kind 2. All prelude message lengths and ordering are strict. A recorded
first KK message fails under the fresh prologue after reconnect or process restart.
Rekey behavior is left to snow's defaults. Application records are ordered;
a failed tag, malformed header, gap, overlap, replay or unexpected record type
closes the channel. The hub rejects unknown/pending/revoked sources before Noise
processing and checks approval again before passing any frame, including an ack,
to reliable delivery. The responder keeps a bounded, non-evicting process-wide
freshness-request replay history (16384 worker nonce requests); exhaustion refuses
fresh handshakes until a process restart. After process restart a replayed nonce
request can only solicit a fresh challenge;
a recorded first KK message fails verification under the new prologue.

Operating-system randomness supplies identities, static keys, SPAKE2 randomness,
Noise ephemeral keys, challenges, codes and reconnect jitter. Secret file creation
uses private same-directory temporary files and atomic no-clobber publication.
Existing symlinked, nonregular, incorrectly sized or non-0600 key files are
rejected. Secret copies owned by Navarch use zeroizing wrappers; dalek secrets
and ChaCha cipher state erase their keys where the crates support it. `snow` and
`spake2` do not expose guaranteed erasure of all internal state; Navarch does not
modify their primitives or claim that every compiler/register copy is erased.

To revoke a machine immediately:

```sh
navarch hub machines revoke MACHINE_ID
```

The daemon persists `revoked`, rejects further traffic and outbound work, and
refuses to approve or re-pair that identity. The worker cannot resume work through
an existing Noise channel. Revocation does not cancel already running agent work
or erase data already delivered; use task cancellation before revoking if needed.
Restore a compromised endpoint with a fresh identity and pair/approve it anew.
Protect backups as carefully as live homes. Public-key pin replacement and key
rotation are intentionally outside M3.

Logs must not contain pairing secrets, keys, plaintext payloads, or opaque relay
payloads. Relay records use only machine IDs, sizes and fixed event names; remote
protocol failures use generic errors. Pairing-code display is intentional CLI
output, not a tracing event. Avoid capturing that terminal output in shared logs.
The limitations and ordering/record choices are recorded in
[the M3 security notes](notes/m3-open-questions.md).
