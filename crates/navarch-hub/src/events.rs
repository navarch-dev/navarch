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
                let _ = reply.send(sessions.clone());
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
            self.store.append_inbox(InboxEvent {
                envelope: envelope.clone(),
            })?;
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
