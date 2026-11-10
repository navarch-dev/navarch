use crate::{Hub, HubError, control::Request};
use navarch_proto::{Body, MachineId};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

impl Hub {
    /// Execute control operations, querying workers asynchronously for session lists.
    pub async fn request_async(&self, request: Request) -> Result<Value, HubError> {
        match request {
            Request::WaitForEvents {
                timeout_secs,
                after_seq,
                include_read,
            } => Ok(serde_json::to_value(
                self.wait_for_events(timeout_secs, after_seq, include_read)
                    .await?,
            )?),
            Request::ListSessions {
                machine,
                agent,
                cwd,
                limit,
            } => self.list_sessions(machine, agent, cwd, limit).await,
            Request::SendToSession {
                machine,
                session_id,
                prompt,
                card_id,
                agent: None,
            } if !self
                .store
                .tasks()?
                .0
                .iter()
                .flat_map(|c| &c.steps)
                .any(|s| s.machine == machine && s.session.as_ref() == Some(&session_id)) =>
            {
                // A worker-registry session may predate this hub. Resolve its
                // adapter identity over the protocol before creating its card.
                let list = self
                    .list_sessions(Some(machine.clone()), None, None, Some(200))
                    .await?;
                let session = list["sessions"]
                    .as_array()
                    .and_then(|sessions| {
                        sessions
                            .iter()
                            .find(|s| s["session_id"] == session_id && s["stale"] == false)
                    })
                    .ok_or_else(|| {
                        HubError::Invalid(format!("unknown session {session_id} on {machine}"))
                    })?;
                let agent = session["agent"]
                    .as_str()
                    .ok_or_else(|| HubError::Invalid("session has no agent".into()))?
                    .to_owned();
                let adapter = session["adapter_session_id"]
                    .as_str()
                    .ok_or_else(|| HubError::Invalid("session has no adapter ID".into()))?
                    .to_owned();
                self.request(Request::SendToSession {
                    machine,
                    session_id: adapter,
                    prompt,
                    card_id,
                    agent: Some(agent),
                })
            }
            request => self.request(request),
        }
    }

    async fn list_sessions(
        &self,
        machine: Option<MachineId>,
        agent: Option<String>,
        cwd: Option<PathBuf>,
        limit: Option<usize>,
    ) -> Result<Value, HubError> {
        let limit = navarch_proto::session_limit(limit);
        let machines = self
            .store
            .machines()?
            .into_iter()
            .filter(|m| machine.as_ref().is_none_or(|id| id == &m.id))
            .collect::<Vec<_>>();
        let (cards, corrupt) = self.store.tasks()?;
        let mut queries = tokio::task::JoinSet::new();
        for machine in machines {
            let hub = self.clone();
            let cwd = cwd.clone();
            let agent = agent.clone();
            let mut observations: Vec<_> = cards
                .iter()
                .flat_map(|c| &c.steps)
                .filter(|s| {
                    s.machine == machine.id
                        && agent.as_ref().is_none_or(|agent| agent == &s.agent)
                        && cwd.as_ref().is_none_or(|cwd| s.cwd.as_ref() == Some(cwd))
                })
                .collect();
            observations.sort_by_key(|s| s.created_at);
            let mut fallback = observations.into_iter().filter_map(|s| s.session.as_ref().map(|id| (id.clone(), json!({"machine":s.machine,"session_id":id,"agent":s.agent,"adapter_session_id":s.adapter_session_id,"cwd":s.cwd,"updated_at":s.created_at / 1_000_000,"stale":true}))))
                .collect::<std::collections::BTreeMap<_,_>>().into_values().collect::<Vec<_>>();
            fallback.sort_by_key(|s| std::cmp::Reverse(s["updated_at"].as_u64()));
            let fallback_truncated = fallback.len() > limit;
            fallback.truncate(limit);
            queries.spawn(async move {
                let request_id = Uuid::new_v4();
                struct Cleanup {
                    hub: Hub,
                    id: Uuid,
                }
                impl Drop for Cleanup {
                    fn drop(&mut self) {
                        if let Ok(mut state) = self.hub.state() {
                            state.session_queries.remove(&self.id);
                        }
                    }
                }
                let _cleanup = Cleanup {
                    hub: hub.clone(),
                    id: request_id,
                };
                let response = (|| {
                    let mut state = hub.state()?;
                    let sender = hub.sender(&state, &machine.id)?;
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    state
                        .session_queries
                        .insert(request_id, (machine.id.clone(), tx));
                    sender.send(Body::ListSessions {
                        request_id,
                        agent,
                        cwd,
                        limit: Some(limit),
                    })?;
                    Ok::<_, HubError>(rx)
                })();
                let sessions = match response {
                    Ok(rx) => tokio::time::timeout(Duration::from_secs(10), rx)
                        .await
                        .ok()
                        .and_then(Result::ok),
                    Err(_) => None,
                };
                sessions
                    .map(|(sessions, truncated)| {
                        (
                            sessions
                                .into_iter()
                                .map(|s| {
                                    let mut value = serde_json::to_value(s).unwrap_or_default();
                                    value["machine"] = json!(machine.id);
                                    value["stale"] = json!(false);
                                    value
                                })
                                .collect::<Vec<_>>(),
                            truncated,
                            false,
                        )
                    })
                    .unwrap_or((fallback, fallback_truncated, true))
            });
        }
        let mut sessions = vec![];
        let mut truncated = false;
        let mut stale = false;
        while let Some(result) = queries.join_next().await {
            let (entries, more, unavailable) =
                result.map_err(|e| HubError::Invalid(e.to_string()))?;
            sessions.extend(entries);
            truncated |= more;
            stale |= unavailable;
        }
        sessions.sort_by_key(|s| {
            std::cmp::Reverse(
                s["updated_at"]
                    .as_i64()
                    .or_else(|| navarch_proto::session_timestamp(s["updated_at"].as_str())),
            )
        });
        Ok(json!({"sessions":sessions,"truncated":truncated,"stale":stale,"corrupt":corrupt}))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use navarch_proto::{CardId, TaskId};
    use navarch_store::{Machine, MachineState, MemoryStore, Step, Store, TaskCard};
    use navarch_transport::{
        Connector, Link, LinkError, ReliableEvent, ReliableTask, memory_pair, reliable,
    };
    use std::sync::Arc;
    struct Once(Option<Box<dyn Link>>);
    #[async_trait::async_trait]
    impl Connector for Once {
        async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
            match self.0.take() {
                Some(link) => Ok(link),
                None => std::future::pending().await,
            }
        }
    }
    async fn fixture() -> (Hub, MachineId, ReliableTask, navarch_transport::MemoryLink) {
        let store = Arc::new(MemoryStore::default());
        let hub = Hub::new(store.clone());
        let local: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let machine: MachineId = "77777777777777777777777774".parse().unwrap();
        store
            .put_machine(Machine {
                id: machine.clone(),
                public_key: [0; 32],
                static_public_key: None,
                display_name: "test".into(),
                state: MachineState::Active,
                capabilities: None,
            })
            .unwrap();
        store
            .put_task(TaskCard {
                id: CardId::new(),
                title: "work".into(),
                project: None,
                state: "finished".into(),
                steps: vec![Step {
                    task_id: TaskId::new(),
                    created_at: 1,
                    role: None,
                    agent: "fake".into(),
                    machine: machine.clone(),
                    session: Some("public".into()),
                    adapter_session_id: None,
                    cwd: Some("/work".into()),
                    state: "finished".into(),
                    result_summary: "done".into(),
                }],
                log: vec![],
                applied_events: vec![],
                permissions: vec![],
            })
            .unwrap();
        let (link, remote) = memory_pair(local.clone(), machine.clone());
        let (sender, mut events, task) = reliable(local, Box::new(Once(Some(Box::new(link)))), 64);
        assert!(matches!(
            events.recv().await,
            Some(ReliableEvent::Connected(_))
        ));
        hub.attach(machine.clone(), sender).unwrap();
        // Keep the event receiver alive so reliable delivery stays connected.
        let event_sink = tokio::spawn(async move { while events.recv().await.is_some() {} });
        // The sink exits when the returned reliable task drops and closes events.
        drop(event_sink);
        (hub, machine, task, remote)
    }
    #[tokio::test(start_paused = true)]
    async fn queries_time_out_at_ten_seconds_and_return_stale_observations() {
        let (hub, machine, _connection, _remote) = fixture().await;
        let before = tokio::time::Instant::now();
        let result = hub
            .request_async(Request::ListSessions {
                agent: None,
                limit: None,
                machine: Some(machine),
                cwd: Some("/work".into()),
            })
            .await
            .unwrap();
        assert_eq!(before.elapsed(), Duration::from_secs(10));
        assert_eq!(result["sessions"][0]["session_id"], "public");
        assert_eq!(result["sessions"][0]["stale"], true);
        assert_eq!(result["stale"], true);
        assert_eq!(result["truncated"], false);
        assert!(hub.state().unwrap().session_queries.is_empty());
    }
    #[tokio::test]
    async fn offline_fallback_honours_filters_and_limit() {
        let (hub, machine, _connection, _remote) = fixture().await;
        let token = hub.state().unwrap().online[&machine].token;
        hub.detach(&machine, token).unwrap();
        for (agent, cwd, limit, count, truncated) in [
            (Some("fake"), Some("/work"), Some(1), 1, false),
            (Some("other"), None, None, 0, false),
            (None, Some("/other"), None, 0, false),
            (None, None, Some(0), 0, true),
        ] {
            let result = hub
                .request_async(Request::ListSessions {
                    machine: Some(machine.clone()),
                    agent: agent.map(str::to_owned),
                    cwd: cwd.map(PathBuf::from),
                    limit,
                })
                .await
                .unwrap();
            assert_eq!(result["sessions"].as_array().unwrap().len(), count);
            assert_eq!(result["truncated"], truncated);
            assert_eq!(result["stale"], true);
        }
    }

    #[tokio::test]
    async fn cancelling_a_query_removes_its_waiter() {
        let (hub, machine, _connection, mut remote) = fixture().await;
        let h = hub.clone();
        let query = tokio::spawn(async move {
            h.request_async(Request::ListSessions {
                agent: None,
                limit: None,
                machine: Some(machine),
                cwd: None,
            })
            .await
        });
        let frame = remote.recv().await.unwrap();
        let envelope: navarch_proto::Envelope = serde_json::from_slice(&frame).unwrap();
        assert!(matches!(envelope.body, Body::ListSessions { .. }));
        assert_eq!(hub.state().unwrap().session_queries.len(), 1);
        query.abort();
        let _ = query.await;
        tokio::task::yield_now().await;
        assert!(hub.state().unwrap().session_queries.is_empty());
    }
    #[tokio::test]
    async fn registry_sessions_not_yet_observed_by_hub_can_receive_prompts() {
        use navarch_proto::{Envelope, WorkerSession};
        let (hub, machine, _connection, mut remote) = fixture().await;
        let h = hub.clone();
        let target = machine.clone();
        let send = tokio::spawn(async move {
            h.request_async(Request::SendToSession {
                machine: target,
                session_id: "standalone-public".into(),
                agent: None,
                card_id: None,
                prompt: "continue".into(),
            })
            .await
        });
        let query: Envelope = serde_json::from_slice(&remote.recv().await.unwrap()).unwrap();
        let Body::ListSessions { request_id, .. } = query.body else {
            panic!("expected query");
        };
        hub.apply(&Envelope {
            v: 1,
            id: Uuid::new_v4(),
            from: machine,
            to: query.from,
            body: Body::SessionList {
                request_id,
                truncated: false,
                sessions: vec![WorkerSession {
                    session_id: Some("standalone-public".into()),
                    agent: "fake".into(),
                    adapter_session_id: "adapter".into(),
                    cwd: "/work".into(),
                    title: None,
                    updated_at: None,
                    navarch_started: true,
                }],
            },
        })
        .unwrap();
        let result = send.await.unwrap().unwrap();
        let command: Envelope = serde_json::from_slice(&remote.recv().await.unwrap()).unwrap();
        assert!(
            matches!(command.body, Body::SendToSession { session_id, agent:Some(agent), .. } if session_id == "adapter" && agent == "fake")
        );
        let card_id = serde_json::from_value(result["card_id"].clone()).unwrap();
        let card = hub.store.task(card_id).unwrap().unwrap();
        assert_eq!(card.steps[0].agent, "fake");
        assert_eq!(card.steps[0].adapter_session_id.as_deref(), Some("adapter"));
    }
}
