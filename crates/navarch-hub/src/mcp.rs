mod tools;
use crate::{
    HubError,
    control::{self, Request},
};
use rmcp::service::NotificationContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Clone)]
struct Mcp {
    socket: PathBuf,
    channel: bool,
    task: Arc<ChannelTask>,
}
#[derive(Default)]
struct ChannelTask(Mutex<Option<tokio::task::JoinHandle<()>>>);
impl Drop for ChannelTask {
    fn drop(&mut self) {
        if let Ok(task) = self.0.get_mut()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}
impl Mcp {
    fn new(socket: &Path, channel: bool) -> Self {
        Self {
            socket: socket.into(),
            channel,
            task: Arc::default(),
        }
    }
    async fn invoke(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> CallToolResult {
        if !tools::tools().iter().any(|t| t.name == name) {
            return CallToolResult::error(vec![Content::text(format!("unknown tool {name}"))]);
        }
        let mut args = arguments;
        args.insert("type".into(), name.into());
        let result = match serde_json::from_value::<Request>(args.into()) {
            Ok(request) => control::call(&self.socket, &request).await,
            Err(error) => Err(HubError::Invalid(format!(
                "invalid {name} arguments: {error}"
            ))),
        };
        match result {
            Ok(value) => CallToolResult::success(vec![Content::text(value.to_string())]),
            Err(error) => CallToolResult::error(vec![Content::text(error.to_string())]),
        }
    }
}
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        capabilities.experimental = Some(std::collections::BTreeMap::from_iter([(
            "claude/channel".into(),
            serde_json::Map::new(),
        )]));
        ServerInfo::new(capabilities).with_server_info(Implementation::new("navarch",env!("CARGO_PKG_VERSION")))
            .with_instructions("Choose workers explicitly. Dispatch returns card_id and task_id; get_task reads a card UUID, cancel targets a turn UUID; read_inbox retrieves completion and permission requests. The hub daemon owns all state.")
    }
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        if !self.channel {
            return;
        }
        if let Ok(mut task) = self.task.0.lock()
            && task.is_none()
        {
            let socket = self.socket.clone();
            *task = Some(tokio::spawn(async move {
                channel_events(socket, context.peer).await
            }));
        }
    }
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools::tools(),
            ..Default::default()
        })
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::tools().into_iter().find(|t| t.name == name)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .invoke(&request.name, request.arguments.unwrap_or_default())
            .await)
    }
}
/// Serve a state-free MCP adapter on stdio, without channel notifications.
pub async fn serve_mcp(socket: &Path) -> Result<(), HubError> {
    serve_mcp_with_channel(socket, false).await
}
/// Serve MCP over stdio. Channel subscriptions reconnect independently of tools,
/// so daemon downtime never stops the MCP session.
pub async fn serve_mcp_with_channel(socket: &Path, channel: bool) -> Result<(), HubError> {
    let server = Mcp::new(socket, channel)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| HubError::Invalid(format!("MCP startup: {e}")))?;
    server
        .waiting()
        .await
        .map_err(|e| HubError::Invalid(format!("MCP server: {e}")))?;
    Ok(())
}
fn channel_params(event: &crate::HubEvent) -> Option<serde_json::Value> {
    let card = event.card_id.map(|id| id.to_string()).unwrap_or_default();
    let content = match event.kind.as_str() {
        "finished" => format!(
            "Work finished for card {card} on machine {}: {}. Call get_task for the result.",
            event.machine, event.summary
        ),
        "failed" => format!(
            "Work failed for card {card} on machine {}: {}. Call get_task to inspect the failure.",
            event.machine, event.summary
        ),
        "rejected" => format!(
            "Work was rejected for card {card} on machine {}: {}. Call get_task to inspect the reason.",
            event.machine, event.summary
        ),
        "permission_ask" => format!(
            "Card {card} on machine {} needs permission: {}. Call read_inbox for the offered options, then answer_permission.",
            event.machine, event.summary
        ),
        "machine_pending" => format!(
            "Machine {} is paired and awaiting approval. Call list_workers to review it, then approve_machine if appropriate.",
            event.machine
        ),
        _ => return None,
    };
    Some(
        serde_json::json!({"content":content,"meta":{"kind":event.kind,"card_id":card,"step":event.step.map(|id|id.to_string()).unwrap_or_default(),"machine":event.machine.to_string(),"seq":event.seq.to_string()}}),
    )
}
async fn channel_events(socket: PathBuf, peer: rmcp::Peer<RoleServer>) {
    let mut cursor = None;
    let mut delay = 1;
    while !peer.is_transport_closed() {
        let subscription = if let Some(seq) = cursor {
            control::subscribe(&socket, Some(seq)).await
        } else {
            control::subscribe_live(&socket)
                .await
                .map(|(seq, subscription)| {
                    cursor = Some(seq);
                    subscription
                })
        };
        if let Ok(mut subscription) = subscription {
            delay = 1;
            loop {
                // The owning ChannelTask aborts this loop when the MCP service
                // drops. Do not cancel read_line: it may hold a partial JSON line.
                let event = match subscription.recv().await {
                    Ok(Some(control::StreamEvent::Event(event))) => event,
                    _ => break,
                };
                if let Some(params) = channel_params(&event) {
                    // rmcp waits only for the transport write, never an acknowledgement.
                    if peer
                        .send_notification(ServerNotification::CustomNotification(
                            CustomNotification::new("notifications/claude/channel", Some(params)),
                        ))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    cursor = Some(event.seq);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(60);
    }
}

#[cfg(test)]
mod tests;
