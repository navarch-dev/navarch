//! ACP connection lifetime and reverse-request handlers.
use super::*;
impl AcpDriver {
    pub(super) async fn start(&self) -> Result<Live, DriverError> {
        let agent = AcpAgent::new(
            AcpAgentConfig::new(&self.command.command).args(self.command.args.clone()),
        );
        let streams = self.streams.clone();
        let permissions = self.permissions.clone();
        let (tx, rx) = oneshot::channel();
        let command = self.command.command.display().to_string();
        let task = tokio::spawn(async move {
            Client
                .v1()
                .on_receive_notification(
                    async move |notification: acp::SessionNotification, _cx| {
                        let mut streams = streams.lock().await;
                        if let Some((sink, result, reset, tools)) =
                            streams.get_mut(&notification.session_id.to_string())
                        {
                            let event = event(notification.update, tools);
                            if let AgentEvent::ToolCall(_) = &event {
                                *reset = true;
                            }
                            if let AgentEvent::MessageChunk(text) = &event {
                                if *reset {
                                    result.clear();
                                    *reset = false;
                                }
                                append_bounded(result, text);
                            }
                            let _ = sink.send(event);
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |req: acp::RequestPermissionRequest, responder, _cx| {
                        let request = permission(req);
                        let offered: Vec<_> =
                            request.options.iter().map(|o| o.id.clone()).collect();
                        let outcome = match permissions.request(request).await {
                            PermissionDecision::Selected { option_id }
                                if offered.contains(&option_id) =>
                            {
                                acp::RequestPermissionOutcome::Selected(
                                    acp::SelectedPermissionOutcome::new(option_id),
                                )
                            }
                            _ => acp::RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(acp::RequestPermissionResponse::new(outcome))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
                    let init = connection
                        .send_request(acp::InitializeRequest::new(
                            agent_client_protocol::schema::ProtocolVersion::V1,
                        ))
                        .block_task()
                        .await;
                    match init {
                        Ok(init) => {
                            let _ = tx.send(Ok((connection.clone(), init)));
                            connection.incoming_closed().await;
                        }
                        Err(e) => {
                            let _ = tx.send(Err(error(e)));
                        }
                    }
                    Ok(())
                })
                .await
                .map_err(|e| DriverError::Adapter(format!("{command}: {e}")))
        });
        match rx.await {
            Ok(Ok((connection, initialized))) => Ok(Live {
                connection,
                initialized,
                task,
                restored: HashSet::new(),
            }),
            Ok(Err(e)) => {
                task.abort();
                Err(e)
            }
            Err(_) => match task.await {
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => Err(error("adapter exited before initialization")),
                Err(e) => Err(error(e)),
            },
        }
    }
}
