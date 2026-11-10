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
resends them after reconnection. Acknowledgements follow application of events to
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
state. It translates tools to control requests. If the daemon is absent, startup
reports the socket path and asks you to run `navarch hub serve`; later connection
failures are visible tool errors. Stdout carries only MCP traffic.

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
| `list_sessions` | Query worker registries and adapters; optionally filter by machine and cwd. A worker timeout (10 seconds) or offline worker returns hub-observed sessions marked `stale: true`. | `{"machine":"MACHINE"}` |
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
cwd; filesystem and agent allowlists remain worker decisions.

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
Adapter pagination is followed up to 500 sessions per agent; registry entries
are preferred when merging. An external session is addressed with `agent` and
its `adapter_session_id` passed as `session_id`. The worker canonicalizes its
reported cwd and checks `allowed_roots` and `allowed_agents` before adoption or
resume. Adoption allocates a public worker session ID and retains
`navarch_started: false` in the registry.

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
worker frames have a four-byte big-endian length prefix. Control types live in
`navarch_hub::control` and contain no MCP SDK types. Requests and events update
state under one hub runtime lock; file writes use same-directory temporary files
and atomic rename. FileStore instances opened on the same canonical directory
share a writer lock within the process.

See [M2 boundary decisions](notes/m2-deviations.md) for transport and layering
decisions.
