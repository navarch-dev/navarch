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
pub(crate) fn event(update: acp::SessionUpdate) -> AgentEvent {
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            acp::ContentBlock::Text(text) => AgentEvent::MessageChunk(text.text),
            _ => AgentEvent::Other("non-text message".into()),
        },
        acp::SessionUpdate::ToolCall(call) => AgentEvent::ToolCall(call.title),
        acp::SessionUpdate::ToolCallUpdate(call) => AgentEvent::ToolCall(format!("{call:?}")),
        acp::SessionUpdate::Plan(plan) => AgentEvent::Plan(format!("{plan:?}")),
        other => AgentEvent::Other(format!("{other:?}")),
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
