# Navarch

> **Navarch** /ˈnævɑːrk/ (NAV-ark) — ancient Greek for "fleet commander".

One orchestrator, many machines, many coding agents.

Navarch lets a single orchestrator session dispatch work to coding agents (Claude Code, Codex, and others) running on any of your machines, hand results from one agent to the next, and ask you only when a decision is yours to make.

[中文说明](README.zh-CN.md)

## Status

**Early design. Nothing here is runnable yet.**

The protocol choices below have been validated in small experiments, but there is no released code. Expect everything to change.

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
| `navarch hub` | The machine your orchestrator session runs on | Keeps all state. Exposes tools to the orchestrator: list workers, dispatch, send to a session, cancel. |
| `navarch worker` | Every machine that does work | Connects outward. Starts, resumes and cancels agent sessions through ACP. Reports what the machine can do. |
| `navarch relay` | Any host with a public address | Forwards encrypted messages by machine ID. Never sees content, keeps nothing on disk. |

All three are subcommands of one binary.

## Design principles

1. **One entry point.** You talk to the orchestrator, never to individual sessions.
2. **The orchestrator dispatches; it does not do the work.** Its context stays small.
3. **Route by strength.** Which agent handles which kind of task is a table you can edit.
4. **Hand off through files, not conversation.** Every task has a card; every step leaves a short result. Any session can be lost and the work continues.
5. **You decide the things that matter.** Merging, deploying, spending and deleting wait for you.
6. **Outbound only.** No machine needs a public address or an open port.

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

A fuller threat model will be published before the first release. Please report security issues privately rather than in a public issue.

## Roadmap

- [ ] Single machine: hub plus a local worker driving agents over ACP
- [ ] Relay, pairing and end-to-end encryption
- [ ] Hub tools for the orchestrator, completion notifications
- [ ] Direct connections on the same local network
- [ ] Packaging, install script and documentation

## Non-goals

- A chat app or a new agent. Navarch coordinates existing ones.
- A hosted control plane that holds your code or your task history.
- A general remote-execution tool.

## License

[Apache-2.0](LICENSE)

Navarch is an independent project. It is not affiliated with or endorsed by Anthropic, OpenAI or Zed Industries. Claude Code and Codex are the products of their respective owners.
