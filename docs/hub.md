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
```

On first local connection the worker is recorded as active, with its display name
and public key. Hub and local worker may share the same machine identity.
`identity.key` contains 32 Ed25519 secret bytes and has mode 0600. The machine ID
is lowercase unpadded base32 of the first 16 bytes of BLAKE2s-256 of the public
key. An existing corrupt key produces an error and is preserved.

The hub creates `hub.sock` (control) and `hub-worker.sock` (worker frames), both
mode 0600. Ctrl-C stops serving and removes owned socket paths. After an abrupt
exit, startup replaces stale sockets, preserving regular files, symlinks and live
listeners. Stop an existing daemon before starting another one.

A worker reconnects automatically with jittered exponential delays of 1–60
seconds. Its in-memory outbox keeps stable envelope IDs until acknowledged and
resends them after reconnection. Acknowledgements follow application of events to
hub state. A live worker can finish a turn while the hub is down and deliver the
result after it returns. Restarting the worker itself does not preserve its
in-memory delivery outbox. Durable task logs prevent replayed events from
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
with an ID returned by `list_workers`, `TASK_UUID` with a returned task UUID,
`SESSION` with a worker session ID, and `REQUEST_UUID` with a permission request
UUID. These examples are reference calls, not commands to run as a shell script.

| Tool | Behavior | Example arguments |
|---|---|---|
| `list_workers` | Known machines, approval/online state and last capabilities. | `{}` |
| `list_sessions` | Sessions observed by the hub, optionally filtered by machine. | `{"machine":"MACHINE"}` |
| `dispatch` | Create a turn on an explicit machine; returns `task_id`. Supply exactly one of `agent` or `role`. | `{"machine":"MACHINE","agent":"codex","cwd":"/absolute/path/to/repo","prompt":"Describe the test setup","title":"Inspect tests"}` |
| `send_to_session` | Queue a follow-up, preserving session history; returns a new `task_id` for this turn. | `{"machine":"MACHINE","session_id":"SESSION","prompt":"Now explain the main entry point"}` |
| `cancel` | Cancel the task's active turn or remove it from the session queue. | `{"task_id":"TASK_UUID"}` |
| `get_task` | Authoritative card with state, session, summaries and event log. | `{"task_id":"TASK_UUID"}` |
| `list_tasks` | Active cards by default; report corrupt cards without hiding good ones. | `{"include_finished":true}` |
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

A task card represents one turn. Follow-up cards retain the same worker session
ID and receive separate task IDs, so queued turns can be cancelled independently.
The derived `hub/tasks/index.toml` contains only active cards; all cards remain
available through `get_task` and `list_tasks` with `include_finished`. The file
store rebuilds the index on opening and on every card write, reporting corrupt
cards in the rebuild result. User configuration files are not rewritten on open.

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

See [M2 boundary decisions](notes/m2-deviations.md) for session visibility and
turn identity details.
