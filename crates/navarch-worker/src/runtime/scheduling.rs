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
            let (tx, rx) = mpsc::unbounded_channel();
            let output = self.inner.events.clone();
            let messages = self.inner.output.clone();
            let task_id = turn.task_id;
            let stream = tokio::spawn(stream_events(rx, output, messages, task_id));
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

async fn stream_events(
    mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    output: mpsc::UnboundedSender<TextEvent>,
    messages: mpsc::UnboundedSender<Body>,
    task_id: TaskId,
) {
    use tokio::time::{Duration, Instant, sleep_until};
    let mut next = Instant::now();
    let mut pending = None;
    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break; };
                if let AgentEvent::ToolCall(summary) | AgentEvent::Plan(summary) = &event {
                    pending = Some(navarch_proto::short_summary(summary));
                    if Instant::now() >= next {
                        if let Some(summary) = pending.take() { let _ = messages.send(Body::Progress { task_id, summary }); }
                        next = Instant::now() + Duration::from_secs(1);
                    }
                }
                let _ = output.send(TextEvent { task_id, event });
            }
            _ = sleep_until(next), if pending.is_some() => {
                if let Some(summary) = pending.take() { let _ = messages.send(Body::Progress { task_id, summary }); }
                next = Instant::now() + Duration::from_secs(1);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn progress_coalesces_latest_update_at_one_second_and_caps_unicode() {
        let task_id = TaskId::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::unbounded_channel();
        let (messages, mut progress) = mpsc::unbounded_channel();
        let stream = tokio::spawn(stream_events(rx, out, messages, task_id));
        tx.send(AgentEvent::ToolCall("First: running".into()))
            .unwrap();
        events.recv().await.unwrap();
        assert!(
            matches!(progress.try_recv().unwrap(), Body::Progress { task_id: id, summary } if id == task_id && summary == "First: running")
        );
        for i in 0..20 {
            tx.send(AgentEvent::Plan(format!("Update {i}"))).unwrap();
            events.recv().await.unwrap();
        }
        assert!(progress.try_recv().is_err());
        tokio::time::advance(std::time::Duration::from_millis(999)).await;
        assert!(progress.try_recv().is_err());
        tx.send(AgentEvent::ToolCall("界".repeat(300))).unwrap();
        events.recv().await.unwrap();
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            matches!(progress.try_recv().unwrap(), Body::Progress { summary, .. } if summary == "界".repeat(200))
        );
        tx.send(AgentEvent::Plan("Too soon".into())).unwrap();
        events.recv().await.unwrap();
        tx.send(AgentEvent::MessageChunk(
            "Final text is streamed without Progress".into(),
        ))
        .unwrap();
        events.recv().await.unwrap();
        drop(tx);
        stream.await.unwrap();
        assert!(progress.try_recv().is_err());
    }
}
