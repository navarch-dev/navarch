//! Compile-only ACP client check. Run manually with an adapter command:
//! `cargo run -p navarch-acp --example acp_smoke -- adapter [args...]`.
//! Running this example against a real agent consumes agent quota.

#![forbid(unsafe_code)]

use std::io::{self, Write};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::{
    ConnectionTo,
    schema::{
        ContentBlock, InitializeRequest, ListSessionsRequest, NewSessionRequest, PromptRequest,
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
        SessionNotification, SessionUpdate, TextContent,
    },
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Check an ACP adapter with one prompt (consumes agent quota)")]
struct Args {
    /// Adapter executable (use -- before the command to stop option parsing).
    command: std::path::PathBuf,
    /// Arguments passed verbatim to the adapter.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let cwd = tempfile::tempdir()?;
    let session_cwd = cwd.path().to_path_buf();
    let agent = AcpAgent::new(AcpAgentConfig::new(args.command).args(args.args));

    Client
        .v1()
        .on_receive_notification(
            async |notification: SessionNotification, _cx| {
                if let SessionUpdate::AgentMessageChunk(chunk) = notification.update
                    && let ContentBlock::Text(text) = chunk.content
                {
                    let mut stdout = io::stdout().lock();
                    write!(stdout, "{}", text.text)
                        .and_then(|()| stdout.flush())
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async |_request: RequestPermissionRequest, responder, _cx| {
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            eprintln!("Initialized: {:?}", initialized.agent_info);

            let session = connection
                .send_request(NewSessionRequest::new(session_cwd))
                .block_task()
                .await?;
            let outcome = connection
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new("Reply with OK only"))],
                ))
                .block_task()
                .await?;
            println!("\nStop reason: {:?}", outcome.stop_reason);

            if initialized
                .agent_capabilities
                .session_capabilities
                .list
                .is_some()
            {
                let sessions = connection
                    .send_request(ListSessionsRequest::default())
                    .block_task()
                    .await?;
                println!("Sessions: {sessions:#?}");
            } else {
                eprintln!("session/list is not supported by this adapter");
            }
            Ok(())
        })
        .await?;

    // Keep the working directory alive until the adapter has shut down.
    drop(cwd);
    Ok(())
}
