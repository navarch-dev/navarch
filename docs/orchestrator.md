# Drive the hub

Run work on other machines with `navarch hub` commands through your shell tool.
Run `navarch hub status` to see known machines, their agents and online status.
Start the daemon with `navarch hub serve`, or install its background service with
`navarch service install hub`, if it is not running.
Always pass `--json` when parsing output. Each command returns one JSON document;
`watch --json` is a continuous stream of JSON lines. Errors go to stderr.

Dispatch short work and wait for its result:

```sh
navarch hub dispatch --machine MACHINE --agent AGENT --cwd /absolute/repo --wait --json 'Describe the test setup'
```

Supply exactly one prompt source: a positional argument, `--prompt-file PATH`,
or stdin with `-` (also used when no argument is given and stdin is not a terminal).
Keep prompts non-empty and at most 256 KiB. Keep titles within 200 characters.
Use `--role NAME` instead of `--agent` to apply a configured role.
Use `--mcp-servers FILE` only to configure the worker agent's servers (64 KiB JSON array).
Save `card_id` (the work card) and `task_id` (the independently cancellable turn).

For long work, dispatch without `--wait`, save the returned `seq`, then run this
as a **background shell command** so your session is told when the command ends:

```sh
navarch hub wait --card CARD --after-seq SEQ --timeout 0 --json
```

Use the cursor from dispatch to include events that arrived immediately.
Use `--timeout 0` for no time limit; otherwise the default wait is 300 seconds.
A timeout leaves the task running. Wait again with the last returned `seq`.
Waits reconnect after a daemon restart and continue from that cursor.
Waits depend only on sequence, so another client reading the inbox cannot hide an outcome.
Read full results with `navarch hub task CARD --json`; add `--log` for history.
Use `navarch hub tasks --all --json` to find previous work.

When a wait reports `permission_ask` (exit 4), inspect `permission.request_id`,
`tool`, `detail` and the offered `options`. Ask the user when needed. Select an
option ID offered by that request, or cancel it:

```sh
navarch hub permission REQUEST --choose OPTION --json
navarch hub permission REQUEST --cancel --json
navarch hub wait --card CARD --after-seq SEQ --timeout 0 --json
```

Readable output prints exact answer commands with shell-quoted option IDs. JSON
includes `permission.answer_commands`, `permission.cancel_command` and `resume_command`.
Permissions expire after ten minutes. A wait never answers or marks inbox entries read.
Use `navarch hub inbox --json` to read and mark a batch read; `--peek` shows only
unread entries and marks nothing. Both leave sequence-based waits unaffected.
Use `navarch hub watch` to follow unread and live events interactively.

Continue the latest session on a card:

```sh
navarch hub send CARD --wait --json 'Now explain the main entry point'
```

Address a session directly with `send --machine MACHINE --session SESSION`;
add `--agent AGENT` when SESSION is an external adapter session ID.
Discover sessions with `navarch hub sessions --machine MACHINE --json`.
Hand off to another agent or machine with `dispatch --card CARD` and the new
`--machine`, `--agent` (or `--role`), `--cwd` and prompt.
Cancel the card's active step with `navarch hub cancel CARD --json`, or cancel a
specific turn with `navarch hub cancel --task TASK --json`.

Interpret exit codes identically for `wait`, `dispatch --wait` and `send --wait`:

| Code | Meaning |
|---|---|
| 0 | Success; a waited step finished |
| 1 | Usage error, daemon unreachable, or hub request rejected |
| 3 | Step failed, worker policy rejected it, or it was cancelled |
| 4 | Permission answer needed |
| 124 | Wait timed out; the task keeps running |

Respect the worker's local policy: it decides what is allowed. Do not try to work
around a rejection. Report the refusal and its reason to the user.
