#![allow(clippy::unwrap_used)]
use super::*;
use clap::Parser;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: HubCommand,
}
fn parse(args: &[&str]) -> bool {
    Cli::try_parse_from(std::iter::once("hub").chain(args.iter().copied())).is_ok()
}
#[test]
fn argument_sources_exclusivity_bounds_and_wait_flags() {
    let base = [
        "dispatch",
        "--machine",
        "aaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--cwd",
        "/repo",
    ];
    let check = |tail: &[&str]| {
        let mut args = base.to_vec();
        args.extend(tail);
        parse(&args)
    };
    assert!(check(&[
        "--agent",
        "fake",
        "--wait",
        "--timeout",
        "0",
        "hello"
    ]));
    assert!(check(&["--role", "developer", "--prompt-file", "/prompt"]));
    assert!(check(&["--agent", "fake", "-"]));
    assert!(!check(&["hello"]));
    assert!(!check(&["--agent", "fake", "--role", "developer", "hello"]));
    assert!(!check(&["--agent", "fake", "--timeout", "1", "hello"]));
    assert!(!check(&[
        "--agent",
        "fake",
        "--prompt-file",
        "/prompt",
        "hello"
    ]));
    assert!(!check(&["--agent", "fake", ""]));
    assert!(check(&[
        "--agent",
        "fake",
        "--title",
        &"界".repeat(200),
        "hello"
    ]));
    assert!(!check(&[
        "--agent",
        "fake",
        "--title",
        &"界".repeat(201),
        "hello"
    ]));
    assert!(check(&["--agent", "fake", &"x".repeat(256 * 1024)]));
    assert!(!check(&["--agent", "fake", &"x".repeat(256 * 1024 + 1)]));
    assert!(parse(&[
        "wait",
        "--timeout",
        "0",
        "--after-seq",
        "42",
        "--json"
    ]));
    assert!(!parse(&[
        "send",
        "--machine",
        "aaaaaaaaaaaaaaaaaaaaaaaaaa",
        "hello"
    ]));
    assert!(parse(&[
        "send",
        "--machine",
        "aaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--session",
        "session",
        "hello"
    ]));
    assert!(!parse(&[
        "permission",
        "request",
        "--cancel",
        "--choose",
        "yes"
    ]));
    assert!(!parse(&["permission", "request"]));
    assert!(!parse(&["mcp"]));
}
