# M4 implementation clarifications and verification

## Events and channel compatibility

- rmcp 1.8.0 advertises protocol `2025-11-25` by default, but its private
  `negotiate_protocol_version` echoes known client-requested revisions, including
  `2026-07-28`, even when `get_info` supplies an older fallback. Its public
  `ServerHandler` API cannot pin those known requests to an older revision.
  Launch **Claude Code itself** with `MCP_PROTOCOL_NEGOTIATION=legacy`, as shown in
  `docs/hub.md`. Raw duplex tests verify both the older negotiation and the newer
  echoed revision. The experimental `claude/channel` capability is exactly `{}`;
  notifications wait only for the transport write, never a client acknowledgement.
- `hub/event-seq` is atomically reserved before inbox append or event publication.
  A crash can leave a sequence gap, but restarting does not reuse a sequence.
  Bus-only online/offline events also reserve sequences. Read inbox rows retain
  internal read markers for durable deduplication; legacy envelope-only rows
  migrate in order with fresh sequences.
- The broadcast buffer holds 256 hints. An overrun sends one `lagged` line and
  closes the stream; a socket write stalled for ten seconds is disconnected.
  Subscription replay walks all unread batches. Inbox tool responses are bounded
  prefixes; `after_seq` is applied before selection, and `read_inbox` marks only
  its returned prefix read.
- A valid worker envelope near 1 MiB can exceed the control budget once inbox
  metadata is added. In that rare case, its returned read view keeps the compact
  event fields and sets `envelope: null`, `envelope_omitted: true`. The original
  envelope remains unchanged in `hub/inbox.jsonl`; opaque identifiers are never
  shortened. Both stores have a regression covering the projected view, later
  rows, cursor reads, deduplication, read markers, and original disk preservation.
- Every control response is encoded against the 1 MiB line limit. An oversized
  result, including a legacy card or log, produces a bounded actionable error
  instead of sending an unreadable line. The connection remains usable for a
  subsequent request; no record is silently truncated.

## Direct links

- TCP begins with `NAVARCH-DIRECT/1\n` and a u32 big-endian length-prefixed JSON
  `{ machine, nonce }`. This hello supplies the worker nonce of the same nonce
  exchange used on relay links. Both paths use one Noise handshake, prologue and
  record implementation in `navarch-transport/src/channel.rs`.
- Authorization ordering matters: immediately dropping an unknown ID before the
  clear hub nonce would expose paired IDs. The listener gives every syntactically
  valid fresh hello the same nonce response and waits for the first KK frame.
  It resolves current authorization at that boundary, rejecting unauthorized IDs
  before creating or processing a Noise handshake. Revocation while waiting for
  the first KK frame therefore cannot use a previously captured active key.
  Invalid active-machine handshakes likewise close
  without a distinguishing reply. Replay history treats all IDs uniformly. The
  ten-second deadline includes magic, hello, nonce exchange, and Noise processing.
- The two-second direct-attempt budget includes TCP connection and authentication.
  Limiting only the TCP connect would allow an accepting, silent listener to
  prevent the required relay fallback.
- Wildcard listeners advertise eligible interface addresses of their bound address
  family. A listener bound to a specific eligible address advertises only that
  reachable address. Explicit `advertise` can include loopback for local testing.
  Safe interface enumeration uses `if-addrs`; no discovery traffic is sent.
- Per-source admission uses a rolling minute with at most ten timestamps per IP.
  The source table is capped at 4096 active IPs and refuses new sources while
  full. One shared semaphore limits unauthenticated connections to 64.
- Relay and direct admissions share one replacement slot and one reliable runtime
  and outbox per machine. Slot publication and watch-generation observation use
  the same mutex so a replacement cannot invalidate itself. Replacements interrupt
  the old receive loop and use normal reliable reconnect/resend and deduplication.
- `worker/endpoints.json` is atomically replaced only after a valid, addressed
  `HubEndpoints` arrives through the authenticated channel. Lists contain at most
  eight IP socket addresses of at most 64 bytes, with at most three tried per
  reconnect. `direct = false` permits storing announcements but disables direct
  attempts and live upgrades.

## Packaging and platform decisions

- The release profile uses thin LTO, one codegen unit and stripping, retaining the
  default `panic = "unwind"`. No explicit `catch_unwind` or `resume_unwind` was
  found. Retaining unwinding conservatively preserves Tokio task panic isolation
  and existing Drop cleanup; a task panic does not terminate every daemon role.
- Relay service installation requires explicit `--listen`. macOS upgrades unload
  an existing agent before bootstrap. Dry runs create no state, service or log
  directories and invoke no service-manager commands.
- Service generators validate absolute UTF-8 paths and reject control characters.
  CLI relative `NAVARCH_HOME` is resolved against the caller's current directory.
  Linux uses systemd's `:` command prefix to preserve literal dollar signs in
  executable paths and arguments. systemd rejects quotes, backslashes and glob
  characters in executable paths after unquoting; generation reports that platform
  restriction instead of producing an invalid unit. Environment values continue
  to support those characters. Both platforms' output is tested by parsing it.
- The release workflow pins third-party actions to full upstream-verified commit
  SHAs. Native runners build all four targets, and only the release job has
  `contents: write` permission.

## Automated verification and remaining manual checks

Commands use `CARGO_HOME=/tmp/navarch-m4-cargo-home` in this sandbox because the
normal dependency cache is read-only. No repository configuration depends on that
temporary cache.

- Final integrated checks passed: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`. The full run passed 192 tests with zero failures or
  ignored tests, plus all documentation-test suites. It includes the one-process
  relay-to-direct mid-turn acceptance test and the existing 100000-handshake
  stress test. `sh -n install.sh` and `git diff --check` also passed.
- Fresh event/store review passed `cargo test -p navarch-hub -p navarch-store`:
  13 hub unit tests, five hub integration tests, 11 store integration tests and
  doctests. Coverage includes exact-once publication, bounded summaries, replay,
  durable sequences across daemon restart, paused-time waits and timeout caps,
  nonblocking lag notification, real duplex MCP channel shape/filtering, disabled
  silence, no acknowledgement, absent daemon startup, socket-drop resubscription,
  migration, paging, near-limit inbox projection and bounded errors for oversized
  card responses on a reusable control connection. Scoped clippy passes with
  warnings denied.
- Direct-link tests cover successful authentication and maximum full-duplex
  frames; actual unknown/pending/revoked hub authorization; nonce/KK replay;
  wrong magic, oversize frames, deadlines and admission limits; refused and silent
  address fallback; repeated replacement; endpoint bounds; relay-to-direct upgrade
  during a permission-gated `FakeDriver` turn with one result and increasing event
  sequences; and direct-only reconnection after hub restart with the relay stopped.
  Sequential and concurrent 1000-publication replacement regressions verify the
  shared slot/generation synchronization; a genuine first-KK regression covers
  revocation during the nonce prelude.
  A daemon-level test checks that advertisements are populated before its first
  relay connection.
- Packaging tests cover version output, parsed launchd/systemd files, dry runs
  without state creation, mock release downloads for all four target mappings,
  latest and explicit versions, checksums, executable mode, PATH quoting and
  preserving an installed binary on checksum mismatch. `sh -n install.sh` passes.
- `shellcheck` is unavailable on this host. Publishing an actual GitHub release, real service loading, a real-adapter Claude
  Code channel session, and a physical-machine relay-to-direct smoke test remain
  reviewer/deployment checks. No M4 functionality is intentionally deferred.

## M4 review follow-up: listener, upgrades and event delivery

- Direct TCP accepts now warn on errors and sleep 100 ms before retrying instead
  of terminating the listener. A paused-time injected-EMFILE regression checks
  repeated failures, the backoff, and successful acceptance afterwards.
- Persisting authenticated endpoints now wakes the relay upgrade task whenever
  an address is absent from its previous list, including the first announcement.
  Identical lists and removals alone do not wake it. The five-minute retry remains
  after failed attempts, and `direct = false` continues to disable upgrades.
  Regressions send real encrypted announcements with initially empty and existing
  endpoint lists, verify persistence and direct authentication within three
  seconds, and check immediate wakeups plus continued periodic retries with paused
  time. The relay-to-direct mid-turn test now uses an endpoint wakeup rather than
  advancing the retry timer, retaining exactly-once completion and restart checks.
- New channel processes request `subscribe_live`. Under the same hub state lock
  used for publication, the daemon registers a receiver and snapshots the durable
  latest sequence, then sends `{ "kind": "ready", "seq": ... }` before newer
  events. This avoids replaying old unread pairing/completion events and has no
  cursor-query/subscription race. Read rows and bus-only sequences count towards
  the baseline; legacy inbox sequences are migrated before taking it. The MCP
  process retains that baseline even if the socket drops before its first
  notification, and later reconnects with the last delivered cursor. Existing
  `subscribe` replay behaviour and backlog access through inbox/wait are retained.
  Tests cover an already approved machine's unread event, publication while ready
  is blocked, real control live subscriptions across daemon restart, and real
  duplex MCP reconnects both before and after notification delivery.
- Channel text now includes the existing bounded summary for `finished`, `failed`,
  `rejected` and `permission_ask`, keeping the next-tool hint. The duplex wire
  regression verifies the summary and hint for each kind.
- `wait_for_events` now projects inbox rows to `HubEvent`: only `seq`, `kind`,
  `card_id`, `step`, `machine`, `summary`, `time`. Control-wire and MCP-tool
  regressions assert that exact shape and verify `read_inbox` still returns the
  original envelope. The tool description, hub/worker guides, design and M4 task
  specification now describe these behaviours. Container packaging remains dropped.
- Final review-fix validation passed: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` (197 tests, zero failures or ignored tests, plus all
  doctest suites). Cargo used `CARGO_HOME=/tmp/navarch-m4-cargo-home` as above.
  The targeted hub/store/transport suite and direct-link integration tests also
  passed, including the 100000-handshake stress test. `git diff --check` passed.
  No commit was created.
