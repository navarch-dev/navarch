//! Worker configuration and filesystem resolution at the policy boundary.
use crate::{WorkerError, error::file};
use navarch_policy::{Policy, expand_home};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};
/// One adapter configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Name used by dispatch.
    pub name: String,
    /// Adapter executable; arguments never pass through a shell.
    pub command: PathBuf,
    /// Literal adapter arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Permission mode override.
    pub mode: Option<String>,
}
impl AgentConfig {
    /// Explicit asking mode for supported built-in adapters.
    pub fn mode(&self) -> Option<String> {
        self.mode.clone().or_else(|| match self.name.as_str() {
            "claude" => Some("default".into()),
            "codex" => Some("workspace-write".into()),
            _ => None,
        })
    }
    /// Locate an executable without starting it.
    pub fn executable(&self) -> Option<PathBuf> {
        if self.command.components().count() > 1 {
            return executable(&self.command).then(|| self.command.clone());
        }
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join(&self.command))
                .find(|p| executable(p))
        })
    }
}
fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}
/// Standalone worker settings. Hub link settings belong to M2.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Display name for capability reports.
    pub display_name: String,
    /// Configured adapters.
    pub agents: Vec<AgentConfig>,
}
impl Config {
    /// Parse worker TOML with field-aware errors.
    pub fn parse(text: &str, path: &Path) -> Result<Self, WorkerError> {
        let config: Self = toml::from_str(text).map_err(|e| file(path, e))?;
        let mut names = std::collections::HashSet::new();
        for (i, a) in config.agents.iter().enumerate() {
            if a.name.is_empty() || !names.insert(&a.name) {
                return Err(file(
                    path,
                    format!("agents[{i}].name must be nonempty and unique"),
                ));
            }
            if a.command.as_os_str().is_empty() {
                return Err(file(path, format!("agents[{i}].command must not be empty")));
            }
        }
        Ok(config)
    }
    /// Read worker.toml; a missing configuration is inert.
    pub fn load(home: &Path) -> Result<Self, WorkerError> {
        let path = home.join("worker.toml");
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, &path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(file(path, e)),
        }
    }
}
/// Load policy and resolve configured roots before passing them to pure logic.
pub fn load_policy(home: &Path, user_home: &Path) -> Result<Policy, WorkerError> {
    let path = home.join("worker-policy.toml");
    let mut policy = match fs::read_to_string(&path) {
        Ok(text) => Policy::parse(&text).map_err(|e| file(&path, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Policy::default(),
        Err(e) => return Err(file(&path, e)),
    };
    for (i, root) in policy.allowed_roots.iter_mut().enumerate() {
        *root = fs::canonicalize(expand_home(root, user_home))
            .map_err(|e| file(&path, format!("allowed_roots[{i}] {}: {e}", root.display())))?;
        if !root.is_dir() {
            return Err(file(
                &path,
                format!("allowed_roots[{i}] must be a directory"),
            ));
        }
    }
    Ok(policy)
}
/// Resolve an existing cwd, including symlinks, before dispatch validation.
pub fn canonical_cwd(cwd: &Path) -> Result<PathBuf, WorkerError> {
    let cwd = fs::canonicalize(cwd).map_err(|e| {
        WorkerError::Invalid(format!("allowed_roots refused cwd {}: {e}", cwd.display()))
    })?;
    if !cwd.is_dir() {
        return Err(WorkerError::Invalid(format!(
            "cwd {} is not a directory",
            cwd.display()
        )));
    }
    Ok(cwd)
}
/// Resolve a permission target, including symlinks in existing parent directories.
/// Nonexistent leaf files can be edited; unresolved parents fail closed.
pub(crate) fn permission_path(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    if let Ok(path) = fs::canonicalize(&path) {
        return Some(path);
    }
    // Do not normalize `..` lexically: its meaning depends on preceding symlinks.
    let leaf = path.file_name()?;
    let parent = fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(leaf))
}
/// Write commented examples using create_new; existing files are preserved.
pub fn init(home: &Path) -> Result<(), WorkerError> {
    fs::create_dir_all(home).map_err(|e| file(home, e))?;
    for (name, text) in [
        ("worker.toml", include_str!("../templates/worker.toml")),
        (
            "worker-policy.toml",
            include_str!("../templates/worker-policy.toml"),
        ),
    ] {
        use std::io::Write;
        let path = home.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => f.write_all(text.as_bytes()).map_err(|e| file(&path, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(file(path, e)),
        }
    }
    Ok(())
}
