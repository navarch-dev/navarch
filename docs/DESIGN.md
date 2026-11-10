# Navarch design

This document is the source of truth for implementation. If code and this document disagree, fix one of them in the same change.

## 1. Roles

| Role | Process | Count | Responsibility |
|---|---|---|---|
| Orchestrator | An agent session (for example Claude Code) | 1 | Understands the user, decides what to dispatch. Not part of this codebase; it reaches Navarch through MCP tools. |
| Hub | `navarch hub serve` | 1 | Owns all state. Routes messages to workers. Serves the orchestrator's tools. |
| Worker | `navarch worker serve` | 1 per machine | Drives agents on its machine through ACP. Enforces local policy. |
| Relay | `navarch relay serve` | 1, on a public host | Forwards opaque frames between machines by machine ID. |

The hub's machine also runs a worker. Local and remote workers use the same messages; only the link differs.

## 2. Workspace layout

A Cargo workspace. One binary, `navarch`, from `navarch-cli`.

```
crates/
  navarch-proto      wire types, no I/O
  navarch-crypto     identity, pairing, channel encryption; no network I/O
  navarch-policy     permission policy, pure functions
  navarch-acp        the only crate that knows ACP
  navarch-store      hub state on disk
  navarch-transport  Link trait and its implementations
  navarch-worker     worker runtime
  navarch-hub        hub runtime and MCP server
  navarch-relay      relay server
  navarch-cli        argument parsing and wiring only
```

### Dependency rules

Layers, top to bottom: `cli` → `hub`, `worker`, `relay` → `store`, `acp`, `policy`, `transport` → `proto`, `crypto`.

1. A crate may depend only on crates in lower layers. Crates in the same layer do not depend on each other, with one exception: `transport` may depend on `crypto`.
2. `hub`, `worker` and `relay` never depend on each other. They share only `proto` types.
3. ACP types appear only in `navarch-acp`. MCP types appear only in `navarch-hub`. Neither leaks through public APIs of those crates.
4. `proto`, `crypto` (except key file helpers) and `policy` perform no I/O.
5. Every crate has `#![forbid(unsafe_code)]`.

### Conventions

- Rust edition 2024. `rust-version` set in the workspace manifest.
- Async runtime: tokio. Serialization: serde. CLI: clap (derive). Logging: tracing. Library errors: `thiserror` enums; `anyhow` only in `navarch-cli`.
- No `unwrap`/`expect` outside tests, except on invariants documented in a comment.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass.
- Public items have doc comments. Code, comments and commit messages are in English.
- Dependencies are declared once in `[workspace.dependencies]`.

## 3. Identity and addressing

- Each machine has an Ed25519 identity key pair, created on first run and stored in `~/.navarch/identity.key` with mode 0600.
- `MachineId` is the lowercase base32 (no padding) encoding of the first 16 bytes of BLAKE2s-256 of the public key. It is what messages are addressed to.
- The hub records, per machine: ID, public key, display name, state (`pending`, `active`, `revoked`), last capability report.

`NAVARCH_HOME` overrides `~/.navarch`. All paths below are relative to it.

## 4. Wire protocol (`navarch-proto`)

One envelope type for hub-worker traffic, serialized as JSON.

```rust
pub struct Envelope {
    pub v: u16,          // protocol version, starts at 1
    pub id: Uuid,        // message ID, used for acks and de-duplication
    pub from: MachineId,
    pub to: MachineId,
    pub body: Body,
}
```

`Body` is an internally tagged enum (`"type": "dispatch"`, snake_case).

Hub to worker:

| Body | Fields | Meaning |
|---|---|---|
| `Dispatch` | `task_id`, `agent`, `cwd`, `prompt`, `mode?`, `model?`, `mcp_servers?` | Start a new agent session. |
| `SendToSession` | `task_id`, `session_id`, `prompt` | Add a prompt to an existing session. |
| `Cancel` | `task_id` | Cancel the running turn. |
| `PermissionAnswer` | `request_id`, `decision` | Answer an escalated permission request. |
| `Probe` | none | Ask for a capability report. |
| `ListSessions` | `request_id`, `cwd?` | Ask for the sessions this worker can resume. |
| `Ack` | `of` | Acknowledge a worker message. |

Worker to hub:

| Body | Fields | Meaning |
|---|---|---|
| `Accepted` | `task_id`, `session_id` | Session exists; work started or queued. |
| `Rejected` | `task_id`, `reason` | Local policy or capacity refused the task. |
| `Progress` | `task_id`, `summary` | Coarse progress. Not every agent update. |
| `PermissionAsk` | `request_id`, `task_id`, `tool`, `detail`, `options` | Policy could not decide; a human must. |
| `Finished` | `task_id`, `session_id`, `stop_reason`, `result` | Turn ended. `result` is the agent's final message, truncated to 64 KiB. |
| `Failed` | `task_id`, `error` | Turn could not complete. |
| `ProbeReport` | `capabilities` | OS, arch, agents with versions and available models, repositories, load. |
| `SessionList` | `request_id`, `sessions` | Registry sessions plus sessions the adapters report (`session/list`), each with agent, adapter session ID, cwd, title, updated time and whether Navarch started it. |
| `Ack` | `of` | Acknowledge a hub message. |

Rules:

- A receiver that sees an unknown `v` replies `Failed`/drops with a clear log line; it never guesses.
- Unknown fields are ignored. Unknown `type` values are an error.
- Every non-`Ack` message is acknowledged. The sender retries unacknowledged messages after a reconnect. Receivers de-duplicate by `id`.

## 5. Driving agents (`navarch-acp`)

`navarch-acp` wraps the official `agent-client-protocol` crate and exposes a small trait so the rest of the system does not see ACP.

```rust
#[async_trait]
pub trait AgentDriver: Send + Sync {
    async fn new_session(&self, req: NewSession) -> Result<SessionHandle, DriverError>;
    async fn resume_session(&self, req: ResumeSession) -> Result<SessionHandle, DriverError>;
    async fn prompt(&self, session: &SessionId, text: &str, events: EventSink) -> Result<TurnOutcome, DriverError>;
    async fn cancel(&self, session: &SessionId) -> Result<(), DriverError>;
    async fn list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionInfo>, DriverError>;
}
```

- `AcpDriver` spawns the adapter command configured for an agent (stdio), runs `initialize`, and maps the calls above to `session/new`, `session/resume` (falling back to `session/load`), `session/prompt`, `session/cancel`, `session/list`.
- Agent updates are reduced to `AgentEvent` values (`MessageChunk`, `ToolCall`, `Plan`, `Other`) and sent to the `EventSink`.
- `session/request_permission` is forwarded to a `PermissionHandler` supplied by the caller. The handler returns one of the offered options or "cancelled".
- `NewSession` carries `cwd`, optional `mode`, optional `model`, and optional MCP servers. Mode and model are applied through the session's config options.
- One adapter process serves many sessions. The driver restarts a dead adapter on the next call and resumes sessions lazily.
- `FakeDriver` (behind the `testing` feature) is a scripted in-memory implementation for tests.
- A separate test binary, `fake-acp-agent`, speaks real ACP over stdio from a script, so `AcpDriver` itself is tested without a real agent.

Navarch does not install adapters. The worker config names the command for each agent; `navarch worker doctor` reports what is missing and how to install it.

## 6. Local policy (`navarch-policy`)

Pure decision logic. Input: a normalized permission request and the policy. Output: `Decision`.

```rust
pub enum Decision { Allow { remember: bool }, Deny, Escalate }
```

Policy file `worker-policy.toml`:

```toml
allowed_roots = ["~/projects"]     # sessions may only start under these
allowed_agents = ["claude", "codex"]
max_concurrent_turns = 3
default = "escalate"               # allow | deny | escalate

[[rule]]
kind = "edit"                      # read | edit | execute | fetch | mcp | other
within_roots = true
decision = "allow"

[[rule]]
kind = "execute"
command_prefix = ["cargo test", "cargo build"]
decision = "allow"
```

- Rules are evaluated in order; the first match wins; otherwise `default`.
- Paths are canonicalized before comparison. A path outside every allowed root never matches a `within_roots = true` rule.
- `Dispatch` with a `cwd` outside `allowed_roots`, or an agent not in `allowed_agents`, is rejected before any session starts.
- With no policy file, the worker uses a built-in policy: no allowed roots, default `escalate`. It does nothing until configured.

## 7. Worker (`navarch-worker`)

- Config `worker.toml`: display name, agents (`name`, `command`, `args`, default `mode`), hub link settings.
- Session registry `worker/sessions.json`: task ID, agent, session ID, cwd, state (`idle`, `running`, `queued`), timestamps. Written atomically (write to a temp file, then rename).
- One turn at a time per session. Prompts for a busy session are queued in order.
- At most `max_concurrent_turns` turns across sessions; beyond that, `Dispatch` is `Rejected`.
- Permission requests go to policy first. `Escalate` becomes `PermissionAsk`; the turn waits up to 10 minutes for `PermissionAnswer`, then the request is cancelled.
- To make agents ask, the worker sets an explicit session mode per agent (Claude Code: `default`; Codex: `workspace-write`) unless the agent config overrides it.
- The worker never executes commands on behalf of the hub. Its only actions are the ACP calls above.
- `ProbeReport` is built from: OS and arch, configured agents (adapter present, version, models from a throwaway `session/new`), git repositories under `allowed_roots` (depth 2), running turn count.

Standalone commands (no hub required): `navarch worker run`, `send`, `sessions`, `probe`, `doctor`. In standalone mode an escalated permission is asked on the terminal if stdin is a TTY, otherwise denied.

## 8. Hub (`navarch-hub`, `navarch-store`)

### State on disk (`navarch-store`), under `hub/`

| Path | Format | Content |
|---|---|---|
| `machines.json` | JSON | Known machines and their state. |
| `projects.toml` | TOML | Project names, aliases, repository names. User-edited. |
| `routes.toml` | TOML | Task kind to role. User-edited. |
| `roles/<name>.md` | Markdown with TOML front matter | Agent, model, mode, preferences. User-edited. |
| `tasks/<card_id>.toml` | TOML | Task card: title, project, state, steps, log. See "Cards and turns". |
| `tasks/index.toml` | TOML | One entry per active task. Derived from cards; rebuilt on demand. |
| `inbox.jsonl` | JSON lines | Worker events not yet read by the orchestrator. |

All writes are atomic. `navarch-store` exposes a `Store` trait with a file implementation and an in-memory one for tests.

### Cards and turns

- A **card** is one piece of work as the user thinks of it. It has a `CardId` and an ordered list of **steps**.
- A **step** is one turn of one agent session: role, agent, machine, session ID, state, result summary, and its own turn ID. The turn ID is the `task_id` carried on the wire, so turns can be queued and cancelled independently.
- `dispatch` creates a card with one step, or appends a step to an existing card when given `card_id` (a hand-off to another agent or machine).
- `send_to_session` appends a step to the card that owns the session's latest step, unless `card_id` says otherwise.
- A card's state is derived from its steps: `running` if any step is running or queued, `waiting` if a step waits for a permission answer, `failed` if the latest step failed, otherwise `finished`.
- The card log holds compact entries (`time`, `kind`, `step`, `summary` of at most 200 characters), never whole envelopes. `get_task` returns the card without the log unless asked.
- `Progress` summaries are short human-readable text. They never contain debug formatting of protocol types.

### Runtime

- `navarch hub serve` is a daemon. It holds links to workers, applies their messages to the store, and serves a control API on a Unix socket `hub.sock` (mode 0600), using line-delimited JSON requests and responses.
- `navarch hub mcp` is a stdio MCP server (official `rmcp` crate) that the orchestrator launches. It translates tool calls to control API requests. It holds no state.
- Tools: `list_workers`, `list_sessions`, `dispatch`, `send_to_session`, `cancel`, `get_task`, `list_tasks`, `read_inbox`, `answer_permission`, `approve_machine`.
- `list_sessions` asks the worker (`ListSessions`), so it includes sessions started outside Navarch that the adapters can resume. `send_to_session` accepts such a session by agent and adapter session ID; the worker adopts it into its registry on first use, subject to `allowed_roots`.
- Public keys are serialized as lowercase hex strings everywhere.
- `dispatch` picks nothing by itself: the orchestrator names the machine, role or agent, cwd and prompt. Choosing is the orchestrator's job; validating is the hub's.
- Worker events (`Finished`, `Failed`, `PermissionAsk`, `Rejected`) update the task card and append to the inbox. `read_inbox` returns and marks them read.

## 9. Links (`navarch-transport`)

```rust
#[async_trait]
pub trait Link: Send {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError>;
    async fn recv(&mut self) -> Result<Bytes, LinkError>;
    fn peer(&self) -> &MachineId;
}
```

Frames are length-prefixed (u32 big-endian), at most 1 MiB.

| Link | Used for | Security |
|---|---|---|
| `LocalLink` | Hub and worker on one machine, over the Unix socket `hub-worker.sock` | File permissions (0600). No encryption. |
| `RelayLink` | Everything else, over a WebSocket to the relay | Noise channel end to end (section 10). |

Both sides reconnect with exponential backoff (1 s to 60 s, jittered). After a reconnect, unacknowledged envelopes are resent.

## 10. Pairing and encryption (`navarch-crypto`)

### Pairing

1. `navarch pair` on the hub creates a code `RRRR-SSSSSS`: a 4-digit room and a 6-digit secret, valid for 10 minutes. The hub joins room `RRRR` on the relay.
2. `navarch join RRRR-SSSSSS --relay <url>` on the new machine joins the same room.
3. Both run SPAKE2 with the secret as the password, then confirm the derived key with a MAC over the transcript. One failed attempt closes the room.
4. Over that key (ChaCha20-Poly1305), they exchange identity public keys, X25519 static public keys and display names.
5. The hub stores the machine as `pending`. It becomes `active` only after `approve_machine`.

The relay sees the room number and ciphertext. It cannot derive the key.

### Channel

- After pairing, hub and worker talk over a Noise session, pattern `Noise_KK_25519_ChaChaPoly_BLAKE2s` (`snow` crate): both sides already know each other's static key, so the handshake authenticates both and gives forward secrecy.
- Each side has an X25519 static key for Noise in addition to the Ed25519 identity key. Both public keys are exchanged during pairing.
- Handshake and transport messages are carried as relay frames. A new handshake runs after every reconnect.
- The hub ignores any worker whose machine is not `active`.

## 11. Relay (`navarch-relay`)

- A WebSocket server (`/v1/ws`). Plain HTTP; terminate TLS in front of it.
- On connect the relay sends a random challenge; the client replies with its Ed25519 public key and a signature. The relay derives the `MachineId` from the key, so IDs cannot be spoofed.
- Routing frame: `{ "to": MachineId, "payload": base64 }`. The relay forwards to the connection registered for `to`, adding `from`. If `to` is offline the sender gets `{ "error": "offline", "to": ... }`. Nothing is buffered or written to disk.
- Pairing rooms: `{ "room": "RRRR", ... }` frames connect exactly two clients. Rooms expire after 10 minutes or one failed confirmation.
- Limits: 1 MiB per frame, 60 frames per second and 16 MiB per minute per connection, one connection per machine ID (a new one replaces the old).
- Logs contain machine IDs and sizes, never payloads.

## 12. What the relay and a stolen pairing code can and cannot do

| Attacker | Can | Cannot |
|---|---|---|
| Malicious relay | See who talks to whom and when; drop or delay frames | Read or forge messages; join as a machine |
| Someone who learns a pairing code later | Nothing; the room is closed | — |
| Someone who guesses during the 10 minutes | One online guess per room (1 in 10^6) | Stay undetected: a failed guess closes the room, and the machine still needs approval |
| Compromised hub | Send tasks to workers | Exceed each worker's local policy; run arbitrary commands |
| Compromised worker | Lie about its results | Reach other workers; read other machines' sessions |

## 13. Testing

- Unit tests for `proto` (round trips, version handling), `policy` (rule matching, path escapes), `crypto` (pairing success and failure, wrong code, tampered frames).
- `navarch-acp` against `fake-acp-agent`: new, prompt, resume after adapter restart, cancel, permission allow and deny, adapter crash mid-turn.
- Worker and hub against `FakeDriver` and in-memory links.
- End to end in one process: hub, relay and worker over loopback with `FakeDriver`; pairing, approval, dispatch, send, cancel, reconnect and resend.
- Tests that use a real agent are marked `#[ignore]` and documented; they consume quota and never run in CI.

## 14. Milestones

| Milestone | Scope |
|---|---|
| M0 | Workspace, conventions, CI, `navarch --help`, ACP crate check against a real adapter |
| M1 | `proto`, `policy`, `acp`, `worker` standalone commands |
| M2 | `store`, `transport` (`LocalLink`), `hub` daemon, control API, MCP server, worker `serve` |
| M3 | `crypto`, `relay`, `RelayLink`, pairing, approval, reconnect and resend |

Task files are in `docs/tasks/`.
