//! Adapter discovery and policy-checked adoption of external sessions.
use super::*;
use navarch_proto::WorkerSession;
impl Worker {
    /// Merge registry sessions with adapter listings, capped at 500 per agent.
    pub async fn list_sessions(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<WorkerSession>, WorkerError> {
        let canonical = cwd.map(canonical_cwd).transpose()?;
        let cwd = canonical.as_deref();
        let records = self.sessions()?;
        let mut result = vec![];
        let mut agents: Vec<_> = self.drivers().iter().collect();
        agents.sort_by_key(|(name, _)| *name);
        for (agent, driver) in agents {
            let mut merged = std::collections::BTreeMap::new();
            for record in records
                .iter()
                .filter(|r| &r.agent == agent && cwd.is_none_or(|p| p == r.config.cwd))
            {
                merged.insert(
                    record.adapter_session_id.clone(),
                    WorkerSession {
                        session_id: Some(record.session_id.clone()),
                        agent: agent.clone(),
                        adapter_session_id: record.adapter_session_id.clone(),
                        cwd: record.config.cwd.clone(),
                        title: None,
                        updated_at: Some(record.updated_at.to_string()),
                        navarch_started: record.navarch_started,
                    },
                );
            }
            match driver.list_sessions(cwd).await {
                Ok(sessions) => {
                    for session in sessions.into_iter().take(500) {
                        if cwd.is_some_and(|p| p != session.cwd) {
                            continue;
                        }
                        if let Some(existing) = merged.get_mut(&session.session_id) {
                            existing.title = session.title;
                            if session.updated_at.is_some() {
                                existing.updated_at = session.updated_at;
                            }
                        } else {
                            merged.insert(
                                session.session_id.clone(),
                                WorkerSession {
                                    session_id: None,
                                    agent: agent.clone(),
                                    adapter_session_id: session.session_id,
                                    cwd: session.cwd,
                                    title: session.title,
                                    updated_at: session.updated_at,
                                    navarch_started: false,
                                },
                            );
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%agent, %error, "session discovery unavailable; retaining registry sessions")
                }
            }
            // Preserve registry sessions before filling remaining capacity from adapters.
            let mut sessions: Vec<_> = merged.into_values().collect();
            sessions.sort_by_key(|s| (s.session_id.is_none(), s.adapter_session_id.clone()));
            result.extend(sessions.into_iter().take(500));
        }
        Ok(result)
    }
    /// Address a registry ID, or an adapter-scoped ID which is adopted on first use.
    pub async fn send_target(
        &self,
        task_id: TaskId,
        id: &str,
        agent: Option<&str>,
        prompt: String,
    ) -> Result<(), WorkerError> {
        let target = self.resolve_session(task_id, id, agent).await;
        match target {
            Ok(id) => self.send(task_id, &id, prompt).await,
            Err(error) => {
                self.emit(Body::Rejected {
                    task_id,
                    reason: error.to_string(),
                });
                Err(error)
            }
        }
    }
    async fn resolve_session(
        &self,
        task_id: TaskId,
        id: &str,
        agent: Option<&str>,
    ) -> Result<String, WorkerError> {
        if agent.is_none() {
            return self
                .state()?
                .registry
                .records
                .get(id)
                .map(|r| r.session_id.clone())
                .ok_or_else(|| {
                    WorkerError::Invalid(format!(
                        "unknown session {id}; supply agent to adopt an adapter session"
                    ))
                });
        }
        let agent = agent.ok_or_else(|| WorkerError::Invalid("agent required".into()))?;
        if let Some(record) = self
            .sessions()?
            .into_iter()
            .find(|r| r.agent == agent && r.adapter_session_id == id)
        {
            return Ok(record.session_id);
        }
        // Check the agent allowlist before contacting its adapter. The listed cwd
        // is canonicalized and checked separately before any registry mutation.
        if !self.policy().allowed_agents.iter().any(|a| a == agent) {
            return Err(WorkerError::Invalid(format!(
                "allowed_agents refused {agent}"
            )));
        }
        let driver = self
            .drivers()
            .get(agent)
            .ok_or_else(|| WorkerError::Invalid(format!("unknown agent {agent}")))?;
        let session = driver
            .list_sessions(None)
            .await?
            .into_iter()
            .take(500)
            .find(|s| s.session_id == id)
            .ok_or_else(|| {
                WorkerError::Invalid(format!("adapter {agent} did not report session {id}"))
            })?;
        let cwd = canonical_cwd(&session.cwd)?;
        self.policy().check_dispatch(&cwd, agent)?;
        let mode = self
            .config()
            .agents
            .iter()
            .find(|a| a.name == agent)
            .and_then(AgentConfig::mode);
        let mut state = self.state()?;
        // A concurrent adoption may have completed while listing the adapter.
        if let Some(record) = state
            .registry
            .records
            .values()
            .find(|r| r.agent == agent && r.adapter_session_id == id)
        {
            return Ok(record.session_id.clone());
        }
        let public_id = uuid::Uuid::new_v4().to_string();
        let timestamp = now();
        state.registry.records.insert(
            public_id.clone(),
            SessionRecord {
                navarch_started: false,
                task_id,
                agent: agent.into(),
                session_id: public_id.clone(),
                adapter_session_id: id.into(),
                config: NewSession {
                    cwd,
                    mode,
                    model: None,
                    mcp_servers: vec![],
                },
                state: SessionState::Idle,
                created_at: timestamp,
                updated_at: timestamp,
            },
        );
        state.registry.save()?;
        state.sessions.insert(
            public_id.clone(),
            Session {
                queue: VecDeque::new(),
                active: None,
                restored: false,
            },
        );
        Ok(public_id)
    }
}
