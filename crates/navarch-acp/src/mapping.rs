use crate::{AgentEvent, DriverError, McpServer, PermissionRequest};
use agent_client_protocol::v1::schema as acp;
use navarch_proto::{PermissionOption, PermissionOptionKind, ToolKind};

pub(crate) fn error(e: impl std::fmt::Display) -> DriverError {
    DriverError::Adapter(e.to_string())
}
pub(crate) fn servers(servers: &[McpServer]) -> Vec<acp::McpServer> {
    servers
        .iter()
        .map(|s| {
            acp::McpServer::Stdio(
                acp::McpServerStdio::new(&s.name, &s.command)
                    .args(s.args.clone())
                    .env(
                        s.env
                            .iter()
                            .map(|(k, v)| acp::EnvVariable::new(k, v))
                            .collect(),
                    ),
            )
        })
        .collect()
}
pub(crate) fn permission(req: acp::RequestPermissionRequest) -> PermissionRequest {
    let call = req.tool_call;
    let kind = match call.fields.kind {
        Some(acp::ToolKind::Read) => ToolKind::Read,
        Some(acp::ToolKind::Edit | acp::ToolKind::Delete | acp::ToolKind::Move) => ToolKind::Edit,
        Some(acp::ToolKind::Execute) => ToolKind::Execute,
        Some(acp::ToolKind::Fetch) => ToolKind::Fetch,
        _ => ToolKind::Other,
    };
    let detail = serde_json::to_string(&call).unwrap_or_default();
    let command = call
        .fields
        .raw_input
        .as_ref()
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    PermissionRequest {
        session_id: req.session_id.to_string(),
        tool: call
            .fields
            .title
            .unwrap_or_else(|| "Tool permission".into()),
        kind,
        detail,
        paths: call
            .fields
            .locations
            .unwrap_or_default()
            .into_iter()
            .map(|l| l.path)
            .collect(),
        command,
        options: req
            .options
            .into_iter()
            .filter_map(|o| {
                let kind = match o.kind {
                    acp::PermissionOptionKind::AllowOnce => PermissionOptionKind::AllowOnce,
                    acp::PermissionOptionKind::AllowAlways => PermissionOptionKind::AllowAlways,
                    acp::PermissionOptionKind::RejectOnce => PermissionOptionKind::RejectOnce,
                    acp::PermissionOptionKind::RejectAlways => PermissionOptionKind::RejectAlways,
                    _ => return None,
                };
                Some(PermissionOption {
                    id: o.option_id.to_string(),
                    name: o.name,
                    kind,
                })
            })
            .collect(),
    }
}
pub(crate) fn event(
    update: acp::SessionUpdate,
    tools: &mut std::collections::HashMap<String, (String, acp::ToolCallStatus)>,
) -> AgentEvent {
    use navarch_proto::short_summary;
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            acp::ContentBlock::Text(text) => AgentEvent::MessageChunk(text.text),
            _ => AgentEvent::Other("non-text message".into()),
        },
        acp::SessionUpdate::ToolCall(call) => {
            let summary = tool_summary(&call.title, &call.status);
            tools.insert(
                call.tool_call_id.to_string(),
                (short_summary(&call.title), call.status),
            );
            AgentEvent::ToolCall(summary)
        }
        acp::SessionUpdate::ToolCallUpdate(call) => {
            let (title, status) = tools
                .entry(call.tool_call_id.to_string())
                .or_insert_with(|| ("Tool".into(), acp::ToolCallStatus::Pending));
            if let Some(value) = call.fields.title {
                *title = short_summary(&value);
            }
            if let Some(value) = call.fields.status {
                *status = value;
            }
            AgentEvent::ToolCall(tool_summary(title, status))
        }
        acp::SessionUpdate::Plan(plan) => {
            let completed = plan
                .entries
                .iter()
                .filter(|e| matches!(e.status, acp::PlanEntryStatus::Completed))
                .count();
            let current = plan
                .entries
                .iter()
                .find(|e| matches!(e.status, acp::PlanEntryStatus::InProgress))
                .map(|e| e.content.as_str())
                .unwrap_or("Plan updated");
            AgentEvent::Plan(short_summary(&format!(
                "{current} ({completed}/{} complete)",
                plan.entries.len()
            )))
        }
        _ => AgentEvent::Other("Session updated".into()),
    }
}
pub(crate) fn values(option: &acp::SessionConfigOption) -> Vec<String> {
    match &option.kind {
        acp::SessionConfigKind::Select(select) => match &select.options {
            acp::SessionConfigSelectOptions::Ungrouped(values) => {
                values.iter().map(|v| v.value.to_string()).collect()
            }
            acp::SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|g| g.options.iter().map(|v| v.value.to_string()))
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    }
}
pub(crate) fn is_category(option: &acp::SessionConfigOption, category: &str) -> bool {
    let cat = option
        .category
        .as_ref()
        .and_then(|c| serde_json::to_value(c).ok());
    cat.as_ref().and_then(|c| c.as_str()) == Some(category)
        || (option.category.is_none() && option.name.eq_ignore_ascii_case(category))
}

fn tool_summary(title: &str, status: &acp::ToolCallStatus) -> String {
    let status = match status {
        acp::ToolCallStatus::Pending => "pending",
        acp::ToolCallStatus::InProgress => "running",
        acp::ToolCallStatus::Completed => "completed",
        acp::ToolCallStatus::Failed => "failed",
        _ => "updated",
    };
    // Reserve room for the status so long tool titles still convey their state.
    let title: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200 - status.len() - 2)
        .collect();
    format!("{title}: {status}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn summaries_keep_titles_and_status_without_debug_or_payloads() {
        let mut tools = std::collections::HashMap::new();
        let call = serde_json::from_value(json!({"sessionUpdate":"tool_call","toolCallId":"1","title":"Read files","status":"in_progress","rawInput":{"secret":"never include"}})).unwrap();
        assert_eq!(
            event(call, &mut tools),
            AgentEvent::ToolCall("Read files: running".into())
        );
        let update = serde_json::from_value(json!({"sessionUpdate":"tool_call_update","toolCallId":"1","status":"completed","rawOutput":{"huge":"never include"}})).unwrap();
        assert_eq!(
            event(update, &mut tools),
            AgentEvent::ToolCall("Read files: completed".into())
        );
        let update = serde_json::from_value(json!({"sessionUpdate":"tool_call_update","toolCallId":"2","title":"界".repeat(300),"status":"failed"})).unwrap();
        let AgentEvent::ToolCall(summary) = event(update, &mut tools) else {
            panic!("missing summary");
        };
        assert_eq!(summary.chars().count(), 200);
        assert!(summary.ends_with(": failed"));
        let plan = serde_json::from_value(json!({"sessionUpdate":"plan","entries":[{"content":"Run checks","priority":"medium","status":"in_progress"},{"content":"Implement","priority":"high","status":"completed"}]})).unwrap();
        assert_eq!(
            event(plan, &mut tools),
            AgentEvent::Plan("Run checks (1/2 complete)".into())
        );
        let other = serde_json::from_value(
            json!({"sessionUpdate":"current_mode_update","currentModeId":"default"}),
        )
        .unwrap();
        assert_eq!(
            event(other, &mut tools),
            AgentEvent::Other("Session updated".into())
        );
    }
}
