//! Session creation and prompt submission.
use super::*;
impl Worker {
    /// Start a policy-approved session, reserving global capacity before adapter startup.
    pub async fn dispatch(
        &self,
        task_id: TaskId,
        agent: &str,
        mut req: NewSession,
        prompt: String,
    ) -> Result<(), WorkerError> {
        let result = self.dispatch_inner(task_id, agent, &mut req, prompt).await;
        if let Err(e) = &result {
            self.emit(Body::Rejected {
                task_id,
                reason: e.to_string(),
            });
        }
        self.schedule();
        result
    }
    async fn dispatch_inner(
        &self,
        task_id: TaskId,
        agent: &str,
        req: &mut NewSession,
        prompt: String,
    ) -> Result<(), WorkerError> {
        req.cwd = canonical_cwd(&req.cwd)?;
        self.policy().check_dispatch(&req.cwd, agent)?;
        let config = self
            .config()
            .agents
            .iter()
            .find(|a| a.name == agent)
            .ok_or_else(|| {
                WorkerError::Invalid(format!("worker.toml agents: unknown agent {agent}"))
            })?;
        if req.mode.is_none() {
            req.mode = config.mode();
        }
        let driver = self
            .drivers()
            .get(agent)
            .ok_or_else(|| WorkerError::Invalid(format!("missing driver {agent}")))?;
        {
            let mut state = self.state()?;
            if state.running + state.starting >= self.policy().max_concurrent_turns {
                return Err(WorkerError::Invalid(
                    "max_concurrent_turns refused dispatch".into(),
                ));
            }
            state.starting += 1;
        }
        let handle = driver.new_session(req.clone()).await;
        {
            let mut state = self.state()?;
            state.starting -= 1;
            let handle = handle?;
            if state.registry.records.values().any(|record| {
                record.agent == agent && record.adapter_session_id == handle.session_id
            }) {
                return Err(WorkerError::Invalid(
                    "adapter returned duplicate session ID".into(),
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = now();
            state.registry.records.insert(
                id.clone(),
                SessionRecord {
                    task_id,
                    agent: agent.into(),
                    session_id: id.clone(),
                    adapter_session_id: handle.session_id,
                    config: req.clone(),
                    state: SessionState::Queued,
                    created_at: timestamp,
                    updated_at: timestamp,
                },
            );
            state.registry.save()?;
            state.sessions.insert(
                id.clone(),
                Session {
                    queue: VecDeque::from([Turn { task_id, prompt }]),
                    active: None,
                    restored: true,
                },
            );
            self.emit(Body::Accepted {
                task_id,
                session_id: id,
            });
        }
        self.schedule();
        Ok(())
    }
    /// Queue a prompt. Existing busy sessions retain FIFO order.
    pub async fn send(
        &self,
        task_id: TaskId,
        id: &SessionId,
        prompt: String,
    ) -> Result<(), WorkerError> {
        let result = (|| {
            let mut state = self.state()?;
            let record = state
                .registry
                .records
                .get(id)
                .ok_or_else(|| WorkerError::Invalid(format!("unknown session {id}")))?;
            let cwd = canonical_cwd(&record.config.cwd)?;
            self.policy().check_dispatch(&cwd, &record.agent)?;
            if !self.drivers().contains_key(&record.agent) {
                return Err(WorkerError::Invalid(format!(
                    "worker.toml agents: unknown agent {}",
                    record.agent
                )));
            }
            let record = state
                .registry
                .records
                .get_mut(id)
                .ok_or_else(|| WorkerError::Invalid(format!("unknown session {id}")))?;
            record.task_id = task_id;
            record.updated_at = now();
            if record.state != SessionState::Running {
                record.state = SessionState::Queued;
            }
            state.registry.save()?;
            state
                .sessions
                .get_mut(id)
                .ok_or_else(|| WorkerError::Invalid(format!("unknown session {id}")))?
                .queue
                .push_back(Turn { task_id, prompt });
            self.emit(Body::Accepted {
                task_id,
                session_id: id.clone(),
            });
            Ok(())
        })();
        if let Err(e) = &result {
            self.emit(Body::Rejected {
                task_id,
                reason: e.to_string(),
            });
        }
        self.schedule();
        result
    }
}
