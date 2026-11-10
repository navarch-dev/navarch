//! Pure policy contracts, with filesystem resolution performed by the caller.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_policy::*;
use navarch_proto::*;
fn request(kind: ToolKind) -> PermissionRequest {
    PermissionRequest {
        session_id: "s".into(),
        tool: "tool".into(),
        kind,
        detail: String::new(),
        paths: vec![],
        command: None,
        options: vec![],
    }
}
#[test]
fn first_match_wins_and_default() {
    let policy=Policy::parse("default='deny'\n[[rule]]\nkind='read'\ndecision='allow'\n[[rule]]\nkind='read'\ndecision='deny'").unwrap();
    assert_eq!(
        policy.decide(&request(ToolKind::Read)),
        Decision::Allow { remember: false }
    );
    assert_eq!(policy.decide(&request(ToolKind::Edit)), Decision::Deny);
}
#[test]
fn empty_policy_is_inert() {
    let policy = Policy::parse("").unwrap();
    assert_eq!(policy.decide(&request(ToolKind::Edit)), Decision::Escalate);
    assert!(
        policy
            .check_dispatch(std::path::Path::new("/tmp"), "fake")
            .is_err()
    );
}
#[test]
fn home_expansion() {
    assert_eq!(
        expand_home(
            std::path::Path::new("~/projects"),
            std::path::Path::new("/home/user")
        ),
        std::path::PathBuf::from("/home/user/projects")
    );
    assert_eq!(
        expand_home(
            std::path::Path::new("~"),
            std::path::Path::new("/home/user")
        ),
        std::path::PathBuf::from("/home/user")
    );
}
#[test]
fn traversal_and_symlink_escape() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let mut policy=Policy::parse("allowed_agents=['fake']\ndefault='deny'\n[[rule]]\nkind='edit'\nwithin_roots=true\ndecision='allow'").unwrap();
    policy.allowed_roots = vec![root.canonicalize().unwrap()];
    assert!(
        policy
            .check_dispatch(&root.canonicalize().unwrap(), "fake")
            .is_ok()
    );
    assert!(
        policy
            .check_dispatch(&root.join("../outside"), "fake")
            .is_err()
    );
    let mut req = request(ToolKind::Edit);
    req.paths = vec![root.join("../outside").canonicalize().unwrap()];
    assert_eq!(policy.decide(&req), Decision::Deny);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let escape = root.join("link").canonicalize().unwrap();
        assert!(policy.check_dispatch(&escape, "fake").is_err());
        req.paths = vec![escape];
        assert_eq!(policy.decide(&req), Decision::Deny);
    }
    req.paths = vec![];
    assert_eq!(policy.decide(&req), Decision::Deny);
}
#[test]
fn command_prefix_word_boundaries() {
    let policy = Policy::parse(
        "default='deny'\n[[rule]]\nkind='execute'\ncommand_prefix=['cargo test']\ndecision='allow'",
    )
    .unwrap();
    let mut req = request(ToolKind::Execute);
    for command in ["cargo test", "cargo test --workspace", "cargo test\tfoo"] {
        req.command = Some(command.into());
        assert_eq!(policy.decide(&req), Decision::Allow { remember: false });
    }
    for command in ["cargo testing", "cargo test;rm", "xcargo test"] {
        req.command = Some(command.into());
        assert_eq!(policy.decide(&req), Decision::Deny);
    }
}
#[test]
fn invalid_fields_are_clear() {
    for text in [
        "max_concurrent_turns=0",
        "max_concurrent_turns='bad'",
        "unknown=true",
        "[[rule]]\nkind='execute'\ncommand_prefix=['']\ndecision='allow'",
    ] {
        assert!(Policy::parse(text).is_err());
    }
}
