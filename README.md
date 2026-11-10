# Navarch

> **Navarch** /ˈnævɑːrk/ (NAV-ark) — ancient Greek for "fleet commander".

One orchestrator, many machines, many coding agents.

Navarch lets a single orchestrator session dispatch work to coding agents (Claude Code, Codex, and others) running on any of your machines, hand results from one agent to the next, and ask you only when a decision is yours to make.

[中文说明](README.zh-CN.md)

## Status

**0.1.0 development release. Interfaces and file formats can still change. The cryptography has not had an independent security audit.**

What exists today: a hub, a worker and a relay in one binary; pairing with a one-time code; encrypted relay and optional direct links; local policy on every worker; hub commands and completion waits for agents, people and scripts; release and service installation tooling. See [docs/](docs/) for setup: [worker](docs/worker.md), [hub](docs/hub.md), [relay](docs/relay.md), [security](docs/security.md), [design](docs/DESIGN.md).

See [installation](docs/install.md) for release archives, source builds, services, upgrades, and uninstalling.

## Quick start

Build from source, or use a published release as described in [installation](docs/install.md):

```sh
cargo build --locked --release -p navarch-cli
mkdir -p "$HOME/.local/bin"
cp target/release/navarch "$HOME/.local/bin/navarch"
export PATH="$HOME/.local/bin:$PATH"
navarch --version
navarch hub init
navarch worker init
# Configure an installed ACP adapter and allowed roots in ~/.navarch/worker*.toml.
navarch hub serve
# In another terminal:
navarch worker serve
# In another terminal:
navarch hub status
navarch hub dispatch --machine MACHINE --agent AGENT --cwd /absolute/repo --wait 'Describe the test setup'
```

Use [hub setup](docs/hub.md) for `--wait`, background `navarch hub wait`, and
`navarch hub watch`. Give agent sessions the pointer `navarch hub guide`. For background operation, preview
`navarch service install hub --dry-run` before installing hub and worker services.

## What it is for

You have several machines (a desktop, a laptop, a cloud box) and more than one coding agent. Today you find the right session yourself, copy context between agents by hand, and check back to see whether anything finished.

With Navarch you talk to one orchestrator. It works out which project and task you mean, picks the right agent and machine, dispatches the work, and reports back.

## How it works

```
        You (phone or desktop)
                 │
                 ▼
   ┌───────────── Hub ─────────────┐
   │  Orchestrator session         │   one per person
   │  navarch hub: task cards,     │   all state lives here
   │  roles, routing, machine list │
   └───────────────┬───────────────┘
                   │ outbound, end-to-end encrypted
                   ▼
              ┌─ Relay ─┐              forwards ciphertext only
              └────┬────┘              stores nothing
       ┌───────────┼───────────┐
       ▼           ▼           ▼
    Worker      Worker      Worker     one per machine
       │           │           │
      ACP         ACP         ACP      local, over stdio
       ▼           ▼           ▼
   Claude Code   Codex    other agents
```

| Component | Runs on | Job |
|---|---|---|
| `navarch hub` | The machine your orchestrator session runs on | Keeps all state. Exposes commands to the orchestrator: list workers, dispatch, send to a session, cancel. |
| `navarch worker` | Every machine that does work | Connects outward. Starts, resumes and cancels agent sessions through ACP. Reports what the machine can do. |
| `navarch relay` | Any host with a public address | Forwards encrypted messages by machine ID. Never sees content, keeps nothing on disk. |

All three are subcommands of one binary.

## Design principles

1. **One entry point.** You talk to the orchestrator, never to individual sessions.
2. **The orchestrator dispatches; it does not do the work.** Its context stays small.
3. **Route by strength.** Which agent handles which kind of task is a table you can edit.
4. **Hand off through files, not conversation.** Every task has a card; every step leaves a short result. Any session can be lost and the work continues.
5. **You decide the things that matter.** Merging, deploying, spending and deleting wait for you.
6. **Outbound by default.** Relay links need no inbound ports; direct links open a hub listener only when explicitly configured.

## Key choices

- **Agents are driven through the [Agent Client Protocol](https://agentclientprotocol.com).** A worker is an ACP client. Supporting a new agent means adding its ACP adapter, not writing a new integration.
- **Pairing uses a one-time code.** Run `navarch pair` on the hub, type the code on the new machine. No accounts, no IP addresses.
- **End-to-end encryption and signatures.** The relay is untrusted by design. A compromised relay cannot read tasks or impersonate your hub.
- **Workers enforce their own policy.** Each machine decides which directories, agents and actions it allows, whatever the hub asks for.
- **No arbitrary remote shell.** A worker accepts "run this agent on this task". It does not accept "run this command".
- **State stays on the hub.** No external database or storage service.

## Security

Navarch is, by nature, a way to make another machine run work. Treat it accordingly.

- Pair only machines you control.
- Run workers under an account with the least access that gets the job done.
- Do not run workers on production servers.
- A relay you do not operate can see which machines talk to each other and when, though not what they say.

The threat model is in [docs/security.md](docs/security.md). The cryptographic design has not been independently audited. Please report security issues privately rather than in a public issue.

## Roadmap

- [x] Single machine: hub plus a local worker driving agents over ACP
- [x] Relay, pairing and end-to-end encryption
- [x] Hub commands and an orchestrator guide
- [x] Completion waits that can run as background shell commands
- [x] Direct connections on the same local network
- [x] Packaging, install script, per-user services
- [ ] Independent security review

## Non-goals

- A chat app or a new agent. Navarch coordinates existing ones.
- A hosted control plane that holds your code or your task history.
- A general remote-execution tool.

## License

[Apache-2.0](LICENSE)

Navarch is an independent project. It is not affiliated with or endorsed by Anthropic, OpenAI or Zed Industries. Claude Code and Codex are the products of their respective owners.
