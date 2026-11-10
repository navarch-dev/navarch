# Running the relay

Build the workspace, then run:

```sh
navarch relay serve --listen 127.0.0.1:8080
```

The server exposes WebSockets at `/v1/ws` and a plain-text `ok` health check at
`/healthz`. It stores no messages or keys on disk. Clients authenticate with a
fresh 32-byte challenge and an Ed25519 signature over
`navarch-relay-auth-v1 || challenge`; authentication has a ten-second deadline.
The machine ID comes from the verified identity public key. A new authenticated
connection replaces the previous connection for that machine.

Use a TLS reverse proxy for any non-loopback deployment. For example, a Caddy
configuration (replace the example hostname with a domain you control):

```caddyfile
relay.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Keep the upstream port private. Permit WebSocket upgrades and long-lived
connections; avoid request-body logging, WebSocket payload logging, and access-log
query strings containing credentials. Clients use
`wss://relay.example.com/v1/ws`. The upstream server intentionally serves HTTP;
TLS certificate validation is performed by the client's rustls WebSocket stack.

The relay limits each complete JSON WebSocket message and each WebSocket frame to
1 MiB, counts at most 60 application messages in a sliding second, and at most
16 MiB in a sliding minute in each direction per connection, including
authentication and control messages. Exceeding a limit or
sending a malformed control message closes the connection. Outbound queues hold
at most 64 frames; overload fails closed. Unknown destinations produce an offline
response; the relay does not buffer undelivered traffic. Logs contain only machine
IDs, counts/sizes, fixed event descriptions, and the startup listening address.
The relay CLI enables info by default. Each event is one structured line:
`listening` (address), `connection_authenticated` (machine ID),
`connection_closed` (machine ID, fixed reason, incoming/outgoing frame and byte
counts), `room_opened` (host machine ID), `room_closed` (host machine ID and
outcome), and `limit_exceeded` (direction and limit type). Frame/byte totals cover
application text messages, including authentication/control: incoming totals
count messages read (including rejected text); outgoing totals count successful
writes. Bytes are UTF-8 JSON sizes, excluding WebSocket/TCP overhead. Room
numbers, secrets, challenges, public keys and payload bytes are never logged.
A `completed` room outcome means all six ordered relay steps were queued;
endpoints verify cryptographic confirmation, and machine approval is separate.

Rooms contain one host and one joiner, expire after ten minutes, and close after
failure, disconnect, success, a third joiner, or an invalid ordering marker. A room
number remains reserved until fifteen minutes after its original creation,
regardless of when it closes, then becomes reusable. Eviction uses monotonic
time; endpoint pairing state machines independently enforce their original
ten-minute deadlines. Reusing the routing number cannot revive an expired
attempt. Restarting invalidates outstanding rooms and clears tombstones. The relay sees only room/step metadata and base64 ciphertext
or SPAKE2 bytes; it never parses pairing payload contents.

# Pairing and operating remote workers

For a loopback-only reviewer setup, use separate absolute directories for hub and
worker. No commands below need a real agent unless you explicitly configure one.

```sh
NAVARCH_HOME=/tmp/navarch-hub navarch hub serve
# In another terminal, while the daemon is running:
NAVARCH_HOME=/tmp/navarch-hub navarch pair --relay ws://127.0.0.1:8080/v1/ws
# In another terminal, substitute the printed code:
NAVARCH_HOME=/tmp/navarch-worker navarch join RRRR-SSSSSS --relay ws://127.0.0.1:8080/v1/ws
# Restart hub serve after pair first writes hub.toml, then in another terminal:
NAVARCH_HOME=/tmp/navarch-hub navarch hub machines
NAVARCH_HOME=/tmp/navarch-hub navarch hub machines approve MACHINE_ID
NAVARCH_HOME=/tmp/navarch-worker navarch worker init
# Edit worker.toml and worker-policy.toml, then:
NAVARCH_HOME=/tmp/navarch-worker navarch worker serve
```

`pair` requires the running hub daemon and prints only after reserving its room.
On `room_busy` it tries a different room, pacing requests within relay limits;
it never retries a failed pairing attempt. It waits for one attempt and saves a
confirmed machine as pending in `hub/machines.json`. It saves the relay URL in
`hub.toml` (`relay = "wss://.../v1/ws"`). `pair` without `--relay` uses that setting;
there is no default hosted relay. Start or restart `hub serve` after changing the
relay URL. Pairing uses a transient relay identity so it can run while the hub's
persistent relay connection remains online; the persistent keys are authenticated
inside the pairing transcript.

`join` saves the confirmed hub identity/static public keys and URL in
`worker/hub.json`; it refuses to overwrite an existing pairing. If `worker.toml`
is absent, its display name is the system hostname, with control characters
removed and truncation to 200 UTF-8 bytes on a character boundary. An empty name
is rejected. Explicitly configured and received names must already be nonempty,
control-free and within that bound; invalid names are never repaired. Successful
join output explains the next steps: `navarch worker init`, edit `worker.toml`
and `worker-policy.toml`, then `navarch worker serve`. Remote worker
serving uses that file unless `local_hub_socket = "/absolute/path"` is explicitly
set in `worker.toml`. An unpaired worker retains the M2 local socket default.
Key files are `identity.key` and `noise.key`, both atomic 0600 files. Approval and
revocation commands use the running daemon's control API. The MCP
`approve_machine` tool has the same approval behavior.

The worker is the Noise KK initiator; the hub is the responder and pins the
active machine's stored static key before processing a handshake. Both sides run
fresh KK on reconnect, binding fresh worker/hub connection nonces and both role
addresses through snow's standard prologue. Reliable delivery retains envelope IDs and retransmits the
outbox. Noise records contain encrypted total-size and offset headers with at
most 48 KiB of envelope bytes each, so a maximum 1 MiB envelope fits despite
Noise tags, record markers, base64, and JSON overhead. Reassembly accepts only
contiguous records for one bounded envelope. The relay's rate/byte budgets still
apply; excessive traffic disconnects and reconnects rather than bypassing limits.

`navarch hub machines` prints ID, state, online status and name as a readable
table. Add `--json` for the structured list. `hub machines approve MACHINE_ID`
and `hub machines revoke MACHINE_ID` each print one plain confirmation sentence.

The hub retains freshness-request nonces for the ten-second handshake deadline
window across relay reconnects. A secondary 65536-entry cap evicts oldest-first;
replays still retained in the window are rejected. Fresh two-sided Noise
prologues defeat recorded handshakes independently of this extra cache, including
after time/cap eviction or restart. See [security.md](security.md).
