# Worker

Standalone commands run one turn per CLI invocation. M2 also provides
`navarch worker serve` to connect this same runtime to the local hub. Navarch
does not install adapters. Configuration and session metadata live under `NAVARCH_HOME`, defaulting
to `~/.navarch`. Run `navarch worker init` to create commented examples; existing
files are never overwritten.

## Configure adapters

`worker.toml`:

```toml
display_name = "Laptop"

[[agents]]
name = "claude"
command = "npx"
args = ["-y", "@agentclientprotocol/claude-agent-acp"]
# mode = "default"

[[agents]]
name = "codex"
command = "npx"
args = ["-y", "@agentclientprotocol/codex-acp"]
# mode = "workspace-write"
```

Install Node.js/npm and authenticate the corresponding agent beforehand. The
adapter commands are `npx -y @agentclientprotocol/claude-agent-acp` and
`npx -y @agentclientprotocol/codex-acp`. Invoking npx can download a package, and
running a real adapter can consume agent quota. Commands and arguments are passed
directly to the child process, without a shell.

Navarch supplies an explicit mode for these built-in agent names:

| Agent | Built-in mode | Meaning |
|---|---|---|
| `claude` | `default` | Normal Claude permission prompting. Navarch handles the adapter's permission requests through local policy. |
| `codex` | `workspace-write` | Codex's workspace write sandbox mode. Access beyond its sandbox remains subject to the adapter's approval rules. Navarch evaluates permission requests it receives. |

A configured `mode` overrides the built-in value; `run --mode` overrides both.
Other agent names require an explicit mode if their adapter needs one. Mode and
model values must be offered in ACP session config options; unavailable values
fail clearly before a prompt is sent. Navarch discovers selectors and values
rather than assuming their option IDs. It never changes an agent's authentication
or global settings. The adapter remains responsible for enforcing its own sandbox
and reporting operations that require permission.

## Configure local policy

`worker-policy.toml`:

```toml
allowed_roots = ["~/projects"]
allowed_agents = ["claude", "codex"]
max_concurrent_turns = 3
default = "escalate"

[[rule]]
kind = "edit"
within_roots = true
decision = "allow"

[[rule]]
kind = "execute"
command_prefix = ["cargo test", "cargo build"]
decision = "allow"
```

Create allowed root directories before loading the policy. `~` uses the user's
home, independently of `NAVARCH_HOME`. Roots and working directories are resolved
to canonical paths, including symlinks, before dispatch checks. A cwd outside the
roots or an agent outside the agent allowlist is refused before an adapter starts;
the error names `allowed_roots` or `allowed_agents`.

Permission rules use `read`, `edit`, `execute`, `fetch`, `mcp` or `other` categories.
The first matching rule wins; otherwise `default` applies. A `within_roots` rule
requires at least one reported path and every path must resolve within a root.
Relative paths use the session cwd. A new file can resolve through an existing
parent; missing parents and unresolvable paths cannot satisfy the rule. Command
prefixes match the exact prefix followed by whitespace or the end of the command,
so `cargo test` does not match `cargo testing`. Only configure command prefixes you
trust, including any further arguments the agent could append. Unknown tool
categories remain `other`. Missing policy files use empty allowlists and default
escalation, permitting no dispatches.

In standalone mode, an escalated permission displays the original offered choices
when stdin is a TTY. Choose a number or press Enter to cancel. Non-TTY input is
denied using an offered reject option, or cancelled if no rejection is offered.
A permission expires after ten minutes. Interactive terminal input is supported
on Unix; other platforms cancel escalations. Allow decisions select an offered
allow-once choice, never silently upgrading to a persistent allowance.

## Commands

```sh
navarch worker doctor
navarch worker run --agent claude --cwd "$HOME/projects/example" \
  "Create hello.txt containing hi"
navarch worker send --session '<printed session ID>' \
  "What file did you just create?"
navarch worker sessions --json
navarch worker probe --json
```

`run` and `send` stream agent text to stdout. Worker session IDs and stop reasons go to
stderr. Exit status is 0 for `end_turn`, 1 for other stop reasons or errors.
Worker IDs are independent of adapter IDs: different adapters can reuse the same
opaque ID. The registry keeps both IDs and routes permissions by adapter and
adapter session together. `send` reloads the session configuration and resumes the adapter session after the
previous Navarch process has exited. The adapter must retain that session's own
history. Resume falls back to load when resume is unavailable or fails and load
is supported. A dead adapter is restarted on the next operation; sessions are
restored lazily. A failed turn is reported and is never automatically replayed.

`sessions` reads worker metadata without launching adapters. The registry is
`worker/sessions.json`, written through a temporary file and atomic rename.
Malformed registries are renamed to `sessions.corrupt-<uuid>.json` in the same
directory. After a worker process restarts, stale running/queued states become
idle; prompt text is not durably queued or automatically replayed.

`doctor` checks configured executables and prints installation guidance without
launching them. Finding `npx` does not verify a package is installed or an account
is authenticated. `probe` explicitly starts present adapters and creates
throwaway sessions to discover versions and model IDs; each adapter probe has a
30-second deadline. It also reports OS, architecture, running turns, and git
repositories at the root and up to two directory levels below allowed roots.
Directory symlinks are not traversed during repository scanning.

## Hub sessions and progress

Run `navarch worker serve` after `navarch hub serve`, with the same
`NAVARCH_HOME`. The worker reconnects automatically and answers `ListSessions`
from both its persisted registry and each configured driver’s `session/list`.
The driver follows pagination and stops at 500 sessions per agent; the merged
response also caps each agent at 500, preferring registry sessions. Unsupported
or failed adapter listings retain registry entries. Session metadata includes
agent, adapter ID, optional public worker ID, cwd, title, update time and whether
Navarch originally started the session. Standalone `worker sessions` continues
to read only the registry without starting adapters.

The hub can send a prompt to an external session using its agent name and adapter
session ID. The worker looks it up through that driver, canonicalizes its cwd and
checks both `allowed_roots` and `allowed_agents` before adoption. A refused
session is never resumed or added to the registry. An adopted session receives
a new public worker ID, is resumed lazily with the configured default mode and
retains `navarch_started: false` across restarts. Subsequent prompts use the same
queue, concurrency limits, cancellation and permission path as sessions created
by dispatch. Adapter IDs are always scoped by agent name.

Each prompt has its own wire `task_id`, even when it appends to an existing hub
card. Progress messages contain short tool titles and status or readable plan
text, never ACP debug output. Summaries are limited to 200 characters. Within
each turn, the first update can be sent immediately and subsequent updates are
coalesced to the latest summary at intervals of at least one second. A pending
summary may be discarded when the turn ends; Finished/Failed carries completion.
Fine-grained standalone text streaming is unaffected.

## Offline testing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Run only the subprocess-driver suite:
cargo test -p navarch-acp --features testing --test driver
```

The workspace suite enables the ACP `testing` feature through the worker's test
dependency. Its `FakeDriver` is in memory; `fake-acp-agent` is a separate offline
stdio process. The latter takes a JSON script path. Example script:

```json
{"steps":[{"reply":"Hello"},{"tool":"Edit"},{"delay_ms":10},{"reply":"Done"}]}
```

A `permission` step holds ACP request-permission parameters (the fixture adds the
session ID). A `crash: true` step exits once, writing a sibling `.crashed` marker
so restart tests can proceed. Fake sessions persist in a sibling `.sessions.json`
file; `log` optionally records received JSON-RPC messages. No test invokes a real
agent or needs network access.

## Direct connections

After pairing and approval, the hub announces its configured direct endpoints inside the authenticated channel. The worker saves them to `worker/endpoints.json` under `NAVARCH_HOME`. On reconnect it tries at most three saved addresses, allowing two seconds per address including authentication, then falls back to the relay. While on a relay connection, an authenticated announcement containing any address not in the previously stored list (including the first announcement) triggers a direct attempt immediately. The five-minute background retry continues after failed attempts; identical announcements or address removals do not trigger extra attempts. A successful direct handshake takes over without cancelling active turns. Reliable acknowledgements and deduplication preserve results through the switch.

Direct connections require a reachable hub listener (see [hub configuration](hub.md#direct-connections)); Navarch performs no network discovery. To keep a worker on the relay, set this top-level setting in `worker.toml` and restart it:

```toml
direct = false
```

The default is `direct = true`. An explicitly configured local Unix socket continues to take precedence over remote links.
