# ACP crate check (M0)

## Version and scope

The example compiles against the official `agent-client-protocol` **5.1.0**
crate, with its `process` feature and the stable protocol-v1 API. Its schema
dependency is `agent-client-protocol-schema` **1.16.0**. The workspace's minimum
Rust version is 1.88, matching the SDK's declared minimum.

This is a client API check, not the M1 driver. No real adapter was run, so adapter
compatibility, authentication and actual model output remain for reviewer testing.
All ACP types and dependencies stay inside `navarch-acp`; the SDK is a development
dependency because only the example uses it in M0.

## Manual use

Running this command starts the supplied adapter and can consume agent quota:

```sh
cargo run -p navarch-acp --example acp_smoke -- /path/to/adapter [adapter arguments...]
```

Arguments are passed separately to the adapter, without a shell. The example
creates a temporary working directory, prints streamed reply text and the stop
reason, and prints the first page of sessions. It automatically cancels every
permission request. If the adapter does not advertise session listing, it reports
that limitation rather than sending an unsupported request. The SDK manages the
child's stdio and shuts down the process when the connection finishes; the
temporary directory lives until that shutdown completes.

## APIs used

The client starts with `Client.v1()`, registers typed notification/request
handlers, then calls `connect_with(AcpAgent::new(AcpAgentConfig::new(command)
.args(args)), ...)`. A Tokio runtime drives the connection. Inside the connection
callback, each request uses `connection.send_request(request).block_task().await`.

| Operation | API |
|---|---|
| `initialize` | `InitializeRequest::new(ProtocolVersion::V1)`; response includes agent information and capabilities. |
| `session/new` | `NewSessionRequest::new(absolute_temp_directory)`; response supplies `session_id`. |
| `session/prompt` | `PromptRequest::new(session_id, vec![ContentBlock::Text(TextContent::new("Reply with OK only"))])`; response supplies `stop_reason`. |
| Reply text | `on_receive_notification` with `SessionNotification`; extract `SessionUpdate::AgentMessageChunk` containing `ContentBlock::Text`. |
| `session/list` | Check `agent_capabilities.session_capabilities.list`, then send `ListSessionsRequest::default()`. |
| `session/request_permission` | `on_receive_request` with `RequestPermissionRequest`; `responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled))`. |

## Coverage and awkward points for M1

- **Resume and load are available without unstable features.** Send
  `ResumeSessionRequest::new(session_id, cwd)` only when
  `agent_capabilities.session_capabilities.resume` is present. It restores a
  session without replaying history. The fallback is
  `LoadSessionRequest::new(session_id, cwd)` when `agent_capabilities.load_session`
  is true; load replays history as notifications. Both accept MCP servers.
  Capability checks, restart management and fallback selection remain driver work.
- **Listing is optional and paginated.** `ListSessionsRequest` supports `cwd`
  and `cursor`; `ListSessionsResponse` contains `sessions` and `next_cursor`.
  The smoke example requests one page without a filter. A driver must follow
  cursors to return the full list.
- **Mode and model configuration is typed, but adapter-defined.**
  `NewSessionResponse.config_options` describes the options. Select values can
  be flat or grouped (`SessionConfigSelectOptions`); options can also be boolean.
  Apply a discovered selection with
  `SetSessionConfigOptionRequest::new(session_id, config_id,
  SessionConfigOptionValue::value_id(value_id))`. The response returns the new
  configuration, and `SessionUpdate::ConfigOptionUpdate` can change it later.
  Optional `Mode` and `Model` categories help discovery but are not guaranteed;
  option IDs and accepted values must not be assumed. `SetSessionModeRequest`
  is also available, but the design calls for using config options. The smoke
  example leaves all configuration at adapter defaults.
- **Permissions use a reverse request.** The client must service requests while
  awaiting a prompt response. The typed callback receives a responder; returning
  `Cancelled` is distinct from selecting an offered allow/deny option. M1 can
  return `Selected(SelectedPermissionOutcome::new(option_id))` after consulting
  its permission handler. The smoke client advertises no filesystem or terminal
  capabilities and implements no file or command execution callbacks.
- **The SDK API has changed from older examples.** Version 5.1.0 uses
  `Client.v1()`, `v1::ConnectionTo<Agent>` and types under `v1::schema`, rather
  than the older `ClientSideConnection` interface. Typed handlers use the
  `on_receive_notification!` and `on_receive_request!` dispatch macros. Most
  schema types are non-exhaustive, so constructors/builders are preferable to
  struct literals, and matches need to tolerate future variants.

No missing client-side call was found for the planned driver through source
inspection and compilation. A temporary mock subprocess (not added to the
repository) verified initialize, new session, prompt, streamed reply text, stop
reason, listing and automatic permission cancellation over stdio. No real-agent
behavior was verified.

Sources: the downloaded 5.1.0 SDK and 1.16.0 schema source, the SDK's
`yolo_one_shot_client` example, and the official
[SDK v1 API documentation](https://docs.rs/agent-client-protocol/5.1.0/agent_client_protocol/v1/index.html)
and [session setup specification](https://agentclientprotocol.com/protocol/v1/session-setup).

## M0 implementation choices (PR notes)

The home helper lives in `navarch-cli::home`: environment lookup and startup
directory creation belong in CLI wiring, while the unimplemented store remains
empty. It resolves `NAVARCH_HOME` before the user's home directory and creates
the selected directory with mode 0700 on Unix, also restricting an existing
directory to that mode. The CLI stubs do not call it or create state.

`MachineId` validates the canonical lowercase, unpadded base32 representation of
16 bytes, including unused trailing bits. `TaskId` wraps a UUID. Both serialize
as JSON strings; deserialization preserves their validation. Identity generation
and protocol messages remain outside M0.
