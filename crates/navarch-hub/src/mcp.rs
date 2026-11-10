mod tools;
use crate::{
    HubError,
    control::{self, Request},
};
use rmcp::{RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use std::path::{Path, PathBuf};
#[derive(Clone)]
struct Mcp {
    socket: PathBuf,
}
impl Mcp {
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
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new("navarch",env!("CARGO_PKG_VERSION")))
            .with_instructions("Choose workers explicitly. Dispatch returns card_id and task_id; get_task reads a card UUID, cancel targets a turn UUID; read_inbox retrieves completion and permission requests. The hub daemon owns all state.")
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
/// Serve a state-free MCP adapter on stdio using the official Rust SDK.
/// All SDK types remain private to navarch-hub. A missing daemon is reported
/// before starting the MCP handshake, and again as a tool error if it goes away.
pub async fn serve_mcp(socket: &Path) -> Result<(), HubError> {
    control::call(socket, &Request::ListWorkers).await?;
    let server = Mcp {
        socket: socket.into(),
    }
    .serve(rmcp::transport::stdio())
    .await
    .map_err(|e| HubError::Invalid(format!("MCP startup: {e}")))?;
    server
        .waiting()
        .await
        .map_err(|e| HubError::Invalid(format!("MCP server: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests;
