# M2 deviations and boundary decisions

1. **`list_sessions` lists hub-observed sessions.** The section 4 wire protocol
   has no request/response for querying the worker's M1 session registry. Having
   the hub depend on `navarch-worker`, or read its registry directly, would violate
   the section 2 role boundary and would not work for M3 workers. The closest sound
   M2 behavior derives sessions from persisted Accepted/Finished events, scoped by
   machine. Standalone sessions that have never been observed by the hub are not
   listed. Full worker-registry enumeration requires an explicit future protocol
   extension; none was silently added here.

2. **Each follow-up receives a new task ID/card.** The design leaves the control
   request/response shapes and task/turn relationship unspecified. M1 uses TaskId
   to correlate queued turns, permission contexts and cancellation; assigning the
   same task ID to overlapping turns would make cancellation and completion
   ambiguous. `send_to_session` therefore takes machine, session and prompt and
   allocates a new TaskId for the turn. Cards retain the shared session ID; each
   has a step, state, summaries and event log. This is a boundary decision, not an
   extension to the M1 wire protocol.

3. **The reconnect wrapper returns replacement links to reliable delivery.**
   Transparent reconnection inside `Link::recv` would hide the reconnection from
   ack tracking and could leave an unacknowledged outbox unsent. `ReconnectingLink`
   wraps a connector with jittered backoff and gives each replacement Link to the
   reliable actor, which resends pending envelopes before normal traffic. It does
   not itself implement Link. LocalLink uses an independent writer task so
   simultaneous large sends do not prevent either peer from receiving.

4. **Crypto returns canonical machine ID text.** Section 2 places proto and
   crypto in the same bottom layer and forbids same-layer dependencies. The
   identity helper therefore derives the required encoding as a String without
   depending on navarch-proto. Transport and CLI convert it to the validated
   MachineId type. This preserves both identity derivation and the strict layer
   rule; no wire format changes are involved.

No relay, remote worker connection, pairing or encryption was implemented.
The real-adapter/Claude Code acceptance check was deliberately not run, as the
implementation request forbids running against real agents. Automated acceptance
uses only FakeDriver, the existing fake ACP subprocess, memory links, temporary
Unix sockets and temporary files.
