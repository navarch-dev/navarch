//! Pure source bounds for variable-size worker messages.
use crate::*;

/// Maximum encoded envelope size.
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
/// Budget for session enumeration and capability reports, including the envelope.
pub const LIST_ENVELOPE_BYTES: usize = 256 * 1024;
/// Maximum final result in UTF-8 bytes.
pub const RESULT_BYTES: usize = 64 * 1024;

/// Effective session query limit.
pub fn session_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(50).min(200)
}

/// Cut UTF-8 text in bytes without splitting a character.
pub fn truncate_bytes(text: &mut String, limit: usize) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

/// Exact encoded size with fixed-width machine IDs, UUIDs and protocol version 1.
/// This also works before the peer has connected; all machine IDs are 26 ASCII bytes.
pub fn encoded_envelope_len(body: &Body) -> Result<usize, serde_json::Error> {
    // These are the exact bytes surrounding a serialized body in a v1 envelope.
    const WRAPPER: &str = concat!(
        "{\"v\":1,\"id\":\"00000000-0000-0000-0000-000000000000\",",
        "\"from\":\"aaaaaaaaaaaaaaaaaaaaaaaaaa\",\"to\":\"aaaaaaaaaaaaaaaaaaaaaaaaaa\",\"body\":}"
    );
    serde_json::to_vec(body).map(|bytes| bytes.len() + WRAPPER.len())
}

impl Body {
    /// Turn ID for worker-originated events only, for delivery failure recovery.
    pub fn worker_task_id(&self) -> Option<TaskId> {
        match self {
            Self::Accepted { task_id, .. }
            | Self::Rejected { task_id, .. }
            | Self::Progress { task_id, .. }
            | Self::PermissionAsk { task_id, .. }
            | Self::Finished { task_id, .. }
            | Self::Failed { task_id, .. } => Some(*task_id),
            _ => None,
        }
    }

    /// Apply worker source bounds. Opaque IDs and paths are never rewritten.
    /// Oversized opaque turn metadata becomes Failed; oversized list entries are omitted.
    pub fn bound_worker_payload(&mut self) {
        match self {
            Self::Accepted { .. } => {}
            Self::Finished {
                result,
                stop_reason,
                ..
            } => {
                truncate_bytes(result, RESULT_BYTES);
                truncate_bytes(stop_reason, 1024);
            }
            Self::Progress { summary, .. } => *summary = short_summary(summary),
            Self::Rejected { reason, .. } => truncate_bytes(reason, 16 * 1024),
            Self::Failed { error, .. } => truncate_bytes(error, 16 * 1024),
            Self::PermissionAsk {
                tool,
                detail,
                options,
                ..
            } => {
                *tool = tool.chars().take(200).collect();
                truncate_bytes(detail, 16 * 1024);
                // IDs must remain exact for permission answers. Omit unrepresentable choices.
                options.retain(|o| o.id.len() <= 1024);
                options.truncate(32);
                for option in options {
                    option.name = option.name.chars().take(200).collect();
                }
            }
            Self::SessionList {
                request_id,
                sessions,
                truncated,
            } => {
                *truncated |= sessions.len() > 200;
                sessions.truncate(200);
                for session in sessions.iter_mut() {
                    if let Some(title) = &mut session.title {
                        *title = title.chars().take(120).collect();
                    }
                }
                // Measure each entry once; repeatedly encoding a shrinking list
                // would make a large opaque ID/path quadratic in the entry count.
                let empty = Self::SessionList {
                    request_id: *request_id,
                    sessions: vec![],
                    truncated: *truncated,
                };
                let mut used = encoded_envelope_len(&empty).unwrap_or(LIST_ENVELOPE_BYTES);
                let mut keep = 0;
                for session in sessions.iter() {
                    let Ok(encoded) = serde_json::to_vec(session) else {
                        break;
                    };
                    let next = used
                        .saturating_add(encoded.len())
                        .saturating_add(usize::from(keep > 0));
                    if next > LIST_ENVELOPE_BYTES {
                        break;
                    }
                    used = next;
                    keep += 1;
                }
                *truncated |= keep < sessions.len();
                sessions.truncate(keep);
            }
            Self::ProbeReport { capabilities: c } => {
                truncate_bytes(&mut c.os, 1024);
                truncate_bytes(&mut c.arch, 1024);
                c.repositories.truncate(200);
                c.agents.retain(|agent| agent.name.len() <= 1024);
                for agent in &mut c.agents {
                    for value in [&mut agent.version, &mut agent.error].into_iter().flatten() {
                        truncate_bytes(value, 4096);
                    }
                    agent.models.retain(|model| model.len() <= 1024);
                    agent.models.truncate(100);
                }
            }
            _ => return,
        }
        loop {
            let budget = if matches!(self, Self::SessionList { .. } | Self::ProbeReport { .. }) {
                LIST_ENVELOPE_BYTES
            } else {
                MAX_ENVELOPE_BYTES
            };
            if encoded_envelope_len(self).is_ok_and(|size| size <= budget) {
                break;
            }
            match self {
                Self::SessionList {
                    sessions,
                    truncated,
                    ..
                } => {
                    *truncated = true;
                    if sessions.pop().is_none() {
                        break;
                    }
                }
                Self::ProbeReport { capabilities } => {
                    if capabilities.repositories.pop().is_some() {
                        continue;
                    }
                    if let Some(agent) = capabilities
                        .agents
                        .iter_mut()
                        .rev()
                        .find(|agent| !agent.models.is_empty())
                    {
                        agent.models.pop();
                    } else if capabilities.agents.pop().is_none() {
                        break;
                    }
                }
                _ => {
                    if let Some(task_id) = self.worker_task_id() {
                        *self = Self::Failed {
                            task_id,
                            error: "Worker event exceeded the 1 MiB envelope limit".into(),
                        };
                    }
                    break;
                }
            }
        }
    }
}

/// Normalize adapter RFC3339 or registry Unix-millisecond timestamps for ordering.
/// Unknown timestamps sort after known timestamps.
pub fn session_timestamp(value: Option<&str>) -> Option<i64> {
    let value = value?;
    value.parse().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| time.timestamp_millis())
    })
}
