# Local hub (M2)

Navarch M2 runs a hub and one worker on a Unix machine. The hub owns task cards,
configuration and unread events; the worker uses the same runtime as the
standalone commands and enforces its own policy. No relay, pairing or encrypted
remote connection is enabled in M2.

## Start on one machine

Build the binary and write examples:

```sh
cargo build -p navarch-cli
export PATH="$PWD/target/debug:$PATH"
navarch hub init
navarch worker init
```

All processes must use the same `NAVARCH_HOME` (default `~/.navarch`). The CLI
creates that directory with mode 0700. `hub init` creates `hub/projects.toml`,
`hub/routes.toml` and `hub/roles/developer.md` without overwriting existing files.
Project and route examples are commented out. Roles use `+++` TOML front matter
(`agent`, optional `model`/`mode`) followed by Markdown preferences. Preferences
are appended to a role-based dispatch prompt. Routes are hints for the
orchestrator; the hub never chooses a worker or role automatically.

Edit `worker.toml` and `worker-policy.toml` as described in [worker.md](worker.md).
Set actual adapter commands, allowed agents and canonicalizable allowed roots.
No allowed roots are configured by default. Navarch does not install adapters.
`navarch worker doctor` checks executable presence without starting agents;
`worker serve` probes configured adapters on connection and therefore does start
present adapters. Running these commands with real adapters can consume quota.

Run these in separate terminals:

```sh
navarch hub serve
navarch worker serve
```

Then inspect the daemon:

```sh
navarch hub status
navarch hub tasks
navarch hub tasks --all
navarch hub status --json
navarch hub tasks --all --json
```

Status and tasks print readable tables by default; `--json` prints their structured
control results. Task listings contain card summaries without logs.

On first local connection the worker is recorded as active, with its display name
and public key. Public keys are lowercase 64-character hex strings in machine
files, control results, MCP output and local introductions. Legacy byte-array keys
are still accepted when reading. Hub and local worker may share the same machine identity.
`identity.key` contains 32 Ed25519 secret bytes and has mode 0600. The machine ID
is lowercase unpadded base32 of the first 16 bytes of BLAKE2s-256 of the public
key. An existing corrupt key produces an error and is preserved.

The hub creates `hub.sock` (control) and `hub-worker.sock` (worker frames), both
mode 0600. Ctrl-C stops serving and removes owned socket paths. After an abrupt
exit, startup replaces stale sockets, preserving regular files, symlinks and live
listeners. Stop an existing daemon before starting another one. If a Unix socket
path exceeds the platform limit, the error names the path and byte limit; set
`NAVARCH_HOME` to a shorter directory.

A worker reconnects automatically with jittered exponential delays of 1–60
seconds. Its in-memory outbox keeps stable envelope IDs until acknowledged and
resends them after reconnection. Encoded envelopes are checked before queuing;
a message above 1 MiB returns an explicit size error without disconnecting.
An oversized legacy outbox entry is logged and dropped; a worker turn event is
replaced with a deliverable `Failed` event so its card cannot wait forever.
Acknowledgements follow application of events to
hub state. A live worker can finish a turn while the hub is down and deliver the
result after it returns. Restarting the worker itself does not preserve its
in-memory delivery outbox. Durable event-ID metadata on cards prevents replayed events from
reverting finished cards or duplicating events after a hub restart. Transport
remembers the most recent 4096 received envelope IDs per peer; it does not promise
message ordering. The hub handles late Accepted/Progress events without reverting
a terminal state.

## Register MCP in Claude Code

Start the hub and worker first. Replace the binary path and state directory with
absolute paths:

```sh
claude mcp add --transport stdio --scope user \
  --env NAVARCH_HOME=/absolute/path/to/navarch-state \
  navarch -- /absolute/path/to/navarch hub mcp
```

See [Claude Code's stdio MCP registration documentation](https://code.claude.com/docs/en/mcp#option-3-add-a-local-stdio-server)
for registration scopes. Check `/mcp` in Claude Code. `hub mcp` uses the official
[`rmcp` SDK](https://github.com/modelcontextprotocol/rust-sdk) and holds no hub
state. It translates tools to control requests. The MCP process stays alive when the daemon is absent; tool calls report the
socket path and ask you to run `navarch hub serve`. A channel subscription
reconnects automatically. Stdout carries only MCP traffic.

No real adapter or Claude Code session is needed by the automated tests. The
reviewer acceptance check is to dispatch a small task through MCP and then send a
follow-up to its session using an authenticated adapter.

## Tools and example arguments

The MCP tool name is shown before each JSON argument object. Replace `MACHINE`
with an ID returned by `list_workers`, `TURN_UUID` with a returned `task_id`,
`CARD_UUID` with a returned `card_id`, `SESSION` with a worker session ID, and
`REQUEST_UUID` with a permission request UUID. These examples are reference calls, not commands to run as a shell script.

| Tool | Behavior | Example arguments |
|---|---|---|
| `list_workers` | Known machines, approval/online state and last capabilities. | `{}` |
| `list_sessions` | Query worker registries and adapters, newest first; filter by machine, agent and cwd. Limit defaults to 50 per worker and is capped at 200. Reports `truncated` and `stale`; a timeout (10 seconds) or offline worker uses hub-observed sessions. | `{"machine":"MACHINE","agent":"codex","cwd":"/absolute/repo","limit":50}` |
| `dispatch` | Create a card and turn on an explicit machine; returns `card_id` and `task_id`. Pass `card_id` to append a hand-off step. Supply exactly one of `agent` or `role`. | `{"machine":"MACHINE","agent":"codex","cwd":"/absolute/path/to/repo","prompt":"Describe the test setup","title":"Inspect tests"}` |
| `send_to_session` | Append to the session’s latest owning card; optional `card_id` overrides it. Returns `card_id` and a new turn `task_id`. Supply `agent` and an adapter session ID to adopt an external session. | `{"machine":"MACHINE","session_id":"SESSION","prompt":"Now explain the main entry point"}` |
| `cancel` | Cancel the task's active turn or remove it from the session queue. | `{"task_id":"TURN_UUID"}` |
| `get_task` | Authoritative card with state and ordered steps; log omitted by default. `task_id` here is the card UUID. | `{"task_id":"CARD_UUID","include_log":true}` |
| `list_tasks` | Active card summaries without logs by default; report unreadable cards without hiding good ones. | `{"include_finished":true}` |
| `read_inbox` | Return unread Finished/Failed/Rejected/PermissionAsk events and mark that batch read. | `{}` |
| `answer_permission` | Select an offered option or cancel an outstanding request. | `{"request_id":"REQUEST_UUID","decision":{"type":"selected","option_id":"offered-option-id"}}` |
| `approve_machine` | Approve a known pending machine; local machines need no approval. Remote pairing is M3. | `{"machine":"MACHINE"}` |

A role dispatch can instead use
`{"machine":"MACHINE","role":"developer","cwd":"/absolute/repo","prompt":"Implement the requested change"}`.
Optional `project` must name a configured project or alias. Optional `mode`,
`model` and `mcp_servers` override/provide session settings. A server entry has
`name`, `command`, optional `args` and optional `env`, with literal arguments.
The hub validates machine approval, online state, reported agent and non-empty
cwd; filesystem and agent allowlists remain worker decisions. Dispatch and
send-to-session reject an encoded envelope above 1 MiB before saving a queued
card or sending anything. This check includes JSON escaping, session settings
and appended role preferences; shorten the prompt or other payload when the
error reports that it cannot fit.

Permission choices must come from that request's `options`. To cancel, use
`{"request_id":"REQUEST_UUID","decision":{"type":"cancelled"}}`.
Permissions still expire after ten minutes on the worker. An expired choice or a
request for a completed turn cannot authorize later work.

A card represents one piece of work and has a distinct `CardId`. Each ordered
step is an independently queued/cancellable turn, with its own wire `task_id`,
role, agent, machine, session, state and result summary. `dispatch` starts a new
card unless `card_id` names an existing one. `send_to_session` appends to the
card owning that session’s most recently submitted step unless overridden. For
external sessions with no owning card, the first send creates one.

Card state is derived: `running` if any step is running or queued, then `waiting`
if any step waits for permission, then `failed` if the latest step failed,
otherwise `finished`. Rejection makes the step failed; a cancelled step is
terminal and does not by itself fail the card. Cancel uses the turn ID returned
as `task_id`; get_task uses the card ID returned as `card_id` in its `task_id`
argument. A hand-off can change agent and machine without changing card identity.

The compact card log contains only `time` (Unix milliseconds), `kind`, `step`
(turn ID) and `summary` (at most 200 characters). It never stores envelopes or
tool payloads. Durable deduplication IDs and permission routing metadata live
separately on the card and are omitted from tool results. `get_task` omits the log
unless `include_log: true`; `list_tasks` returns summaries with no steps or logs.
Worker progress uses human-readable tool titles and status or plan text, capped
at 200 characters and coalesced to at most one message per second per turn.

`list_sessions` returns agent, adapter session ID, optional public worker session
ID, cwd, title, updated timestamp and `navarch_started`, plus machine and `stale`.
The response also includes aggregate `truncated` and `stale` flags. Agent and cwd
filters apply to both live results and stale fallback. `limit` defaults to 50 per
worker and is capped at 200 (zero returns no entries). Results are ordered by
most recent update, with unknown timestamps last. Adapter pagination stops once
that adapter has supplied enough matching sessions for the query. Registry
identity is preferred when merging duplicates. Titles are cut to 120 Unicode
characters, and lists are shortened further until the full encoded worker
envelope fits within 256 KiB. `truncated: true` means additional sessions may
exist or entries were removed to fit that budget; shortening titles alone does
not set it. An external session is addressed with `agent` and
its `adapter_session_id` passed as `session_id`. The worker canonicalizes its
reported cwd and checks `allowed_roots` and `allowed_agents` before adoption or
resume. Adoption allocates a public worker session ID and retains
`navarch_started: false` in the registry.

Worker final results are capped at 64 KiB of UTF-8 bytes on a character boundary.
Progress is capped at 200 characters. Permission tool titles and option labels
are capped at 200 characters, detail at 16 KiB, and choices at 32; choice IDs
over 1024 bytes are omitted rather than rewritten. If no representable choice
remains, the worker cancels the permission request. Capability reports contain
at most 200 repositories and 100 models per agent, with bounded descriptive
strings; entries are removed if needed to fit a 256 KiB encoded envelope.
Agent/model IDs over 1024 bytes are omitted rather than rewritten.
Opaque session IDs and paths are preserved; undeliverable turn metadata
produces `Failed`.

The derived `hub/tasks/index.toml` contains only active cards. The file store
rebuilds the index on opening and on every card write. Old one-turn cards without
per-step turn IDs are preserved, ignored on direct reads and reported as
unreadable in listings/index rebuilds; they do not prevent current cards from
loading. User configuration files are not rewritten on open.

## Control protocol

`hub.sock` accepts one JSON request per line and returns one response per line.
Use a `type` matching the tool name, with the same arguments. For example:

```json
{"type":"list_workers"}
{"status":"ok","value":[]}
```

Failures use `{"status":"error","message":"..."}`. A connection may carry
multiple requests. Input lines and worker frame payloads are limited to 1 MiB;
worker frames have a four-byte big-endian length prefix. Oversized control
requests return a size error asking the caller to shorten the prompt or payload.
Control types live in
`navarch_hub::control` and contain no MCP SDK types. Requests and events update
state under one hub runtime lock; file writes use same-directory temporary files
and atomic rename. FileStore instances opened on the same canonical directory
share a writer lock within the process.

See [M2 boundary decisions](notes/m2-deviations.md) for transport and layering
decisions.

## Remote machines (M3)

Pairing, approval and encrypted relay operation are described in
[relay.md](relay.md). The hub daemon must be running for pairing and approval.
Inspect and manage machines with:

```sh
navarch hub machines
navarch hub machines --json
navarch hub machines approve MACHINE_ID
navarch hub machines revoke MACHINE_ID
```

The default list is a readable table with ID, state, online status and name.
`--json` returns the structured worker list, including stored public pins and
capabilities. Successful approval prints `Approved machine MACHINE_ID.`;
revocation prints `Revoked machine MACHINE_ID.`. Both use the daemon control API.

The hub allocates another room on an explicit busy response before displaying
a pairing code. Rooms are reserved for fifteen minutes from creation, while
endpoint pairing state machines enforce ten-minute code deadlines. Failed
pairing attempts are never retried with the same code. With no `worker.toml`,
`navarch join` uses a bounded, control-free OS hostname as the worker name;
empty or invalid introductions are rejected. After joining, run `navarch worker
init`, edit `worker.toml` and `worker-policy.toml`, and run `navarch worker serve`.

Freshness-request replay history lasts for the ten-second handshake deadline,
with a 65536-entry oldest-first secondary cap. Fresh two-sided Noise prologues
keep recorded handshakes invalid even after history eviction or hub restart;
network reconnections do not consume a lifetime handshake allowance.

## Getting told when work finishes

The inbox and task cards are authoritative. Events are bounded hints and never mark
anything read or approve a permission. Each event has a persisted monotonic `seq`.
There are three ways to follow work:

- **Claude Code channel:** register the stdio server under the name `navarch`,
  passing `hub mcp --channel` (or set `NAVARCH_CHANNEL=1` on the server):

  ```sh
  claude mcp add --transport stdio --scope user navarch -- /absolute/path/to/navarch hub mcp --channel
  MCP_PROTOCOL_NEGOTIATION=legacy claude --dangerously-load-development-channels server:navarch
  ```

  Claude Code receives a block such as `<channel source="navarch" kind="finished"
  card_id="..." machine="..." seq="...">` with a short instruction to call
  `get_task`, or `read_inbox` and `answer_permission`. Finished, failed, rejected
  and permission notifications include the event summary (at most 200 characters),
  so the outcome is visible immediately. Each new MCP process starts at the daemon's
  current sequence and sends only subsequent events; it does not replay the unread
  backlog. Old events remain available through `read_inbox` and `wait_for_events`.
  Notifications arrive without polling and do not require an acknowledgement.
  The MCP process stays alive when
  the daemon is down and reconnects with backoff, resuming after its last delivered
  sequence. Without `--channel` or `NAVARCH_CHANNEL=1`, it sends no channel notifications.
  Channels are a Claude Code research-preview feature, require a claude.ai or
  Console login, and may change. Set `MCP_PROTOCOL_NEGOTIATION=legacy` on the
  **Claude Code launch**, because channel registration requires a protocol revision
  older than `2026-07-28`. rmcp's default is `2025-11-25`, but it echoes known
  requested revisions, including `2026-07-28`.
- **Any MCP client:** after dispatching, call `wait_for_events` when there is
  nothing else to do. It returns early when unread newer entries exist, or returns
  `[]` after `timeout_secs` (default 300, capped at 1800). Supply `after_seq` to
  exclude older entries. Each returned event contains only `seq`, `kind`,
  `card_id`, `step`, `machine`, `summary` and `time`, never a worker envelope.
  Call `get_task` for full results, or `read_inbox` for permission request IDs and
  offered options. It does not mark entries read; `read_inbox` still does. Large inboxes are returned in batches that fit the control frame limit; pass the last returned `seq` to wait for the next batch. Each `read_inbox` marks only its returned batch read.
- **People and scripts:** run `navarch hub watch`, or `navarch hub watch --json`
  for raw JSON lines. Ctrl-C stops watching. It replays unread inbox entries and
  follows live events, including `worker_online` and `worker_offline` (these two
  are bus-only and do not enter the inbox).

A dedicated control connection starts streaming after
`{"type":"subscribe","after_seq":42}`. Replay includes unread inbox rows with
`seq > 42`; subsequent lines are live event objects. A slow subscriber receives
`{"kind":"lagged"}` and the stream closes: re-read the inbox and resubscribe.
The broadcast buffer holds 256 hints, so publishers never wait for subscribers.
A socket write stalled for ten seconds closes the subscriber connection. Existing
request/response control connections continue to work normally.

For a fresh channel process, `{"type":"subscribe_live"}` atomically registers
its receiver and snapshots the latest sequence under the hub's publication lock.
Its first line is `{"kind":"ready","seq":42}`, followed only by events newer
than that cursor, including events published while the ready line is being sent.
The channel saves this baseline even before its first notification; after a socket
drop it uses `subscribe` with the last delivered sequence (or that baseline).
There is no separate cursor-query/subscription gap.

Control responses also fit the 1 MiB line limit. If a large card or legacy log
exceeds it, the daemon returns an error and leaves the connection usable; for
example, retry `get_task` without `include_log` to request a smaller result.

If a worker envelope itself nearly fills the frame limit, the extra inbox fields
can make that single row too large for a control response. In that case,
`read_inbox` returns its compact fields with `envelope: null`
and `envelope_omitted: true`. The original envelope remains unchanged in
`hub/inbox.jsonl`; its card ID, step and sequence remain available to clients.
Subscription events and `wait_for_events` always contain only the seven compact
hint fields, without `id`, `envelope` or `envelope_omitted`.

## Direct connections

The hub opens no TCP port by default. To allow paired workers to connect over a LAN, VPN or tailnet, add this to `~/.navarch/hub.toml` and restart `navarch hub serve`:

```toml
[direct]
listen = "0.0.0.0:7391"
# Optional: override automatic interface addresses, for example behind a VPN.
advertise = ["192.168.1.20:7391"]
```

Without `advertise`, a wildcard listener advertises up to eight non-loopback, non-link-local interface addresses of its address family with the listening port. A listener bound to one specific address advertises only that address if eligible. For an intentional loopback setup, supply an explicit loopback `advertise` address. Only active paired machines receive these addresses inside their encrypted channel. There is no mDNS, broadcast or unauthenticated discovery.

Transient TCP accept errors are logged at warn level and retried after 100 ms,
so temporary resource exhaustion does not stop the listener.

New authenticated connections replace the worker's old connection, retaining the same reliable delivery queue. `list_workers` and `navarch hub status` report `local`, `relay` or `direct`. Pairing still uses the configured relay; a worker with previously saved direct endpoints can reconnect without a running relay.
