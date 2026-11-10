use crate::HubError;
use std::{fs, io::Write, path::Path};
/// Write commented project, route and role examples without overwriting any file.
pub fn init(home: &Path) -> Result<(), HubError> {
    let root = home.join("hub");
    fs::create_dir_all(root.join("roles"))?;
    for (name, text) in [
        (
            "projects.toml",
            "# Projects are optional; use names or aliases in dispatch.\n# [projects.navarch]\n# aliases = [\"nav\"]\n# repositories = [\"navarch\"]\n[projects]\n",
        ),
        (
            "routes.toml",
            "# Task-kind to role hints for the orchestrator; the hub never chooses.\n# [routes]\n# implementation = \"developer\"\n[routes]\n",
        ),
        (
            "roles/developer.md",
            "+++\n# Change this to an agent name reported by your worker.\nagent = \"codex\"\n# model = \"your-model\"\n# mode = \"workspace-write\"\n\n+++\nImplement the requested change and report relevant checks.\n",
        ),
    ] {
        let path = root.join(name);
        let parent = path
            .parent()
            .ok_or_else(|| HubError::Invalid("example has no parent directory".into()))?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        match file.persist_noclobber(&path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.error.into()),
        }
    }
    Ok(())
}
