//! CLI regressions never launch a real adapter.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
#[test]
fn outside_allowed_roots_prints_error_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(
        temp.path().join("worker.toml"),
        "[[agents]]\nname='fake'\ncommand='/must-not-be-run'\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("worker-policy.toml"),
        format!(
            "allowed_agents=['fake']\nallowed_roots=[{:?}]\n",
            root.to_string_lossy()
        ),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", temp.path())
        .args(["worker", "run", "--agent", "fake", "--cwd"])
        .arg(&outside)
        .arg("hi")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let error = String::from_utf8(result.stderr).unwrap();
    assert_eq!(
        error.matches("allowed_roots refused cwd").count(),
        1,
        "{error}"
    );
    assert!(
        error.contains(
            &outside
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        )
    );
    assert!(!temp.path().join("worker/sessions.json").exists());
}
#[test]
fn hub_commands_parse_and_missing_daemon_is_clear() {
    for command in ["serve", "mcp", "status", "tasks", "init"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
            .args(["hub", command, "--help"])
            .output()
            .unwrap();
        assert!(result.status.success());
    }
    let temp = tempfile::tempdir().unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", temp.path())
        .args(["hub", "mcp"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("start `navarch hub serve`")
    );
}
