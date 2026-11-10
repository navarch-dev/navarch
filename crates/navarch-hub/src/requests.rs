use crate::{Hub, HubError, control::Request, runtime::State};
use navarch_proto::{Body, Envelope, TaskId};
use navarch_store::{MachineState, Step};
use serde_json::{Value, json};
use uuid::Uuid;
impl Hub {
    /// Execute one typed control operation. This also serializes state changes
    /// against incoming worker events so responses and cards remain consistent.
    pub fn request(&self, request: Request) -> Result<Value, HubError> {
        let state = self.state()?;
        match request {
            Request::ListWorkers => Ok(Value::Array(
                self.store
                    .machines()?
                    .into_iter()
                    .map(|m| json!({"machine":m,"online":state.online.contains_key(&m.id)}))
                    .collect(),
            )),
            Request::ListSessions { machine } => {
                let (cards, corrupt) = self.store.tasks()?;
                let mut sessions = std::collections::BTreeMap::new();
                for card in cards {
                    for step in card.steps {
                        if machine.as_ref().is_none_or(|m| m == &step.machine)
                            && let Some(session) = &step.session
                        {
                            sessions.insert((step.machine.clone(),session.clone()), json!({"session_id":session,"machine":step.machine,"agent":step.agent}));
                        }
                    }
                }
                Ok(json!({"sessions":sessions.into_values().collect::<Vec<_>>(),"corrupt":corrupt}))
            }
            Request::Dispatch {
                machine,
                agent,
                role,
                cwd,
                mut prompt,
                title,
                project,
                mut mode,
                mut model,
                mcp_servers,
            } => {
                let sender = self.sender(&state, &machine)?;
                if cwd.as_os_str().is_empty() {
                    return Err(HubError::Invalid("cwd must be non-empty".into()));
                }
                let agent = match (&agent, &role) {
                    (Some(agent), None) => agent.clone(),
                    (None, Some(name)) => {
                        let role = self
                            .store
                            .role(name)?
                            .ok_or_else(|| HubError::Invalid(format!("unknown role {name}")))?;
                        mode = mode.or(role.mode);
                        model = model.or(role.model);
                        if !role.preferences.trim().is_empty() {
                            prompt.push_str("\n\nRole preferences:\n");
                            prompt.push_str(&role.preferences);
                        }
                        role.agent
                    }
                    _ => {
                        return Err(HubError::Invalid(
                            "name exactly one of agent or role".into(),
                        ));
                    }
                };
                if !self
                    .machine(&machine)?
                    .capabilities
                    .as_ref()
                    .is_some_and(|c| c.agents.iter().any(|a| a.name == agent))
                {
                    return Err(HubError::Invalid(format!(
                        "machine {machine} has not reported agent {agent}"
                    )));
                }
                if let Some(name) = &project {
                    let projects = self.store.projects()?;
                    if !projects
                        .projects
                        .iter()
                        .any(|(n, p)| n == name || p.aliases.contains(name))
                    {
                        return Err(HubError::Invalid(format!("unknown project {name}")));
                    }
                }
                let task_id = TaskId::new();
                let title = title.unwrap_or_else(|| prompt.chars().take(120).collect());
                self.create(
                    sender,
                    Step {
                        role,
                        agent: agent.clone(),
                        machine: machine.clone(),
                        session: None,
                        state: "queued".into(),
                        result_summary: String::new(),
                    },
                    title,
                    project,
                    Body::Dispatch {
                        task_id,
                        agent,
                        cwd,
                        prompt,
                        mode,
                        model,
                        mcp_servers,
                    },
                )
            }
            Request::SendToSession {
                machine,
                session_id,
                prompt,
            } => {
                let sender = self.sender(&state, &machine)?;
                let (cards, _) = self.store.tasks()?;
                let mut step = cards
                    .into_iter()
                    .flat_map(|c| c.steps)
                    .find(|s| s.machine == machine && s.session.as_ref() == Some(&session_id))
                    .ok_or_else(|| {
                        HubError::Invalid(format!("unknown session {session_id} on {machine}"))
                    })?;
                step.state = "queued".into();
                step.result_summary.clear();
                let task_id = TaskId::new();
                let title = prompt.chars().take(120).collect();
                self.create(
                    sender,
                    step,
                    title,
                    None,
                    Body::SendToSession {
                        task_id,
                        session_id,
                        prompt,
                    },
                )
            }
            Request::Cancel { task_id } => {
                let card = self.card(task_id)?;
                let step = card
                    .steps
                    .last()
                    .ok_or_else(|| HubError::Invalid("task has no steps".into()))?;
                self.sender(&state, &step.machine)?
                    .send(Body::Cancel { task_id })?;
                Ok(json!({"queued":true}))
            }
            Request::GetTask { task_id } => Ok(serde_json::to_value(self.card(task_id)?)?),
            Request::ListTasks { include_finished } => {
                let (cards, corrupt) = self.store.tasks()?;
                Ok(
                    json!({"tasks":cards.into_iter().filter(|c| include_finished || c.is_active()).collect::<Vec<_>>(),"corrupt":corrupt}),
                )
            }
            Request::ReadInbox => Ok(serde_json::to_value(self.store.inbox(true)?)?),
            Request::AnswerPermission {
                request_id,
                decision,
            } => self.answer(&state, request_id, decision),
            Request::ApproveMachine { machine } => {
                let mut machine = self.machine(&machine)?;
                if machine.state == MachineState::Revoked {
                    return Err(HubError::Invalid(
                        "revoked machine cannot be approved".into(),
                    ));
                }
                machine.state = MachineState::Active;
                self.store.put_machine(machine)?;
                Ok(json!({"approved":true}))
            }
        }
    }
    fn answer(
        &self,
        state: &State,
        request_id: Uuid,
        decision: navarch_proto::PermissionDecision,
    ) -> Result<Value, HubError> {
        let (cards, _) = self.store.tasks()?;
        for mut card in cards {
            if card.log.iter().any(
                |e| matches!(e.body, Body::PermissionAnswer {request_id:id,..} if id == request_id),
            ) {
                return Err(HubError::Invalid("permission already answered".into()));
            }
            let ask = card.log.iter().find(|e| matches!(e.body, Body::PermissionAsk {request_id:id,..} if id == request_id)).cloned();
            if let Some(ask) = ask {
                if !card.is_active() {
                    return Err(HubError::Invalid("permission turn has ended".into()));
                }
                if let (
                    navarch_proto::PermissionDecision::Selected { option_id },
                    Body::PermissionAsk { options, .. },
                ) = (&decision, &ask.body)
                    && !options.iter().any(|o| &o.id == option_id)
                {
                    return Err(HubError::Invalid(
                        "permission option was not offered".into(),
                    ));
                }
                let body = Body::PermissionAnswer {
                    request_id,
                    decision,
                };
                self.sender(state, &ask.from)?.send(body.clone())?;
                card.log.push(Envelope {
                    v: navarch_proto::PROTOCOL_VERSION,
                    id: Uuid::new_v4(),
                    from: ask.to,
                    to: ask.from,
                    body,
                });
                card.state = "running".into();
                if let Some(step) = card.steps.last_mut() {
                    step.state = "running".into();
                }
                self.store.put_task(card)?;
                return Ok(json!({"queued":true}));
            }
        }
        Err(HubError::Invalid("unknown permission request".into()))
    }
}
