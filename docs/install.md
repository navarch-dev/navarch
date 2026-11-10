# Installing Navarch

Navarch ships as one binary for macOS and Linux, on x86-64 and arm64. Linux release
binaries require glibc 2.35 or newer. Windows and musl distributions are not release
targets. Interfaces and state formats can still change; the cryptography has not
had an independent audit.

## From a release

Once a release has been published, download and inspect the installer, then run it:

```sh
curl -fLsS https://raw.githubusercontent.com/navarch-dev/navarch/main/install.sh -o install.sh
less install.sh
sh install.sh
export PATH="$HOME/.local/bin:$PATH"
navarch --version
```

The installer downloads the latest GitHub release archive and its `SHA256SUMS`,
checks SHA-256, and installs only after a matching checksum. It never uses `sudo`.
It needs `curl`, `tar`, and `sha256sum` or macOS's `shasum`. The checksum protects
against corrupted downloads; it is not an independent signature of a release.
Set `NAVARCH_VERSION=0.1.0` (with or without `v`) to select a published release, or
`NAVARCH_INSTALL_DIR=/absolute/path` to choose a different installation directory.
The default is `$HOME/.local/bin`; add the printed PATH line to your shell profile
if necessary. Do not pipe downloaded content into a shell.

You can also download the matching `navarch-<version>-<target>.tar.gz` and
`SHA256SUMS` from [GitHub Releases](https://github.com/navarch-dev/navarch/releases),
compare `sha256sum <archive>` (or `shasum -a 256 <archive>` on macOS) with
the matching filename entry in `SHA256SUMS`, and extract it yourself. Each archive contains `navarch`, `LICENSE`, and `README.md`.

## From source

Install Rust 1.88 or newer and a platform C toolchain, then:

```sh
git clone https://github.com/navarch-dev/navarch.git
cd navarch
cargo build --locked --release -p navarch-cli
mkdir -p "$HOME/.local/bin"
cp target/release/navarch "$HOME/.local/bin/navarch"
navarch --version
```

The version includes the Git commit and `-dirty` when applicable. Source archives
without Git metadata report `unknown` instead. The build works without Git.

## First local setup

```sh
navarch hub init
navarch worker init
# Edit ~/.navarch/worker.toml and ~/.navarch/worker-policy.toml.
# Configure an installed ACP adapter and the directories it may use.
navarch hub serve
# In another terminal:
navarch worker serve
# In another terminal:
navarch hub status
claude mcp add --transport stdio navarch -- navarch hub mcp
```

Both configuration commands preserve existing files. The worker starts with no
allowed roots, so explicitly configure its policy. Use `NAVARCH_HOME` consistently
for every command if overriding `~/.navarch`. See [worker.md](worker.md),
[hub.md](hub.md), and [relay.md](relay.md) for adapters, events, direct links, and
remote pairing.

## Per-user services

Preview the generated configuration before installing:

```sh
navarch service install hub --dry-run
navarch service install worker --dry-run
navarch service install hub
navarch service install worker
navarch service status hub
```

Stop foreground copies before installing services. Configure the hub and worker
first; services do not create adapter configuration or broaden policy. Relay
installation requires an explicit listening address:

```sh
navarch service install relay --listen 127.0.0.1:8080 --dry-run
navarch service install relay --listen 127.0.0.1:8080
```

The generated file embeds the current executable's absolute path and, when set,
the absolute `NAVARCH_HOME`. Keep the executable at that path. No commands require
root. A dry run prints the file and service-manager commands without writing
files or loading services.

On macOS, files are `~/Library/LaunchAgents/dev.navarch.<role>.plist`, loaded with
`launchctl bootstrap gui/<uid>`. Install while logged into the GUI session. Logs
are `~/.navarch/logs/<role>.log`, including when NAVARCH_HOME is overridden. Agents
start at login and restart after failure. Before reinstalling an already loaded
agent, use `navarch service uninstall <role>` to unload it.

On Linux, files are `~/.config/systemd/user/navarch-<role>.service`, enabled with
`systemctl --user enable --now`. This needs an available systemd user manager;
services normally run while the user is logged in. Read logs with
`journalctl --user -u navarch-hub.service -f` (substitute worker or relay). Units
restart after failure. Service-manager errors include the failed command.
systemd rejects executable paths containing quotes, backslashes, or glob characters;
move the binary to another path if service generation reports this limitation.

## Upgrading

Back up your Navarch state directory. Uninstall running services first, or stop
foreground daemons. Re-run the installer with the desired `NAVARCH_VERSION`, or
rebuild from source and replace the binary. Check `navarch --version`, then install
the same services again and check their status. Reinstalling regenerates the
absolute executable path and the current NAVARCH_HOME setting. Key files and task
state are retained by service uninstall and by the installer.

## Uninstalling

```sh
navarch service uninstall worker
navarch service uninstall hub
# If installed:
navarch service uninstall relay
rm "$HOME/.local/bin/navarch"
```

Use your chosen installation path if different. Remove the PATH line from your
shell profile if it is no longer needed. Service uninstall removes only service
files; configuration, identity keys, pairing records, and task history remain.
Delete the state directory yourself only when you intend to discard those records.
