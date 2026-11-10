use crate::Worker;
use navarch_acp::NewSession;
use navarch_proto::{AgentCapability, Capabilities};
use std::path::Path;
impl Worker {
    /// Inspect configured executables and initialize present adapters using a throwaway
    /// session to discover their version and model config options.
    pub async fn probe(&self) -> Capabilities {
        let mut report = Capabilities {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            running_turns: self.running_turns(),
            ..Capabilities::default()
        };
        for agent in &self.config().agents {
            let mut capability = AgentCapability {
                name: agent.name.clone(),
                present: agent.executable().is_some(),
                ..AgentCapability::default()
            };
            if capability.present {
                let result = async {
                    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
                    let driver = self.drivers().get(&agent.name).ok_or("driver missing")?;
                    driver
                        .new_session(NewSession {
                            cwd: temp.path().to_owned(),
                            mode: None,
                            model: None,
                            mcp_servers: vec![],
                        })
                        .await
                        .map_err(|e| e.to_string())
                };
                match tokio::time::timeout(std::time::Duration::from_secs(30), result).await {
                    Ok(Ok(session)) => {
                        capability.version = session.agent_version;
                        capability.models = session.models.into_iter().take(100).collect();
                    }
                    Ok(Err(e)) => capability.error = Some(e),
                    Err(_) => {
                        capability.error = Some("adapter probe timed out after 30 seconds".into())
                    }
                }
            } else {
                capability.error = Some(format!(
                    "executable {} not found; see navarch worker doctor",
                    agent.command.display()
                ));
            }
            report.agents.push(capability);
        }
        for root in &self.policy().allowed_roots {
            repositories(root, root, 0, &mut report.repositories);
        }
        report.repositories.sort();
        report.repositories.dedup();
        let mut body = navarch_proto::Body::ProbeReport {
            capabilities: report,
        };
        body.bound_worker_payload();
        if let navarch_proto::Body::ProbeReport { capabilities } = body {
            capabilities
        } else {
            unreachable!()
        }
    }
    /// Diagnose executable availability without launching adapters or installing anything.
    pub fn doctor(&self) -> Vec<String> {
        if self.config().agents.is_empty() {
            return vec!["No agents configured in worker.toml; run navarch worker init.".into()];
        }
        self.config().agents.iter().map(|a|{
            let install=match a.name.as_str(){"claude"=>"Use command = \"npx\", args = [\"-y\", \"@agentclientprotocol/claude-agent-acp\"]; install Node.js/npm and authenticate Claude Code.","codex"=>"Use command = \"npx\", args = [\"-y\", \"@agentclientprotocol/codex-acp\"]; install Node.js/npm and authenticate Codex.",_=>"Install the configured adapter executable and add it to PATH."};
            format!("{}: {}. {install} Executable presence does not verify an npx package or authentication.",a.name,if a.executable().is_some(){"executable present"}else{"executable missing"})
        }).collect()
    }
}
fn repositories(path: &Path, root: &Path, depth: usize, result: &mut Vec<std::path::PathBuf>) {
    if result.len() >= 200 {
        return;
    }
    if path.join(".git").exists() {
        result.push(path.to_owned());
    }
    if depth == 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        if result.len() >= 200 {
            break;
        }
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if let Ok(kind) = entry.file_type()
            && kind.is_dir()
            && let Ok(canonical) = entry.path().canonicalize()
            && canonical.starts_with(root)
        {
            repositories(&canonical, root, depth + 1, result);
        }
    }
}
