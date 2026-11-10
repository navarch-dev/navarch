//! Embed a best-effort source revision without requiring Git at build time.
#![forbid(unsafe_code)]

use std::{path::PathBuf, process::Command};

fn git(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Watch source inputs and Git metadata, including worktree Git directories.
    // No watch covers target/, so builds never invalidate themselves.
    for path in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
    }
    if let Some(reference) = git(&root, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&root, &["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    for path in [
        "crates",
        "docs",
        ".github",
        "Cargo.toml",
        "Cargo.lock",
        "README.md",
        "README.zh-CN.md",
        "install.sh",
        ".gitignore",
        "LICENSE",
        "NOTICE",
        "rustfmt.toml",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let commit = match (
        git(&root, &["rev-parse", "--short", "HEAD"]),
        git(&root, &["status", "--porcelain"]),
    ) {
        (Some(hash), Some(status)) if !hash.is_empty() => {
            format!("{hash}{}", if status.is_empty() { "" } else { "-dirty" })
        }
        _ => "unknown".into(),
    };
    println!(
        "cargo:rustc-env=NAVARCH_BUILD_VERSION={} ({commit})",
        env!("CARGO_PKG_VERSION")
    );
}
