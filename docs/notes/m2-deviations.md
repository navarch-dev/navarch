# M2 deviations and boundary decisions

1. **The reconnect wrapper returns replacement links to reliable delivery.**
   Transparent reconnection inside `Link::recv` would hide the reconnection from
   ack tracking and could leave an unacknowledged outbox unsent. `ReconnectingLink`
   wraps a connector with jittered backoff and gives each replacement Link to the
   reliable actor, which resends pending envelopes before normal traffic. It does
   not itself implement Link. LocalLink uses an independent writer task so
   simultaneous large sends do not prevent either peer from receiving.

2. **Crypto returns canonical machine ID text.** Section 2 places proto and
   crypto in the same bottom layer and forbids same-layer dependencies. The
   identity helper therefore derives the required encoding as a String without
   depending on navarch-proto. Transport and CLI convert it to the validated
   MachineId type. This preserves both identity derivation and the strict layer
   rule; no wire format changes are involved.

The earlier session-visibility and one-card-per-turn deviations no longer apply.
The revised design defines `ListSessions`/`SessionList`, card IDs and independent
turn IDs. M2 now queries worker registries and adapter sessions, supports
policy-checked adoption, and appends steps to cards across follow-ups and
hand-offs. Hub discovery waits at most 10 seconds per worker and falls back to
hub-observed sessions marked stale. Adapter pagination and merged worker
listings cap results at 500 per agent.

Old one-turn cards without per-step turn IDs are preserved but ignored on direct
reads and reported in unreadable-card diagnostics. No attempt is made to guess
turn identity from old envelope logs. Current cards hold compact logs; durable
deduplication and permission metadata are stored separately. Public keys write
lowercase hex everywhere; legacy array-form keys remain readable.

No relay, remote worker connection, pairing or encryption was implemented.
The real-adapter/Claude Code acceptance check was deliberately not run, as the
implementation request forbids running against real agents. Automated acceptance
uses only FakeDriver, the existing fake ACP subprocess, memory links, temporary
Unix sockets and temporary files.
