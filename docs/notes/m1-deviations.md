# M1 deviations and boundary decisions

1. **Filesystem canonicalization lives in `navarch-worker`, not `navarch-policy`.**
   DESIGN section 2 forbids policy I/O, while section 6 requires canonical path
   comparisons, including symlink escapes. Pure policy code cannot resolve a
   filesystem symlink. The worker expands configured home paths and canonicalizes
   roots, dispatch cwds and reported permission targets before calling the pure
   policy functions. The policy API documents its canonical-path precondition and
   rejects nonabsolute paths and unresolved `..` components. Nonexistent permission
   targets resolve through an existing parent; unresolved paths cannot match a
   within-roots rule. This preserves both the dependency boundary and the required
   runtime behavior. Tests cover traversal and symlink escape at both boundaries.

2. **Interactive permissions are Unix-only.** The design targets Unix local
   sockets and Unix file permissions, but does not specify an OS restriction for
   terminal prompting. On Unix the CLI uses a safe nonblocking terminal descriptor,
   so the ten-minute deadline can cancel input without leaving a blocking reader.
   Other platforms cancel terminal escalation instead of starting an uncancellable
   stdin read. Non-TTY denial works on every platform. Linux and macOS are the
   workspace CI targets; Windows interactive prompting remains unsupported.

3. **Worker session IDs are independent of adapter session IDs.** The task and
   design do not define an adapter namespace in `SendToSession`. ACP session IDs
   are opaque and two configured adapters can return the same value. Reusing
   those values as worker registry keys would reject valid sessions or route to
   the wrong agent. The worker therefore allocates a UUID for each public worker
   session, stores `adapter_session_id` alongside it, and scopes permission
   callbacks by agent plus adapter session ID. Driver APIs still use the
   adapter's own ID. A regression test covers colliding IDs in two drivers.

No real adapter was run. Real-agent file creation and persistent history remain
reviewer acceptance checks, as explicitly requested. Hub, transport, relay,
encryption, MCP serving, and worker serve remain outside M1.
