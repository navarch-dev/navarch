//! Pure per-user service files and thin launchctl/systemctl command wiring.
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use std::{
    fmt::Write as _,
    io::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
};

/// Daemon managed by the user's service manager.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Role {
    /// The hub daemon.
    Hub,
    /// The worker daemon.
    Worker,
    /// The opaque relay server.
    Relay,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Worker => "worker",
            Self::Relay => "relay",
        }
    }
}
/// Arguments shared by service operations.
#[derive(Args)]
pub struct ServiceArgs {
    /// Daemon to manage.
    pub role: Role,
    /// Relay listening address; required when installing a relay service.
    #[arg(long)]
    pub listen: Option<SocketAddr>,
    /// Print the file and commands without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}
/// Per-user service operations.
#[derive(Subcommand)]
pub enum ServiceCommand {
    /// Write and load the service, restarting it on failure.
    Install(ServiceArgs),
    /// Stop the service and remove its service file, retaining Navarch state.
    Uninstall(ServiceArgs),
    /// Show service-manager status.
    Status(ServiceArgs),
}
/// Supported service-manager file format.
#[derive(Clone, Copy, Debug)]
pub enum Platform {
    /// A launchd agent in the logged-in user's GUI domain.
    MacOs,
    /// A systemd user unit.
    Linux,
}
/// Explicit inputs to pure service generation; no environment is read here.
pub struct ServiceSpec<'a> {
    /// Target service manager, independent of the build host.
    pub platform: Platform,
    /// Daemon to start.
    pub role: Role,
    /// Absolute binary path.
    pub executable: &'a Path,
    /// Absolute user home directory.
    pub user_home: &'a Path,
    /// Optional absolute NAVARCH_HOME override.
    pub navarch_home: Option<&'a Path>,
    /// Explicit listening address for the relay only.
    pub listen: Option<SocketAddr>,
}
/// Generated service file, ready for review or installation.
#[derive(Debug)]
pub struct ServiceFile {
    /// Absolute service file path.
    pub path: PathBuf,
    /// UTF-8 plist or unit contents.
    pub content: String,
}

fn path_text(path: &Path) -> Result<&str> {
    if !path.is_absolute() {
        bail!("service paths must be absolute: {}", path.display());
    }
    let value = path.to_str().context("service paths must be UTF-8")?;
    if value.chars().any(char::is_control) {
        bail!("service paths must not contain control characters");
    }
    Ok(value)
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
// systemd.syntax quoting and specifiers: https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html
// ExecStart uses the ':' prefix to disable environment expansion, so '$' is
// literal in both the executable path and argv. systemd stores the former before
// argv expansion: https://github.com/systemd/systemd/blob/main/src/core/load-fragment.c
fn unit_quote(value: &str) -> String {
    let value = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{value}\"")
}

/// Generate a service file without reading or writing files or invoking commands.
pub fn generate(spec: &ServiceSpec<'_>) -> Result<ServiceFile> {
    let executable = path_text(spec.executable)?;
    path_text(spec.user_home)?;
    let navarch_home = spec.navarch_home.map(path_text).transpose()?;
    let name = spec.role.name();
    let mut args = vec![executable.to_owned(), name.into(), "serve".into()];
    match (spec.role, spec.listen) {
        (Role::Relay, Some(listen)) => args.extend(["--listen".into(), listen.to_string()]),
        (Role::Relay, None) => bail!("installing a relay service requires --listen <addr>"),
        (_, Some(_)) => bail!("--listen is only valid for a relay service"),
        (_, None) => {}
    }
    match spec.platform {
        Platform::MacOs => {
            let path = spec
                .user_home
                .join(format!("Library/LaunchAgents/dev.navarch.{name}.plist"));
            let log = spec.user_home.join(format!(".navarch/logs/{name}.log"));
            let mut content = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
            );
            writeln!(
                content,
                "  <key>Label</key><string>dev.navarch.{name}</string>\n  <key>ProgramArguments</key><array>"
            )?;
            for arg in args {
                writeln!(content, "    <string>{}</string>", xml(&arg))?;
            }
            content.push_str("  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n  <key>ThrottleInterval</key><integer>5</integer>\n");
            if let Some(home) = navarch_home {
                writeln!(
                    content,
                    "  <key>EnvironmentVariables</key><dict><key>NAVARCH_HOME</key><string>{}</string></dict>",
                    xml(home)
                )?;
            }
            for key in ["StandardOutPath", "StandardErrorPath"] {
                writeln!(
                    content,
                    "  <key>{key}</key><string>{}</string>",
                    xml(path_text(&log)?)
                )?;
            }
            content.push_str("</dict>\n</plist>\n");
            Ok(ServiceFile { path, content })
        }
        Platform::Linux => {
            // systemd rejects these characters after unquoting the executable
            // name; escaping them cannot make a valid command->path.
            // https://github.com/systemd/systemd/blob/main/src/basic/string-util.c
            if executable.contains(['\'', '"', '\\', '*', '?', '[', ']']) {
                bail!(
                    "systemd executable paths must not contain quotes, backslashes or glob characters; move the binary to a different path"
                );
            }
            let path = spec
                .user_home
                .join(format!(".config/systemd/user/navarch-{name}.service"));
            let mut content = format!(
                "[Unit]\nDescription=Navarch {name}\n\n[Service]\nType=simple\nExecStart=:{}\nRestart=on-failure\nRestartSec=5\n",
                args.iter()
                    .map(|a| unit_quote(a))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if let Some(home) = navarch_home {
                writeln!(
                    content,
                    "Environment={}",
                    unit_quote(&format!("NAVARCH_HOME={home}"))
                )?;
            }
            content.push_str("StandardOutput=journal\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n");
            Ok(ServiceFile { path, content })
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Install,
    Uninstall,
    Status,
}
fn platform() -> Result<Platform> {
    match std::env::consts::OS {
        "macos" => Ok(Platform::MacOs),
        "linux" => Ok(Platform::Linux),
        os => bail!("per-user services are unsupported on {os}"),
    }
}
fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
fn checked(program: &str, args: &[String]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("could not run {program} {}", args.join(" ")))?;
    if !status.success() {
        bail!(
            "{program} {} failed ({status}); check that the user service manager is available and inspect its logs",
            args.join(" ")
        );
    }
    Ok(())
}
fn commands(
    platform: Platform,
    action: Action,
    role: Role,
    file: &Path,
    uid: &str,
) -> Vec<(&'static str, Vec<String>)> {
    let name = role.name();
    let args = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
    match platform {
        Platform::MacOs => {
            let domain = format!("gui/{uid}");
            let target = format!("{domain}/dev.navarch.{name}");
            match action {
                Action::Install => vec![(
                    "launchctl",
                    args(&["bootstrap", &domain, &file.to_string_lossy()]),
                )],
                Action::Uninstall => vec![("launchctl", args(&["bootout", &target]))],
                Action::Status => vec![("launchctl", args(&["print", &target]))],
            }
        }
        Platform::Linux => {
            let unit = format!("navarch-{name}.service");
            match action {
                Action::Install => vec![
                    ("systemctl", args(&["--user", "daemon-reload"])),
                    ("systemctl", args(&["--user", "enable", "--now", &unit])),
                    ("systemctl", args(&["--user", "restart", &unit])),
                ],
                Action::Uninstall => {
                    vec![("systemctl", args(&["--user", "disable", "--now", &unit]))]
                }
                Action::Status => vec![("systemctl", args(&["--user", "status", &unit]))],
            }
        }
    }
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Execute a service operation. Dry runs only read environment/process metadata.
pub fn run(command: ServiceCommand) -> Result<u8> {
    let (action, args) = match command {
        ServiceCommand::Install(args) => (Action::Install, args),
        ServiceCommand::Uninstall(args) => (Action::Uninstall, args),
        ServiceCommand::Status(args) => (Action::Status, args),
    };
    let platform = platform()?;
    if args.listen.is_some()
        && (!matches!(args.role, Role::Relay) || !matches!(action, Action::Install))
    {
        bail!("--listen is only valid for service install relay");
    }
    let user_home =
        absolute(std::env::home_dir().context("cannot resolve the user home directory")?)?;
    let executable = std::env::current_exe()?;
    let navarch_home = std::env::var_os("NAVARCH_HOME")
        .map(|home| {
            if home.is_empty() {
                bail!("NAVARCH_HOME must not be empty");
            }
            absolute(home.into())
        })
        .transpose()?;
    // Uninstall/status need only the role's path, never the installed relay address.
    let spec = ServiceSpec {
        platform,
        role: args.role,
        executable: &executable,
        user_home: &user_home,
        navarch_home: navarch_home.as_deref(),
        listen: if matches!(action, Action::Install) {
            args.listen
        } else if matches!(args.role, Role::Relay) {
            Some("127.0.0.1:8080".parse()?)
        } else {
            None
        },
    };
    let file = generate(&spec)?;
    let uid = if matches!(platform, Platform::MacOs) {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("cannot determine launchd user ID")?;
        let uid = String::from_utf8(output.stdout)?.trim().to_owned();
        if !output.status.success() || uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
            bail!("id -u did not return a numeric user ID");
        }
        uid
    } else {
        String::new()
    };
    let commands = commands(platform, action, args.role, &file.path, &uid);
    if args.dry_run {
        println!("# {}\n{}", file.path.display(), file.content);
        if matches!(action, Action::Install) {
            println!("# Write the file above before running:");
        }
        for (program, args) in &commands {
            println!(
                "{} {}",
                program,
                args.iter()
                    .map(|s| shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if matches!(action, Action::Uninstall) {
            println!("rm -- {}", shell_quote(&file.path.to_string_lossy()));
            if matches!(platform, Platform::Linux) {
                println!("systemctl --user daemon-reload");
            }
        }
        return Ok(0);
    }
    if matches!(action, Action::Install) {
        let parent = file.path.parent().context("service file has no parent")?;
        std::fs::create_dir_all(parent)?;
        if matches!(platform, Platform::MacOs) {
            std::fs::create_dir_all(user_home.join(".navarch/logs"))?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(file.content.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(&file.path)
            .with_context(|| format!("cannot write {}", file.path.display()))?;
    }
    for (program, args) in commands {
        checked(program, &args)?;
    }
    if matches!(action, Action::Uninstall) {
        match std::fs::remove_file(&file.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if matches!(platform, Platform::Linux) {
            checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
        }
    }
    if !matches!(action, Action::Status) {
        println!(
            "{} service {}.",
            args.role.name(),
            if matches!(action, Action::Install) {
                "installed"
            } else {
                "uninstalled"
            }
        );
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn spec(platform: Platform, role: Role) -> ServiceSpec<'static> {
        ServiceSpec {
            platform,
            role,
            executable: Path::new("/opt/Navarch & \"tools\"/navarch%$\\x"),
            user_home: Path::new("/home/test & user"),
            navarch_home: Some(Path::new("/state/&\"<%$\\navarch")),
            listen: if matches!(role, Role::Relay) {
                Some("[::1]:8080".parse().unwrap())
            } else {
                None
            },
        }
    }
    #[test]
    fn launchd_plist_parses_and_preserves_arguments_and_environment() {
        for role in [Role::Hub, Role::Worker, Role::Relay] {
            let spec = spec(Platform::MacOs, role);
            let file = generate(&spec).unwrap();
            let parsed = plist::Value::from_reader_xml(file.content.as_bytes()).unwrap();
            let dict = parsed.as_dictionary().unwrap();
            let args = dict["ProgramArguments"].as_array().unwrap();
            assert_eq!(args[0].as_string(), spec.executable.to_str());
            assert_eq!(args[1].as_string(), Some(role.name()));
            assert_eq!(args[2].as_string(), Some("serve"));
            assert_eq!(args.len(), if matches!(role, Role::Relay) { 5 } else { 3 });
            assert_eq!(
                dict["EnvironmentVariables"].as_dictionary().unwrap()["NAVARCH_HOME"].as_string(),
                spec.navarch_home.unwrap().to_str()
            );
            assert_eq!(
                dict["KeepAlive"].as_dictionary().unwrap()["SuccessfulExit"].as_boolean(),
                Some(false)
            );
            assert_eq!(dict["RunAtLoad"].as_boolean(), Some(true));
            assert!(
                dict["StandardErrorPath"]
                    .as_string()
                    .unwrap()
                    .ends_with(&format!(".navarch/logs/{}.log", role.name()))
            );
            assert!(
                file.path
                    .ends_with(format!("dev.navarch.{}.plist", role.name()))
            );
        }
    }
    #[test]
    fn systemd_units_parse_with_restart_and_journal_and_escaped_paths() {
        for role in [Role::Hub, Role::Worker, Role::Relay] {
            let mut spec = spec(Platform::Linux, role);
            spec.executable = Path::new("/opt/Navarch & tools/navarch%$x");
            let file = generate(&spec).unwrap();
            let mut ini = configparser::ini::Ini::new_cs();
            ini.read(file.content).unwrap();
            assert_eq!(ini.get("Service", "Restart").as_deref(), Some("on-failure"));
            assert_eq!(
                ini.get("Service", "StandardOutput").as_deref(),
                Some("journal")
            );
            assert_eq!(
                ini.get("Install", "WantedBy").as_deref(),
                Some("default.target")
            );
            let exec = ini.get("Service", "ExecStart").unwrap();
            assert!(
                exec.starts_with(":\"/opt/Navarch & tools/navarch%%$x\""),
                "{exec}"
            );
            assert!(!exec.contains("$$"));
            assert!(exec.contains(&format!("\"{}\" \"serve\"", role.name())));
            let env = ini.get("Service", "Environment").unwrap();
            assert!(env.contains("%%$\\\\navarch"), "{env}");
            assert!(!env.contains("$$"));
        }
    }
    #[test]
    fn rejects_injection_relative_paths_and_wrong_listen_usage() {
        let mut spec = spec(Platform::Linux, Role::Hub);
        for path in [Path::new("relative"), Path::new("/bad\nRestart=no")] {
            spec.executable = path;
            assert!(generate(&spec).is_err());
        }
        spec.executable = Path::new("/bin/navarch");
        spec.listen = Some("127.0.0.1:1".parse().unwrap());
        assert!(generate(&spec).is_err());
        spec.listen = None;
        spec.role = Role::Hub;
        for path in [
            "/bin/nav'arch",
            "/bin/nav\"arch",
            "/bin/nav\\arch",
            "/bin/nav*arch",
            "/bin/nav?arch",
            "/bin/nav[arch]",
        ] {
            spec.executable = Path::new(path);
            assert!(generate(&spec).is_err());
        }
        spec.role = Role::Relay;
        spec.listen = None;
        assert!(generate(&spec).is_err());
    }
    #[test]
    fn absent_override_is_omitted_and_commands_target_user_manager() {
        for platform in [Platform::MacOs, Platform::Linux] {
            let mut spec = spec(platform, Role::Hub);
            spec.executable = Path::new("/bin/navarch");
            spec.navarch_home = None;
            let file = generate(&spec).unwrap();
            assert!(!file.content.contains("NAVARCH_HOME"));
            let commands = commands(platform, Action::Install, Role::Hub, &file.path, "501");
            match platform {
                Platform::MacOs => assert_eq!(commands[0].1[0..2], ["bootstrap", "gui/501"]),
                Platform::Linux => assert_eq!(
                    commands[1].1,
                    ["--user", "enable", "--now", "navarch-hub.service"]
                ),
            }
        }
    }
}
