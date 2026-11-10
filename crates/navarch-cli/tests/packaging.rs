//! Packaging regressions do not install host services.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use std::process::Command;

#[test]
fn version_contains_crate_version_and_best_effort_commit() {
    let result = Command::new(env!("CARGO_BIN_EXE_navarch"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(
        text.starts_with(concat!("navarch ", env!("CARGO_PKG_VERSION"), " (")),
        "{text}"
    );
    let commit = text.trim().split_once('(').unwrap().1.trim_end_matches(')');
    if commit != "unknown" {
        let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
        assert!(
            hash.len() >= 7 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            "{commit}"
        );
    }
}

#[test]
fn service_dry_run_prints_configuration_without_creating_state() {
    let temp = tempfile::tempdir().unwrap();
    let navarch_home = temp.path().join("state-that-must-not-be-created");
    let result = Command::new(env!("CARGO_BIN_EXE_navarch"))
        .env("NAVARCH_HOME", &navarch_home)
        .args(["service", "install", "hub", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("NAVARCH_HOME"));
    assert!(text.contains(navarch_home.to_str().unwrap()));
    #[cfg(target_os = "macos")]
    {
        assert!(text.contains("<plist version=\"1.0\">"));
        assert!(text.contains("launchctl 'bootstrap'"));
        let xml_start = text.find("<?xml").unwrap();
        let xml_end = text.find("</plist>").unwrap() + "</plist>".len();
        plist::Value::from_reader_xml(&text.as_bytes()[xml_start..xml_end]).unwrap();
    }
    #[cfg(target_os = "linux")]
    {
        assert!(text.contains("systemctl '--user' 'enable' '--now' 'navarch-hub.service'"));
        let start = text.find("[Unit]").unwrap();
        let end = text.find("# Write").unwrap();
        configparser::ini::Ini::new_cs()
            .read(text[start..end].to_owned())
            .unwrap();
    }
    assert!(!navarch_home.exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn installer_selects_platform_verifies_checksum_and_preserves_existing_binary_on_mismatch() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (index, os, arch, target) in [
        (0, "Darwin", "arm64", "aarch64-apple-darwin"),
        (1, "Darwin", "x86_64", "x86_64-apple-darwin"),
        (2, "Linux", "aarch64", "aarch64-unknown-linux-gnu"),
        (3, "Linux", "x86_64", "x86_64-unknown-linux-gnu"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();
        let mocks = path.join("mocks");
        fs::create_dir(&mocks).unwrap();
        fs::write(path.join("navarch"), "fixture binary\n").unwrap();
        let archive = path.join("fixture.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(path)
                .arg("navarch")
                .status()
                .unwrap()
                .success()
        );
        let sum = if cfg!(target_os = "macos") {
            Command::new("shasum")
                .args(["-a", "256"])
                .arg(&archive)
                .output()
                .unwrap()
        } else {
            Command::new("sha256sum").arg(&archive).output().unwrap()
        };
        assert!(sum.status.success());
        let digest = String::from_utf8(sum.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let filename = format!("navarch-0.1.0-{target}.tar.gz");
        fs::write(path.join("SHA256SUMS"), format!("{digest}  {filename}\n")).unwrap();
        fs::write(mocks.join("uname"), "#!/bin/sh\ncase $1 in -s) printf '%s\\n' \"$FAKE_OS\";; -m) printf '%s\\n' \"$FAKE_ARCH\";; *) exit 1;; esac\n").unwrap();
        fs::write(
            mocks.join("curl"),
            r#"#!/bin/sh
set -eu
out=
url=
while [ "$#" -gt 0 ]; do
    case $1 in
        -o) shift; out=$1 ;;
        https://*) url=$1 ;;
    esac
    shift
done
printf '%s\n' "$url" >> "$FIXTURES/requests"
case $url in
    */releases/latest) printf '%s\n' 'https://github.com/navarch-dev/navarch/releases/tag/v0.1.0' ;;
    */SHA256SUMS) cp "$FIXTURES/SHA256SUMS" "$out" ;;
    */*.tar.gz) cp "$FIXTURES/fixture.tar.gz" "$out" ;;
    *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        for mock in ["uname", "curl"] {
            fs::set_permissions(mocks.join(mock), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let install_dir = path.join("install ' with spaces");
        let mut command = Command::new("sh");
        command
            .arg(root.join("install.sh"))
            .env(
                "PATH",
                format!("{}:{}", mocks.display(), std::env::var("PATH").unwrap()),
            )
            .env("HOME", path)
            .env("FIXTURES", path)
            .env("FAKE_OS", os)
            .env("FAKE_ARCH", arch)
            .env("NAVARCH_INSTALL_DIR", &install_dir);
        if index == 0 {
            command.env_remove("NAVARCH_VERSION");
        } else {
            command.env("NAVARCH_VERSION", "v0.1.0");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            fs::read(install_dir.join("navarch")).unwrap(),
            b"fixture binary\n"
        );
        assert_eq!(
            fs::metadata(install_dir.join("navarch"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let requests = fs::read_to_string(path.join("requests")).unwrap();
        assert!(
            requests.contains(&format!("/releases/download/v0.1.0/{filename}")),
            "{requests}"
        );
        assert_eq!(requests.contains("/releases/latest"), index == 0);
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains("export PATH="), "{output}");
        let export = output.lines().find(|s| s.contains("export PATH=")).unwrap();
        assert!(
            Command::new("sh")
                .args(["-n", "-c", export])
                .status()
                .unwrap()
                .success()
        );
        // A bad checksum must leave an already installed binary exactly intact.
        fs::write(
            path.join("SHA256SUMS"),
            format!("{}  {filename}\n", "0".repeat(64)),
        )
        .unwrap();
        fs::write(install_dir.join("navarch"), "existing binary").unwrap();
        let result = command.output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("checksum mismatch"));
        assert_eq!(
            fs::read(install_dir.join("navarch")).unwrap(),
            b"existing binary"
        );
    }
}
