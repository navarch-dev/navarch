use anyhow::Result;
use serde_json::{Value, json};
pub(super) fn table(value: &serde_json::Value, json: bool, tasks: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    let mut rows = vec![
        if tasks {
            vec!["CARD", "STATE", "STEPS", "TITLE"]
        } else {
            vec!["ID", "STATE", "ONLINE", "NAME", "LINK", "AGENTS"]
        }
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    ];
    let items = if tasks { &value["tasks"] } else { value };
    for item in items.as_array().into_iter().flatten() {
        let text = |v: &serde_json::Value| {
            v.as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        rows.push(if tasks {
            vec![
                text(&item["id"]),
                text(&item["state"]),
                item["step_count"].to_string(),
                text(&item["title"]),
            ]
        } else {
            vec![
                text(&item["machine"]["id"]),
                text(&item["machine"]["state"]),
                if item["online"] == true { "yes" } else { "no" }.into(),
                text(&item["machine"]["display_name"]),
                text(&item["connection"]),
                item["machine"]["capabilities"]["agents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        });
    }
    let columns = if tasks { 4 } else { 6 };
    let widths: Vec<_> = (0..columns)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut output = rows
        .into_iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| {
                    format!(
                        "{cell}{}",
                        " ".repeat(if i + 1 == columns {
                            0
                        } else {
                            widths[i] - cell.chars().count() + 2
                        })
                    )
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(corrupt) = value["corrupt"].as_array() {
        for error in corrupt {
            output.push_str(&format!(
                "\nWarning: {}",
                error.as_str().unwrap_or("unreadable card")
            ));
        }
    }
    Ok(output)
}

pub(super) fn task(value: &Value, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    let mut lines = vec![format!(
        "Card: {}\nTitle: {}\nState: {}",
        text(&value["id"]),
        text(&value["title"]),
        text(&value["state"])
    )];
    for step in value["steps"].as_array().into_iter().flatten() {
        lines.push(format!(
            "Step {} role={} agent={} machine={} session={} state={}\n{}",
            text(&step["task_id"]),
            text(&step["role"]),
            text(&step["agent"]),
            text(&step["machine"]),
            text(&step["session"]),
            text(&step["state"]),
            text(&step["result_summary"])
        ));
    }
    if let Some(log) = value["log"].as_array() {
        for entry in log {
            lines.push(format!(
                "{} {} {}",
                entry["time"],
                text(&entry["kind"]),
                text(&entry["summary"])
            ));
        }
    }
    Ok(lines.join("\n"))
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("-")
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn permission(value: &Value) -> String {
    let id = text(&value["request_id"]);
    let mut lines = vec![format!(
        "Permission {id}: {}\n{}",
        text(&value["tool"]),
        text(&value["detail"])
    )];
    for option in value["options"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{}: {}\nnavarch hub permission {id} --choose {}",
            text(&option["id"]),
            text(&option["name"]),
            shell_quote(text(&option["id"]))
        ));
    }
    lines.push(format!("navarch hub permission {id} --cancel"));
    lines.join("\n")
}
pub(super) fn outcome(value: &Value, json: bool, card_scoped: bool) -> Result<String> {
    let mut value = value.clone();
    if value["permission"].is_object() {
        let id = text(&value["permission"]["request_id"]);
        let commands: Vec<_> = value["permission"]["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|option| {
                json!({"option_id":option["id"], "command":format!(
                "navarch hub permission {id} --choose {}", shell_quote(text(&option["id"])))})
            })
            .collect();
        let cancel = format!("navarch hub permission {id} --cancel");
        value["permission"]["answer_commands"] = json!(commands);
        value["permission"]["cancel_command"] = json!(cancel);
        value["resume_command"] = json!(format!(
            "navarch hub wait --card {} --after-seq {}",
            text(&value["card_id"]),
            value["seq"]
        ));
    }
    if json {
        return Ok(serde_json::to_string_pretty(&value)?);
    }
    if card_scoped {
        let mut lines = Vec::new();
        for field in ["card_id", "task_id", "outcome"] {
            if let Some(text) = value[field].as_str() {
                lines.push(format!("{field}: {text}"));
            }
        }
        if let Some(seq) = value["seq"].as_u64() {
            lines.push(format!("seq: {seq}"));
        }
        if value["permission"].is_object() {
            lines.push(String::new());
            lines.push(permission(&value["permission"]));
            lines.push(format!("Resume: {}", text(&value["resume_command"])));
        } else if let Some(result) = value["result"].as_str() {
            lines.push(String::new());
            lines.push(result.into());
        }
        return Ok(lines.join("\n"));
    }
    let mut lines = Vec::new();
    for field in ["card_id", "task_id", "outcome", "message"] {
        if let Some(text) = value[field].as_str() {
            lines.push(format!("{field}: {text}"));
        }
    }
    if value["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty())
    {
        lines.push(records(&value["events"], false, false)?);
    }
    if value["queued"] == true {
        lines.push("Request queued.".into());
    }
    if let Some(result) = value["result"].as_str() {
        lines.push(result.into());
    }
    if value["permission"].is_object() {
        lines.push(permission(&value["permission"]));
    }
    if let Some(seq) = value["seq"].as_u64() {
        lines.push(format!("seq: {seq}"));
        if value["permission"].is_object() {
            lines.push(format!(
                "Resume: navarch hub wait --card {} --after-seq {seq}",
                text(&value["card_id"])
            ));
        }
    }
    if lines.is_empty() {
        return Ok(serde_json::to_string_pretty(&value)?);
    }
    Ok(lines.join("\n"))
}
pub(super) fn records(value: &Value, json: bool, sessions: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    if sessions {
        let mut lines = vec!["MACHINE AGENT SESSION CWD TITLE".into()];
        for s in value["sessions"].as_array().into_iter().flatten() {
            lines.push(format!(
                "{} {} {} {} {}",
                text(&s["machine"]),
                text(&s["agent"]),
                s["session_id"]
                    .as_str()
                    .unwrap_or(text(&s["adapter_session_id"])),
                text(&s["cwd"]),
                text(&s["title"])
            ));
        }
        lines.push(format!(
            "stale={} truncated={}",
            value["stale"], value["truncated"]
        ));
        return Ok(lines.join("\n"));
    }
    let mut lines = Vec::new();
    for entry in value.as_array().into_iter().flatten() {
        lines.push(format!(
            "{} {} card={} {}",
            entry["seq"],
            text(&entry["kind"]),
            text(&entry["card_id"]),
            text(&entry["summary"])
        ));
        let body = &entry["envelope"]["body"];
        if body["type"] == "permission_ask" {
            lines.push(permission(body));
        }
    }
    if lines.is_empty() {
        lines.push("No unread events.".into());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rendering_preserves_results_and_quotes_permission_answers() {
        let value = json!({"card_id":"card", "task_id":"turn", "outcome":"permission_ask", "seq":8,
            "result":"earlier progress", "events":[{"seq":8,"summary":"Permission required: edit"}], "permission":{
            "request_id":"request", "tool":"edit", "detail":"write a file", "options":[{"id":"a'b", "name":"Allow once"}]}});
        let rendered = outcome(&value, false, true).unwrap_or_default();
        assert_eq!(
            rendered,
            "card_id: card\ntask_id: turn\noutcome: permission_ask\nseq: 8\n\nPermission request: edit\nwrite a file\na'b: Allow once\nnavarch hub permission request --choose 'a'\\''b'\nnavarch hub permission request --cancel\nResume: navarch hub wait --card card --after-seq 8"
        );
        let output: Value = serde_json::from_str(&outcome(&value, true, true).unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(output["result"], value["result"]);
        assert_eq!(
            output["permission"]["answer_commands"][0]["command"],
            "navarch hub permission request --choose 'a'\\''b'"
        );
        assert_eq!(
            output["resume_command"],
            "navarch hub wait --card card --after-seq 8"
        );
        assert_eq!(
            outcome(&value, true, false).unwrap_or_default(),
            outcome(&value, true, true).unwrap_or_default()
        );
    }
    #[test]
    fn card_outcome_prints_metadata_then_full_result_without_event_summary() {
        let value = json!({"card_id":"card", "task_id":"turn", "outcome":"finished", "seq":8,
            "result":"full\nresult", "events":[{"seq":8,"kind":"finished","card_id":"card","summary":"truncated hint"}]});
        assert_eq!(
            outcome(&value, false, true).unwrap_or_default(),
            "card_id: card\ntask_id: turn\noutcome: finished\nseq: 8\n\nfull\nresult"
        );
        assert!(
            outcome(&value, false, false)
                .unwrap_or_default()
                .contains("truncated hint")
        );
        let output: Value = serde_json::from_str(&outcome(&value, true, true).unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(output, value);
    }
    #[test]
    fn task_includes_identity_states_log_and_complete_result() {
        let card = json!({"id":"card", "title":"Title", "state":"finished", "steps":[{
            "task_id":"turn", "role":"review", "agent":"fake", "machine":"machine", "session":"session",
            "state":"finished", "result_summary":"one\ntwo"}], "log":[{"time":1,"kind":"finished","summary":"done"}]});
        let result = task(&card, false).unwrap_or_default();
        for expected in [
            "Title",
            "role=review",
            "agent=fake",
            "machine=machine",
            "session=session",
            "one\ntwo",
            "done",
        ] {
            assert!(result.contains(expected));
        }
    }
}
