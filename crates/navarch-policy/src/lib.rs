//! Pure policy parsing and evaluation. Filesystem resolution belongs to the caller.
#![forbid(unsafe_code)]
use navarch_proto::{PermissionRequest, ToolKind};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Policy evaluation result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Permit the operation.
    Allow {
        /// Prefer a persistent permission choice.
        remember: bool,
    },
    /// Reject the operation.
    Deny,
    /// Ask the configured escalation handler.
    #[default]
    Escalate,
}
/// A policy file's decision spelling.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDecision {
    /// Permit once.
    Allow,
    /// Deny.
    Deny,
    /// Ask the user.
    #[default]
    Escalate,
}
impl From<RuleDecision> for Decision {
    fn from(value: RuleDecision) -> Self {
        match value {
            RuleDecision::Allow => Self::Allow { remember: false },
            RuleDecision::Deny => Self::Deny,
            RuleDecision::Escalate => Self::Escalate,
        }
    }
}
/// One ordered permission rule.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Permission category.
    pub kind: ToolKind,
    /// Require all target paths to be inside an allowed root.
    #[serde(default)]
    pub within_roots: bool,
    /// Accepted command prefixes, ending at a word boundary.
    #[serde(default)]
    pub command_prefix: Vec<String>,
    /// Result when matched.
    pub decision: RuleDecision,
}
/// Parsed worker policy. Paths must be resolved by the worker before evaluation.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Absolute, canonical allowed roots after worker loading.
    pub allowed_roots: Vec<PathBuf>,
    /// Permitted configured agent names.
    pub allowed_agents: Vec<String>,
    /// Maximum active turns.
    pub max_concurrent_turns: usize,
    /// Fallback permission decision.
    pub default: RuleDecision,
    /// Rules in evaluation order.
    #[serde(rename = "rule")]
    pub rules: Vec<Rule>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            allowed_roots: vec![],
            allowed_agents: vec![],
            max_concurrent_turns: 3,
            default: RuleDecision::Escalate,
            rules: vec![],
        }
    }
}
/// Invalid policy or rejected dispatch.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// Invalid TOML, including its field path.
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
    /// A configured field is invalid.
    #[error("{0}")]
    Invalid(String),
    /// A dispatch violated an allowlist.
    #[error("{0}")]
    Refused(String),
}
/// Expand `~` using an explicitly supplied home; no environment or filesystem I/O.
pub fn expand_home(path: &Path, home: &Path) -> PathBuf {
    if path == Path::new("~") {
        home.to_owned()
    } else if let Ok(tail) = path.strip_prefix("~/") {
        home.join(tail)
    } else {
        path.to_owned()
    }
}
fn normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}
impl Policy {
    /// Parse policy TOML without loading any files.
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        let policy: Self = toml::from_str(text)?;
        if policy.max_concurrent_turns == 0 {
            return Err(PolicyError::Invalid(
                "max_concurrent_turns must be greater than zero".into(),
            ));
        }
        if policy
            .rules
            .iter()
            .any(|r| r.command_prefix.iter().any(|p| p.trim().is_empty()))
        {
            return Err(PolicyError::Invalid(
                "rule.command_prefix must not contain empty prefixes".into(),
            ));
        }
        Ok(policy)
    }
    /// Check an absolute canonical cwd before starting an adapter.
    /// Callers must resolve symlinks on both cwd and allowed roots first.
    pub fn check_dispatch(&self, cwd: &Path, agent: &str) -> Result<(), PolicyError> {
        if !self.allowed_agents.iter().any(|a| a == agent) {
            return Err(PolicyError::Refused(format!(
                "allowed_agents refused agent {agent:?}"
            )));
        }
        if !self.contains(cwd) {
            return Err(PolicyError::Refused(format!(
                "allowed_roots refused cwd {}",
                cwd.display()
            )));
        }
        Ok(())
    }
    /// Whether a canonical absolute path lies within an allowed root.
    pub fn contains(&self, path: &Path) -> bool {
        normalized_absolute(path)
            && self
                .allowed_roots
                .iter()
                .any(|root| normalized_absolute(root) && path.starts_with(root))
    }
    /// Evaluate ordered rules using canonical paths. Missing/unresolved paths cannot
    /// satisfy `within_roots`; unknown tool categories remain `other`.
    pub fn decide(&self, request: &PermissionRequest) -> Decision {
        for rule in &self.rules {
            if rule.kind != request.kind {
                continue;
            }
            if rule.within_roots
                && (request.paths.is_empty() || !request.paths.iter().all(|p| self.contains(p)))
            {
                continue;
            }
            if !rule.command_prefix.is_empty()
                && !request.command.as_ref().is_some_and(|command| {
                    rule.command_prefix.iter().any(|prefix| {
                        command.strip_prefix(prefix).is_some_and(|tail| {
                            tail.is_empty() || tail.starts_with(char::is_whitespace)
                        })
                    })
                })
            {
                continue;
            }
            return rule.decision.into();
        }
        self.default.into()
    }
}
