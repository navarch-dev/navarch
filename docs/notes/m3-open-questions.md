# M3 security clarifications

1. Section 10 orders confirmation before exchanging public keys, but confirmation
must bind both roles, room, both SPAKE2 messages, and both identity and Noise
static public keys. Proposed and implemented conservative ordering: SPAKE2,
direction-separated authenticated encryption of both introductions, then mutual
role-separated keyed BLAKE2s confirmation over the entire ordered transcript.
No introduction is accepted or persisted before peer confirmation. This adjusts
section 10's ordering without exposing introductions to the relay.

2. Noise messages are limited to 65535 bytes; a 1 MiB envelope plus base64 and
JSON cannot fit a 1 MiB relay frame. Proposed and implemented: split each envelope
into ordered Noise records with authenticated total-length and offset headers.
Each plaintext chunk is at most 48 KiB; each relay JSON frame stays below 1 MiB.
Reassembly rejects gaps, overlaps, interleaving, oversize and empty fragments.
Limits apply to each relay frame, including JSON/base64 overhead.

3. Relay authentication needs domain separation and freshness: signatures cover
`navarch-relay-auth-v1` and a fresh 32-byte OS-random connection challenge, with a
10-second authentication deadline. Rooms have explicit host/join roles and an
outer step number; the relay never inspects the opaque pairing payload. Used
room numbers remain reserved until 15 minutes after their original creation,
including expiry, failure and success. Caller-supplied monotonic time drives
expiry and tombstone eviction, so wall-clock changes cannot shorten retention.
Each endpoint independently enforces the original ten-minute pairing deadline;
reclaiming a room after fifteen minutes cannot revive its old state machine.
The extra five minutes separate code expiry from routing-number reuse without
exhausting the 10000-number namespace over the relay's lifetime. Relay restart
loses rooms and tombstones; outstanding attempts are invalidated. Before showing
a code, the hub retries a different room on an explicit `room_busy` response.
Other errors destroy the allocation; an actual pairing attempt is never retried.

4. A malicious relay can fork its room state and grant multiple online guesses.
The one-attempt guarantee therefore assumes an honest relay enforcing rooms;
PAKE prevents offline guessing, but cannot enforce relay-side global state.
The hub state machine itself permits only one attempt per generated code.

5. Noise KK does not transmit static keys: the hub selects an already stored
active peer's key using the authenticated relay source ID before invoking Noise.
An unknown/pending/revoked source is rejected before any Noise processing.
Revocation is checked on every frame, including acknowledgements.

6. Pairing CLI commands use transient authenticated relay identities to avoid
replacing the running hub/worker connection (the relay allows one connection per
identity). Persistent identities/static keys remain inside the confirmed
transcript. Relay labels are not used as cryptographic pairing identity proof.

7. The hub relay listener remembers worker freshness requests across relay reconnects
within the process for the ten-second handshake deadline window, measured with
monotonic time. Expired entries are evicted; a secondary cap of 65536 entries
evicts oldest-first rather than refusing new handshakes. Duplicate requests in
the retained window are rejected without extending their lifetime. At extreme
request volume the size cap can evict an entry before ten seconds; this is safe
because the history is an additional request filter, not the authentication
boundary. After eviction or restart, replay of a nonce request can only obtain
a fresh responder nonce. A recorded KK first message cannot establish a
connection against that fresh two-sided prologue (see item 10) or establish
traffic keys against a fresh responder ephemeral key. Lifetime admission limits
therefore add denial of service without strengthening authentication.
Snow/SPAKE2 do not expose full internal zeroization; owned secret copies are
erased and supported dalek/cipher erasure is used without modifying primitives.

8. Relay URL changes take effect when hub serve starts/restarts. No public hosted
relay is assumed. A paired worker uses worker/hub.json unless local_hub_socket is
explicit in worker.toml. Existing/revoked key pins cannot be replaced by pairing.
The manual real-agent acceptance check is left to the reviewer as requested.

9. Pairing persistence goes through a private local control operation on the
running hub daemon, so it is serialized with approval/revocation and capability
reports. The CLI never directly mutates machines.json in another process; M2's
FileStore locks are process-local. Thus pair requires hub serve to be running.
Worker hub-pin publication is atomic and no-clobber even for concurrent joins.

10. Bare Noise KK permits replaying the initiator's first message to a responder
with no remembered history. Although fresh responder ephemerals prevent reading
transport traffic, treating such a replay as an authenticated connection conflicts
with the required handshake rejection rule. Proposed conservative change: before
KK, exchange fresh OS-random 32-byte worker/hub connection nonces, and bind both
ordered nonces, roles and machine IDs through snow's standard prologue API.
Unexpected first KK messages are dropped unless a responder challenge is pending;
a recorded KK first message fails under a fresh prologue, including after process
restart. The bounded process history additionally rejects replayed worker nonce
requests, but security across restarts no longer relies on replay history.

11. Section 11 does not specify a rate-limit direction. The conservative choice
is independent rolling budgets in each direction of each connection, counting
authentication/control frames too. A recipient cannot receive unlimited traffic
by having many senders each stay under their own incoming budget. Exceeding either
direction's budget closes the connection; the relay never buffers offline frames.

12. Relay operational events are structured info lines: listening address,
authenticated machine, connection closure with a fixed reason and lifetime
incoming/outgoing application-frame and UTF-8 JSON byte counts, room open/close
with a fixed outcome, and limit violation with direction/type. The relay CLI
enables info by default. No per-payload logging is necessary. Room numbers,
secrets, public keys, challenges and payloads are omitted; machine IDs are the
only key-derived identifiers. Pairing introductions reject empty names as well
as controls and names over 200 UTF-8 bytes. Only locally sourced hostname defaults
are stripped of controls and truncated before constructing an introduction;
received or explicitly configured names are rejected rather than repaired.
