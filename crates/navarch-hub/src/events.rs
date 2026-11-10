use crate::{Hub, HubError};
use navarch_proto::{Body, Envelope};
use navarch_store::{InboxEvent, MachineState};
impl Hub {
    /// Apply a worker envelope durably before acknowledging it. Task logs make
    /// repeated events idempotent even after the hub process loses its dedup cache.
    pub fn apply(&self, envelope: &Envelope) -> Result<(), HubError> {
        let _state = self.state()?;
        envelope
            .check_version()
            .map_err(|e| HubError::Invalid(e.to_string()))?;
        let mut machine = self
            .store
            .machines()?
            .into_iter()
            .find(|m| m.id == envelope.from)
            .ok_or_else(|| HubError::Invalid("event from unknown machine".into()))?;
        if machine.state != MachineState::Active {
            return Err(HubError::Invalid("event from inactive machine".into()));
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
        let mut card = self
            .store
            .task(task_id)?
            .ok_or_else(|| HubError::Invalid(format!("event for unknown task {task_id}")))?;
        if card.log.iter().any(|e| e.id == envelope.id) {
            return Ok(());
        }
        let step = card
            .steps
            .last_mut()
            .ok_or_else(|| HubError::Invalid("task has no step".into()))?;
        if step.machine != envelope.from {
            return Err(HubError::Invalid("worker does not own task".into()));
        }
        // Reliable delivery is unordered. Late Accepted/Progress/PermissionAsk
        // events must be logged without reverting a terminal card.
        let terminal = matches!(
            card.state.as_str(),
            "finished" | "failed" | "rejected" | "cancelled"
        );
        match &envelope.body {
            Body::Accepted { session_id, .. } => {
                step.session = Some(session_id.clone());
                if !terminal {
                    card.state = "running".into();
                }
            }
            Body::Progress { summary, .. } => {
                if !terminal {
                    step.result_summary = summary.clone();
                }
            }
            Body::PermissionAsk { .. } => {
                if !terminal {
                    card.state = "permission".into();
                }
            }
            Body::Finished {
                session_id,
                stop_reason,
                result,
                ..
            } => {
                step.session = Some(session_id.clone());
                card.state = if stop_reason == "cancelled" {
                    "cancelled"
                } else {
                    "finished"
                }
                .into();
                step.result_summary = result.clone();
            }
            Body::Failed { error, .. } => {
                card.state = "failed".into();
                step.result_summary = error.clone();
            }
            Body::Rejected { reason, .. } => {
                card.state = "rejected".into();
                step.result_summary = reason.clone();
            }
            _ => {}
        }
        step.state = card.state.clone();
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
        card.log.push(envelope.clone());
        self.store.put_task(card)?;
        Ok(())
    }
}
