use rmcp::model::Tool;
use serde_json::{Value, json};
fn string(description: &str) -> Value {
    json!({"type":"string","description":description})
}
fn object(description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","description":description,"properties":properties,"required":required,"additionalProperties":false})
}
fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> Tool {
    let schema = object(description, properties, required);
    // object() always builds a JSON object; avoid an invariant unwrap in runtime.
    Tool::new(
        name,
        description,
        schema.as_object().cloned().unwrap_or_default(),
    )
}
pub(super) fn tools() -> Vec<Tool> {
    let machine = string(
        "Machine ID returned by list_workers. Choose explicitly; the hub does not schedule for you.",
    );
    let task = string("Task UUID returned by dispatch or send_to_session.");
    let session = string(
        "Public worker session ID returned by list_sessions or get_task, scoped to a machine.",
    );
    vec![
        tool(
            "list_workers",
            "List known workers with online/approval state and last capabilities. Use reported agent names when dispatching.",
            json!({}),
            &[],
        ),
        tool(
            "list_sessions",
            "Query worker registries and adapters for resumable sessions. Results are newest first and report truncated and stale. Offline or timed-out workers return hub-observed sessions marked stale.",
            json!({"machine":machine,"agent":string("Optional adapter name filter."),"cwd":string("Optional worker-local directory filter."),"limit":{"type":"integer","minimum":0,"default":50,"description":"Maximum sessions per worker, capped at 200. Results are newest first and report truncated and stale."}}),
            &[],
        ),
        tool(
            "dispatch",
            "Start a task on an explicit active, online machine. Supply exactly one of agent or role, a worker-local cwd and prompt. The worker enforces its local policy. Returns card_id and task_id; poll get_task by card_id or read_inbox for completion.",
            json!({
                "card_id":string("Optional existing card UUID for a hand-off."),"machine":machine,"agent":string("Configured adapter name reported by that machine. Omit when using role."),
                "role":string("Name of hub/roles/<name>.md. Supplies agent, default model/mode and prompt preferences. Omit when using agent."),
                "cwd":string("Non-empty directory path on the worker machine, inside its allowed_roots."),
                "prompt":string("Task instructions for the agent."),"title":string("Optional task-card title."),"project":string("Optional configured project name or alias."),
                "mode":string("Optional adapter permission mode override."),"model":string("Optional adapter model ID override."),
                "mcp_servers":{"type":"array","description":"Optional command-based servers exposed to this agent session.","items":object("Server executable and literal arguments; never interpreted by a shell.",json!({"name":string("Server name."),"command":string("Executable path."),"args":{"type":"array","items":{"type":"string"}},"env":{"type":"object","additionalProperties":{"type":"string"}}}),&["name","command"])}
            }),
            &["machine", "cwd", "prompt"],
        ),
        tool(
            "send_to_session",
            "Queue a follow-up in an existing session on an explicit machine. Appends to the session’s latest owning card unless card_id overrides it. Supply agent with an adapter session ID to adopt an external session. Returns card_id and task_id.",
            json!({"machine":machine,"session_id":session,"agent":string("Adapter name when session_id is an external adapter session ID."),"card_id":string("Optional card UUID override."),"prompt":string("Follow-up instructions.")}),
            &["machine", "session_id", "prompt"],
        ),
        tool(
            "cancel",
            "Cancel the task's running turn or remove its queued turn. Completion arrives in read_inbox and get_task.",
            json!({"task_id":task}),
            &["task_id"],
        ),
        tool(
            "get_task",
            "Read a card by its card UUID, with state and steps. Compact event log is omitted unless include_log is true.",
            json!({"task_id":string("Card UUID returned as card_id."),"include_log":{"type":"boolean","default":false}}),
            &["task_id"],
        ),
        tool(
            "list_tasks",
            "List active card summaries without logs and report corrupt files. Set include_finished to also see finished and failed cards.",
            json!({"include_finished":{"type":"boolean","description":"Include terminal task cards; defaults to false."}}),
            &[],
        ),
        tool(
            "read_inbox",
            "Read unread Finished, Failed, Rejected and PermissionAsk events and mark that batch read. PermissionAsk includes request_id and offered option IDs for answer_permission.",
            json!({}),
            &[],
        ),
        tool(
            "answer_permission",
            "Answer an outstanding PermissionAsk using an offered option ID, or cancel it. Worker policy and the ten-minute permission deadline remain authoritative.",
            json!({"request_id":string("UUID from a PermissionAsk event."),"decision":{"oneOf":[object("Select one of the offered options.",json!({"type":{"const":"selected"},"option_id":string("Exact offered option ID.")}),&["type","option_id"]),object("Cancel the permission request.",json!({"type":{"const":"cancelled"}}),&["type"])]}}),
            &["request_id", "decision"],
        ),
        tool(
            "approve_machine",
            "Approve an already known pending machine. Local workers are active automatically. Pairing and remote connections are introduced in M3.",
            json!({"machine":machine}),
            &["machine"],
        ),
    ]
}
