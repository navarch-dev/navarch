use crate::{Hub, HubError, runtime::timestamp};
use navarch_proto::{Body, Envelope, short_summary};
use navarch_store::{CardLog, CardPermission, InboxEvent, MachineState};
impl Hub {
    /// Apply a worker event durably before acknowledging it, including replay metadata.
    pub fn apply(&self, envelope: &Envelope) -> Result<(), HubError> {
        let mut state = self.state()?;
        envelope
            .check_version()
            .map_err(|e| HubError::Invalid(e.to_string()))?;
        let mut machine = self.machine(&envelope.from)?;
        if machine.state != MachineState::Active {
            return Err(HubError::Invalid("event from inactive machine".into()));
        }
        if let Body::SessionList {
            request_id,
            sessions,
            truncated,
        } = &envelope.body
        {
            // Persist adapter identity and cwd on already observed steps, so a
            // future adapter-addressed follow-up finds the same owning card.
            let (cards, _) = self.store.tasks()?;
            for mut card in cards {
                let mut changed = false;
                for step in card.steps.iter_mut().filter(|s| s.machine == envelope.from) {
                    if let Some(session) = sessions.iter().find(|s| {
                        s.agent == step.agent
                            && s.session_id.is_some()
                            && s.session_id == step.session
                    }) {
                        step.adapter_session_id = Some(session.adapter_session_id.clone());
                        step.cwd = Some(session.cwd.clone());
                        changed = true;
                    }
                }
                if changed {
                    self.store.put_task(card)?;
                }
            }
            if state
                .session_queries
                .get(request_id)
                .is_some_and(|(machine, _)| machine == &envelope.from)
                && let Some((_, reply)) = state.session_queries.remove(request_id)
            {
                let _ = reply.send((sessions.clone(), *truncated));
            }
            return Ok(());
        }
        if let Body::ProbeReport { capabilities } = &envelope.body {
            machine.capabilities = Some(capabilities.clone());
            self.store.put_machine(machine)?;
            return Ok(());
        }
        let task_id = match &envelope.body {
            Body::Accepted { task_id, .. }
            | Body::Rejected { task_id, .. }
            | Body::Progress { task_id, .. }
            | Body::PermissionAsk { task_id, .. }
            | Body::Finished { task_id, .. }
            | Body::Failed { task_id, .. } => *task_id,
            _ => return Err(HubError::Invalid("expected a worker event".into())),
        };
        let (cards, _) = self.store.tasks()?;
        let mut card = cards
            .into_iter()
            .find(|c| c.steps.iter().any(|s| s.task_id == task_id))
            .ok_or_else(|| HubError::Invalid(format!("event for unknown turn {task_id}")))?;
        if card.applied_events.contains(&envelope.id) {
            return Ok(());
        }
        let step = card
            .steps
            .iter_mut()
            .find(|s| s.task_id == task_id)
            .ok_or_else(|| HubError::Invalid("turn has no step".into()))?;
        if step.machine != envelope.from {
            return Err(HubError::Invalid("worker does not own turn".into()));
        }
        let terminal = matches!(step.state.as_str(), "finished" | "failed" | "cancelled");
        let (kind, summary) = match &envelope.body {
            Body::Accepted { session_id, .. } => {
                step.session = Some(session_id.clone());
                if step.state == "queued" {
                    step.state = "running".into();
                }
                ("accepted", "Turn accepted".to_owned())
            }
            Body::Progress { summary, .. } => {
                if !terminal {
                    step.result_summary = short_summary(summary);
                }
                ("progress", summary.clone())
            }
            Body::PermissionAsk {
                request_id,
                tool,
                options,
                ..
            } => {
                if !terminal
                    && !card
                        .permissions
                        .iter()
                        .any(|p| p.request_id == *request_id && p.answered)
                {
                    step.state = "waiting".into();
                }
                if !card.permissions.iter().any(|p| p.request_id == *request_id) {
                    card.permissions.push(CardPermission {
                        request_id: *request_id,
                        step: task_id,
                        options: options.clone(),
                        answered: false,
                    });
                }
                ("permission_ask", format!("Permission required: {tool}"))
            }
            Body::Finished {
                session_id,
                stop_reason,
                result,
                ..
            } => {
                step.session = Some(session_id.clone());
                step.state = if stop_reason == "cancelled" {
                    "cancelled"
                } else {
                    "finished"
                }
                .into();
                step.result_summary = result.clone();
                (
                    "finished",
                    if result.is_empty() {
                        format!("Turn {stop_reason}")
                    } else {
                        result.clone()
                    },
                )
            }
            Body::Failed { error, .. } => {
                step.state = "failed".into();
                step.result_summary = error.clone();
                ("failed", error.clone())
            }
            Body::Rejected { reason, .. } => {
                step.state = "failed".into();
                step.result_summary = reason.clone();
                ("rejected", reason.clone())
            }
            _ => return Err(HubError::Invalid("expected turn event".into())),
        };
        if matches!(
            envelope.body,
            Body::Finished { .. }
                | Body::Failed { .. }
                | Body::Rejected { .. }
                | Body::PermissionAsk { .. }
        ) {
            self.publish_inbox(InboxEvent::worker(
                envelope.clone(),
                Some(card.id),
                kind,
                &summary,
                timestamp() / 1_000_000,
            ))?;
        }
        card.log.push(CardLog {
            time: timestamp() / 1_000_000,
            kind: kind.into(),
            step: task_id,
            summary: short_summary(&summary),
        });
        card.applied_events.push(envelope.id);
        card.derive_state();
        self.store.put_task(card)?;
        Ok(())
    }
}

impl Hub {
    pub(crate) fn publish_inbox(&self, event: InboxEvent) -> Result<(), HubError> {
        if let Some(event) = self.store.append_inbox(event)? {
            let _ = self.events.send(event.event);
        }
        Ok(())
    }
    pub(crate) fn lifecycle(
        &self,
        machine: &navarch_proto::MachineId,
        kind: &str,
    ) -> Result<(), HubError> {
        let event = navarch_store::HubEvent {
            seq: self.store.next_event_seq()?,
            kind: kind.into(),
            card_id: None,
            step: None,
            machine: machine.clone(),
            summary: if kind == "worker_online" {
                "Worker connected"
            } else {
                "Worker disconnected"
            }
            .into(),
            time: timestamp() / 1_000_000,
        };
        let _ = self.events.send(event);
        Ok(())
    }
    /// Subscribe before taking the unread snapshot under the application lock.
    /// Replay/live overlap is removed by the stream's sequence cursor.
    pub fn subscribe_events(
        &self,
        after_seq: Option<u64>,
    ) -> Result<
        (
            Vec<navarch_store::HubEvent>,
            tokio::sync::broadcast::Receiver<navarch_store::HubEvent>,
        ),
        HubError,
    > {
        let _state = self.state()?;
        let receiver = self.events.subscribe();
        let mut replay = Vec::new();
        let mut cursor = after_seq.unwrap_or(0);
        loop {
            let batch = self.store.inbox_after(cursor, false)?;
            if batch.is_empty() {
                break;
            }
            for event in batch {
                cursor = cursor.max(event.event.seq);
                replay.push(event.event);
            }
        }
        Ok((replay, receiver))
    }
    /// Register the receiver and snapshot the durable cursor under the publication lock.
    pub(crate) fn subscribe_live_events(
        &self,
    ) -> Result<
        (
            u64,
            tokio::sync::broadcast::Receiver<navarch_store::HubEvent>,
        ),
        HubError,
    > {
        let _state = self.state()?;
        let receiver = self.events.subscribe();
        Ok((self.store.latest_event_seq()?, receiver))
    }
    /// Wait for compact unread newer events without marking them read.
    pub async fn wait_for_events(
        &self,
        timeout_secs: Option<u64>,
        after_seq: Option<u64>,
    ) -> Result<Vec<navarch_store::HubEvent>, HubError> {
        let mut events = self.events.subscribe();
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(timeout_secs.unwrap_or(300).min(1800));
        loop {
            let unread = self.store.inbox_after(after_seq.unwrap_or(0), false)?;
            if !unread.is_empty() {
                return Ok(unread.into_iter().map(|entry| entry.event).collect());
            }
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Err(_) => return Ok(vec![]),
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return Ok(vec![]),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use navarch_proto::{CardId, MachineId, TaskId};
    use navarch_store::{HubEvent, MemoryStore, Store};
    use std::sync::Arc;
    fn event(seq: u64) -> InboxEvent {
        InboxEvent {
            id: uuid::Uuid::new_v4(),
            envelope: None,
            envelope_omitted: false,
            event: HubEvent {
                seq,
                kind: "machine_pending".into(),
                card_id: None,
                step: None,
                machine: "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
                summary: "Pending approval".into(),
                time: 1,
            },
        }
    }
    #[tokio::test]
    async fn inbox_publication_is_exactly_once_and_replays_only_unread_newer_rows() {
        let store = Arc::new(MemoryStore::default());
        let hub = Hub::new(store.clone());
        let (_, mut live) = hub.subscribe_events(None).unwrap();
        let first = event(0);
        hub.publish_inbox(first.clone()).unwrap();
        hub.publish_inbox(first).unwrap();
        let first = live.recv().await.unwrap();
        assert_eq!(first.seq, 1);
        assert!(live.try_recv().is_err());
        hub.publish_inbox(event(0)).unwrap();
        let (replay, _) = hub.subscribe_events(Some(first.seq)).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, 2);
        assert_eq!(store.inbox(true).unwrap().len(), 2);
        assert!(hub.subscribe_events(None).unwrap().0.is_empty());
        hub.lifecycle(&first.machine, "worker_online").unwrap();
        assert!(store.inbox(false).unwrap().is_empty());
        assert_eq!(live.recv().await.unwrap().seq, 2);
        assert_eq!(live.recv().await.unwrap().seq, 3);
    }
    #[tokio::test(start_paused = true)]
    async fn wait_returns_early_and_times_out_without_marking_read() {
        let hub = Hub::new(Arc::new(MemoryStore::default()));
        let h = hub.clone();
        let wait = tokio::spawn(async move { h.wait_for_events(None, Some(0)).await.unwrap() });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        hub.publish_inbox(event(0)).unwrap();
        assert_eq!(wait.await.unwrap().len(), 1);
        assert_eq!(hub.store.inbox(false).unwrap().len(), 1);
        let before = tokio::time::Instant::now();
        assert!(
            hub.wait_for_events(Some(2000), Some(1))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(before.elapsed(), std::time::Duration::from_secs(1800));
    }
    #[tokio::test]
    async fn apply_worker_envelope_twice_publishes_one_bounded_hint() {
        let store = Arc::new(MemoryStore::default());
        let hub = Hub::new(store.clone());
        let machine: MachineId = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        store
            .put_machine(navarch_store::Machine {
                id: machine.clone(),
                public_key: [0; 32],
                static_public_key: None,
                display_name: "test".into(),
                state: MachineState::Active,
                capabilities: None,
            })
            .unwrap();
        let task_id = TaskId::new();
        let card_id = CardId::new();
        store
            .put_task(navarch_store::TaskCard {
                id: card_id,
                title: "test".into(),
                project: None,
                state: "running".into(),
                steps: vec![navarch_store::Step {
                    task_id,
                    created_at: 1,
                    role: None,
                    agent: "fake".into(),
                    machine: machine.clone(),
                    session: None,
                    adapter_session_id: None,
                    cwd: None,
                    state: "running".into(),
                    result_summary: String::new(),
                }],
                log: vec![],
                applied_events: vec![],
                permissions: vec![],
            })
            .unwrap();
        let (_, mut rx) = hub.subscribe_events(None).unwrap();
        let envelope = Envelope {
            v: 1,
            id: uuid::Uuid::new_v4(),
            from: machine.clone(),
            to: machine,
            body: Body::Finished {
                task_id,
                session_id: "session".into(),
                stop_reason: "end_turn".into(),
                result: "界".repeat(300),
            },
        };
        hub.apply(&envelope).unwrap();
        hub.apply(&envelope).unwrap();
        let hint = rx.recv().await.unwrap();
        assert_eq!(hint.kind, "finished");
        assert_eq!(hint.card_id, Some(card_id));
        assert_eq!(hint.summary.chars().count(), 200);
        assert!(rx.try_recv().is_err());
        assert_eq!(store.inbox(false).unwrap().len(), 1);
        let expected = serde_json::to_value(&hint).unwrap();
        let control = hub
            .request_async(crate::control::Request::WaitForEvents {
                timeout_secs: Some(0),
                after_seq: None,
            })
            .await
            .unwrap();
        assert_eq!(control, serde_json::json!([expected]));
        assert_eq!(control[0].as_object().unwrap().len(), 7);
        assert!(store.inbox(false).unwrap()[0].envelope.is_some());
    }
}
