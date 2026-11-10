# M5 implementation notes

## Control API additions

- Dispatch and send responses add `seq`, captured under the existing publication
  lock before queuing the worker command. Both CLI waiting paths use that cursor.
- Task responses add `permission_requests`, a public projection containing request
  ID, task ID, tool, detail and offered options for unanswered, waiting turns.
  Internal permission routing and deduplication fields remain hidden. Tool/detail
  are retained with existing permission metadata so details survive inbox reads
  and daemon restarts. Legacy metadata defaults these new strings to empty.
  This additive extension is needed because compact event hints intentionally
  omit permission payloads, and waits must not mark the inbox read.
- Control connection failures are distinguished from rejected requests, so a wait
  retries disconnections with bounded backoff while surfacing validation failures.
- Existing streaming operations, worker session server configuration, and worker
  permission categories remain available. No framing or trust boundary changed.

## Verification and remaining manual checks

Automated checks cover the real daemon/control socket, a FakeDriver worker over
LocalLink, built binary dispatch/wait JSON and exit codes, 50 instant completions,
permission answers and resume, inbox read markers, direct session continuation,
hand-offs, cancellation, prompt sources and bounds, capped wait renewal, unrelated
cursor advancement, timeout/backoff, and daemon restart during a wait.

The reviewer-only checks with a real Claude adapter and a Claude Code session
have not been run. These include a real dispatch result, background-shell wakeup,
and an agent learning to dispatch from only the one-line guide pointer. No real
adapter was started and no quota was consumed by the automated tests.

The supplied M5 task file itself contains literal search-pattern examples that
its acceptance search forbids outside historical records. It is preserved as
supplied; the obsolete-integration search excludes that specification as well
as the historical files named in it. No implementation or current user guide
contains those obsolete references.

Card cancellation prefers the oldest running/waiting step, then the oldest queued
step. The existing worker sends Accepted for queued continuations too, so these
may already appear running on the hub; newest-first would cancel a continuation
instead of the turn ahead of it. A real binary regression test covers this order.

Initial M5 implementation validation passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 206 passed, zero failed, zero ignored, including the
  existing high-volume relay and handshake stress tests.
- CLI dependency-tree inspection, obsolete-integration scan with the specification
  exception described above, whitespace checks, and changed-file path/email scan.

## M5 review fixes

- Added optional `WaitForEvents.include_read`, defaulting to `false` for existing
  control clients. Every CLI wait path opts in, so its replay depends only on
  `after_seq` even after another client reads the inbox. `inbox --peek` keeps
  unread-only behavior. Both stores retain read events for bounded replay;
  file-store replay also survives reopen and legacy sequence migration.
- Confirmed the hub already records a worker's cancelled result as a cancelled
  step. Fixed card-state derivation to retain `cancelled` when the latest step
  was cancelled and no step remains running, queued or waiting. This fixes the
  stored card, task details and task list together, including older card reads.
- Readable card outcomes now show card ID, task ID, outcome and sequence, then a
  blank line followed by the complete result or permission block. Event summaries
  and earlier progress no longer duplicate that body. JSON output is unchanged.
- Kept the repository root free of `AGENTS.md`. The guide pointer remains an
  example in `docs/hub.md` for users' own project instructions; clarified M5's
  wording so it does not imply adding instructions to this repository.
- Updated DESIGN section 8a, the card-state rules, hub documentation and the
  orchestrator guide. Added protocol/store/renderer regressions and built-binary
  tests against a real daemon with a FakeDriver worker over LocalLink for replay
  after inbox reads, cancellation persistence and presentation, all three readable
  card wait commands, and the absence of root instructions. No real adapter was
  rerun during these fixes; the manual adapter checks above remain unverified.

The first full review-validation run exposed an older workflow test that expected
a cancelled card to be finished. Updated that assertion to require a cancelled
card and step, with no active-index entry, before rerunning the workspace checks.

Review validation passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 213 passed, 0 failed, 0 ignored, including
  the existing relay and 100,000-handshake stress tests.
- Whitespace checks and changed-file personal home-path/email scan passed.
- No repository-root `AGENTS.md` was created. Changes remain uncommitted.
