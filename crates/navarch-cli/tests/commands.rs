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

#[test]
fn long_navarch_home_reports_socket_path_limit_and_remedy_once() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("x".repeat(180));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", &home)
        .args(["hub", "serve"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains(&home.join("hub-worker.sock").display().to_string()),
        "{error}"
    );
    assert!(
        error.contains("limit") && error.contains("bytes"),
        "{error}"
    );
    assert_eq!(error.matches("too long").count(), 1, "{error}");
    assert_eq!(
        error
            .matches("set NAVARCH_HOME to a shorter directory")
            .count(),
        1,
        "{error}"
    );
    assert!(!error.contains("SUN_LEN"));
}

#[tokio::test]
async fn relay_logs_listening_at_info_by_default() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut relay = tokio::process::Command::new(env!("CARGO_BIN_EXE_navarch"))
        .args(["relay", "serve", "--listen", "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(relay.stderr.take().unwrap()).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(line.contains("INFO"), "{line}");
    assert!(line.contains("listening"), "{line}");
    assert!(line.contains("127.0.0.1:"), "{line}");
    relay.kill().await.unwrap();
    relay.wait().await.unwrap();
}
