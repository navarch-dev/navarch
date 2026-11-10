//! Turn scheduling, streaming and cancellation.
use super::*;
impl Worker {
    pub(super) fn schedule(&self) {
        let Ok(mut state) = self.state() else {
            return;
        };
        while state.running + state.starting < self.policy().max_concurrent_turns {
            let Some(id) = state
                .sessions
                .iter()
                .find(|(_, s)| s.active.is_none() && !s.queue.is_empty())
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            let Some(session) = state.sessions.get_mut(&id) else {
                break;
            };
            let Some(turn) = session.queue.pop_front() else {
                break;
            };
            session.active = Some(turn.task_id);
            let restored = session.restored;
            let Some(record) = state.registry.records.get_mut(&id) else {
                break;
            };
            record.state = SessionState::Running;
            record.updated_at = now();
            let record = record.clone();
            if let Err(e) = state.registry.save() {
                self.emit(Body::Failed {
                    task_id: turn.task_id,
                    error: e.to_string(),
                });
                if let Some(session) = state.sessions.get_mut(&id) {
                    session.active = None;
                }
                continue;
            }
            state.running += 1;
            let worker = self.clone();
            tokio::spawn(async move {
                worker.run_turn(id, record, turn, restored).await;
            });
        }
    }
    async fn run_turn(&self, id: String, record: SessionRecord, turn: Turn, restored: bool) {
        let context_key = (record.agent.clone(), record.adapter_session_id.clone());
        if let Ok(mut contexts) = self.inner.permissions.contexts.lock() {
            contexts.insert(
                context_key.clone(),
                (turn.task_id, record.config.cwd.clone(), id.clone()),
            );
        }
        let result = async {
            let driver = self
                .drivers()
                .get(&record.agent)
                .ok_or_else(|| WorkerError::Invalid(format!("missing driver {}", record.agent)))?;
            if !restored {
                driver
                    .resume_session(ResumeSession {
                        session_id: record.adapter_session_id.clone(),
                        config: record.config,
                    })
                    .await?;
            }
            let (tx, mut rx) = mpsc::unbounded_channel();
            let output = self.inner.events.clone();
            let messages = self.inner.output.clone();
            let task_id = turn.task_id;
            let stream = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if let AgentEvent::ToolCall(summary) | AgentEvent::Plan(summary) = &event {
                        let _ = messages.send(Body::Progress {
                            task_id,
                            summary: summary.clone(),
                        });
                    }
                    let _ = output.send(TextEvent { task_id, event });
                }
            });
            let result = driver
                .prompt(&record.adapter_session_id, &turn.prompt, tx)
                .await;
            let _ = stream.await;
            result.map_err(WorkerError::from)
        }
        .await;
        self.inner.permissions.cancel(turn.task_id);
        if let Ok(mut contexts) = self.inner.permissions.contexts.lock() {
            contexts.remove(&context_key);
        }
        let succeeded = result.is_ok();
        let mut completion = match result {
            Ok(outcome) => Body::Finished {
                task_id: turn.task_id,
                session_id: id.clone(),
                stop_reason: outcome.stop_reason,
                result: outcome.result,
            },
            Err(e) => Body::Failed {
                task_id: turn.task_id,
                error: e.to_string(),
            },
        };
        if let Ok(mut state) = self.state() {
            state.running = state.running.saturating_sub(1);
            let queued = if let Some(session) = state.sessions.get_mut(&id) {
                session.active = None;
                session.restored = succeeded;
                !session.queue.is_empty()
            } else {
                false
            };
            if let Some(record) = state.registry.records.get_mut(&id) {
                record.state = if queued {
                    SessionState::Queued
                } else {
                    SessionState::Idle
                };
                record.updated_at = now();
            }
            if let Err(e) = state.registry.save() {
                completion = Body::Failed {
                    task_id: turn.task_id,
                    error: e.to_string(),
                };
            }
        }
        self.emit(completion);
        self.schedule();
    }
    /// Cancel a running turn or remove a queued task.
    pub async fn cancel(&self, task_id: TaskId) -> Result<(), WorkerError> {
        self.inner.permissions.cancel(task_id);
        let target = {
            let mut state = self.state()?;
            let mut active = None;
            for (id, session) in &mut state.sessions {
                if session.active == Some(task_id) {
                    active = Some(id.clone());
                }
                let before = session.queue.len();
                session.queue.retain(|t| t.task_id != task_id);
                if before != session.queue.len() {
                    self.emit(Body::Finished {
                        task_id,
                        session_id: id.clone(),
                        stop_reason: "cancelled".into(),
                        result: String::new(),
                    });
                }
            }
            let statuses: Vec<_> = state
                .sessions
                .iter()
                .map(|(id, s)| {
                    (
                        id.clone(),
                        if s.active.is_some() {
                            SessionState::Running
                        } else if s.queue.is_empty() {
                            SessionState::Idle
                        } else {
                            SessionState::Queued
                        },
                    )
                })
                .collect();
            for (id, status) in statuses {
                if let Some(record) = state.registry.records.get_mut(&id) {
                    record.state = status;
                    record.updated_at = now();
                }
            }
            state.registry.save()?;
            active.and_then(|id| {
                state
                    .registry
                    .records
                    .get(&id)
                    .map(|r| (r.adapter_session_id.clone(), r.agent.clone()))
            })
        };
        if let Some((id, agent)) = target {
            self.drivers()
                .get(&agent)
                .ok_or_else(|| WorkerError::Invalid(format!("missing driver {agent}")))?
                .cancel(&id)
                .await?;
        }
        Ok(())
    }
    /// Deliver a human answer; unknown or expired request IDs are ignored.
    pub fn answer_permission(&self, id: uuid::Uuid, decision: navarch_proto::PermissionDecision) {
        self.inner.permissions.answer(id, decision);
    }
}
