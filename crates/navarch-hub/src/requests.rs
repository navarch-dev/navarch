use crate::runtime::timestamp;
use crate::{Hub, HubError, control::Request, runtime::State};
use navarch_proto::{Body, TaskId};
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
                    .map(|m| json!({"machine":m,"online":state.online.contains_key(&m.id),"connection":state.online.get(&m.id).map(|c|c.sender.connection_kind())}))
                    .collect(),
            )),
            Request::Subscribe { .. } | Request::SubscribeLive | Request::WaitForEvents { .. } => Err(HubError::Invalid(
                "use the asynchronous event control path".into(),
            )),
            Request::ListSessions { .. } => Err(HubError::Invalid(
                "use request_async for worker session discovery".into(),
            )),
            Request::Dispatch {
                card_id,
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
                        task_id,
                        created_at: timestamp(),
                        role,
                        agent: agent.clone(),
                        machine: machine.clone(),
                        session: None,
                        adapter_session_id: None,
                        cwd: Some(cwd.clone()),
                        state: "queued".into(),
                        result_summary: String::new(),
                    },
                    card_id,
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
                card_id,
                agent,
            } => {
                let sender = self.sender(&state, &machine)?;
                let (cards, _) = self.store.tasks()?;
                let owner = cards
                    .iter()
                    .flat_map(|c| c.steps.iter().map(move |s| (c.id, s)))
                    .filter(|(_, s)| {
                        s.machine == machine
                            && if let Some(agent) = &agent {
                                &s.agent == agent
                                    && s.adapter_session_id.as_ref() == Some(&session_id)
                            } else {
                                s.session.as_ref() == Some(&session_id)
                            }
                    })
                    .max_by_key(|(_, s)| s.created_at);
                let task_id = TaskId::new();
                let (owner_id, mut step) = if let Some((id, step)) = owner {
                    (Some(id), step.clone())
                } else if let Some(name) = &agent {
                    (
                        None,
                        Step {
                            task_id,
                            created_at: timestamp(),
                            role: None,
                            agent: name.clone(),
                            machine: machine.clone(),
                            session: None,
                            adapter_session_id: Some(session_id.clone()),
                            cwd: None,
                            state: String::new(),
                            result_summary: String::new(),
                        },
                    )
                } else {
                    return Err(HubError::Invalid(format!(
                        "unknown session {session_id} on {machine}; supply agent and adapter session ID to adopt it"
                    )));
                };
                step.task_id = task_id;
                step.created_at = timestamp();
                step.state = "queued".into();
                step.result_summary.clear();
                let title = prompt.chars().take(120).collect();
                self.create(
                    sender,
                    step,
                    card_id.or(owner_id),
                    title,
                    None,
                    Body::SendToSession {
                        task_id,
                        session_id,
                        prompt,
                        agent,
                    },
                )
            }
            Request::Cancel { task_id } => {
                let (cards, _) = self.store.tasks()?;
                let step = cards
                    .iter()
                    .flat_map(|c| &c.steps)
                    .find(|s| s.task_id == task_id)
                    .ok_or_else(|| HubError::Invalid(format!("unknown turn {task_id}")))?;
                self.sender(&state, &step.machine)?
                    .send(Body::Cancel { task_id })?;
                Ok(json!({"queued":true}))
            }
            Request::GetTask {
                task_id,
                include_log,
            } => {
                let mut value = serde_json::to_value(self.card(task_id)?)?;
                clean_card(&mut value, include_log);
                Ok(value)
            }
            Request::ListTasks { include_finished } => {
                let (cards, corrupt) = self.store.tasks()?;
                let tasks = cards.into_iter().filter(|c| include_finished || c.is_active())
                    .map(|c| json!({"id":c.id,"title":c.title,"project":c.project,"state":c.state,"step_count":c.steps.len()})).collect::<Vec<_>>();
                Ok(json!({"tasks":tasks,"corrupt":corrupt}))
            }
            Request::ReadInbox => Ok(serde_json::to_value(self.store.inbox(true)?)?),
            Request::AnswerPermission {
                request_id,
                decision,
            } => self.answer(&state, request_id, decision),
            Request::RegisterPaired { peer } => Ok(json!({"machine":self.put_paired(peer)?})),
            Request::RevokeMachine { machine } => {
                let mut machine = self.machine(&machine)?;
                machine.state = MachineState::Revoked;
                self.store.put_machine(machine)?;
                Ok(json!({"revoked":true}))
            }
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
            let Some(index) = card
                .permissions
                .iter()
                .position(|p| p.request_id == request_id)
            else {
                continue;
            };
            let permission = &card.permissions[index];
            if permission.answered {
                return Err(HubError::Invalid("permission already answered".into()));
            }
            let step = card
                .steps
                .iter_mut()
                .find(|s| s.task_id == permission.step)
                .ok_or_else(|| HubError::Invalid("permission step is missing".into()))?;
            if matches!(step.state.as_str(), "finished" | "failed" | "cancelled") {
                return Err(HubError::Invalid("permission turn has ended".into()));
            }
            if let navarch_proto::PermissionDecision::Selected { option_id } = &decision
                && !permission.options.iter().any(|o| &o.id == option_id)
            {
                return Err(HubError::Invalid(
                    "permission option was not offered".into(),
                ));
            }
            self.sender(state, &step.machine)?
                .send(Body::PermissionAnswer {
                    request_id,
                    decision,
                })?;
            card.permissions[index].answered = true;
            step.state = "running".into();
            card.log.push(navarch_store::CardLog {
                time: timestamp() / 1_000_000,
                kind: "permission_answer".into(),
                step: step.task_id,
                summary: "Permission answered".into(),
            });
            card.derive_state();
            self.store.put_task(card)?;
            return Ok(json!({"queued":true}));
        }
        Err(HubError::Invalid("unknown permission request".into()))
    }
}
fn clean_card(value: &mut Value, include_log: bool) {
    if let Some(object) = value.as_object_mut() {
        object.remove("applied_events");
        object.remove("permissions");
        if !include_log {
            object.remove("log");
        }
    }
}
