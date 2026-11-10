//! Hub command arguments and bounded values.
use clap::{Args, Subcommand};
use navarch_proto::{CardId, MachineId, TaskId};
use std::path::PathBuf;

pub(super) const EXIT_HELP: &str = "Exit codes: 0 success (wait: finished); 1 usage, daemon unreachable or hub rejection; 3 failed, policy rejected or cancelled; 4 permission answer needed; 124 wait timed out (task keeps running).";

/// Hub commands, all using the local daemon's control API.
#[derive(Subcommand)]
#[command(after_help = EXIT_HELP)]
pub enum HubCommand {
    /// Serve the local hub daemon and worker/control sockets.
    Serve,
    /// Follow unread replay and live events until interrupted.
    Watch {
        /// Print raw JSON lines.
        #[arg(long)]
        json: bool,
    },
    /// Show machines, their agents, and online status.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show task-card summaries.
    Tasks {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Include terminal cards.
        #[arg(long)]
        all: bool,
    },
    /// Dispatch work to a worker's agent or role.
    Dispatch(DispatchArgs),
    /// Continue a card's latest session, or address a session directly.
    Send(SendArgs),
    /// Cancel the card's running step, or a specific task.
    Cancel {
        /// Card whose active step should stop.
        #[arg(required_unless_present = "task", conflicts_with = "task")]
        card_id: Option<CardId>,
        /// Cancel a turn directly.
        #[arg(long)]
        task: Option<TaskId>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Read a full card and the latest step's result text.
    Task {
        /// Card identifier.
        card_id: CardId,
        /// Include event history.
        #[arg(long)]
        log: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List resumable worker and adapter sessions.
    Sessions {
        /// Filter by machine.
        #[arg(long)]
        machine: Option<MachineId>,
        /// Filter by agent.
        #[arg(long, value_parser = bounded)]
        agent: Option<String>,
        /// Filter by worker-local directory.
        #[arg(long, value_parser = path)]
        cwd: Option<PathBuf>,
        /// Maximum entries per worker (daemon caps at 200).
        #[arg(long)]
        limit: Option<usize>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Read unread events and mark the returned batch read.
    Inbox {
        /// Inspect compact events without marking read.
        #[arg(long)]
        peek: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Wait for events, or for a card to need attention.
    #[command(after_help = EXIT_HELP)]
    Wait(WaitArgs),
    /// Answer an outstanding permission using an offered option.
    Permission {
        /// Permission request UUID.
        #[arg(value_parser = bounded)]
        request_id: String,
        /// Offered option ID.
        #[arg(long, required_unless_present = "cancel", conflicts_with = "cancel", value_parser = bounded)]
        choose: Option<String>,
        /// Cancel the permission request.
        #[arg(long)]
        cancel: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List machines or change approval state.
    Machines {
        /// Print JSON.
        #[arg(long, global = true)]
        json: bool,
        /// Optional approval action.
        #[command(subcommand)]
        action: Option<MachineAction>,
    },
    /// Write example configuration without overwriting files.
    Init,
    /// Print the orchestrator guide.
    Guide {
        /// Print the guide in a JSON document.
        #[arg(long)]
        json: bool,
    },
}
/// Machine approval commands.
#[derive(Subcommand)]
pub enum MachineAction {
    /// Approve a pending machine.
    Approve {
        /// Machine ID.
        id: MachineId,
    },
    /// Immediately revoke a machine.
    Revoke {
        /// Machine ID.
        id: MachineId,
    },
}
/// A prompt supplied by one positional, file, or stdin source.
#[derive(Args)]
pub struct PromptArgs {
    /// Prompt text, or '-' to read stdin; omitted reads non-terminal stdin.
    #[arg(conflicts_with = "prompt_file", value_parser = prompt)]
    pub prompt: Option<String>,
    /// Read UTF-8 prompt text from a file (256 KiB maximum).
    #[arg(long, value_parser = path)]
    pub prompt_file: Option<PathBuf>,
}
/// Waiting options shared by dispatch and send.
#[derive(Args)]
pub struct FollowArgs {
    /// Wait for completion or a permission request.
    #[arg(long)]
    pub wait: bool,
    /// Wait seconds (default 300); 0 means no limit. Timeout leaves work running.
    #[arg(long, requires = "wait")]
    pub timeout: Option<u64>,
    /// Print one JSON document.
    #[arg(long)]
    pub json: bool,
}
/// New work and optional session settings.
#[derive(Args)]
#[command(after_help = EXIT_HELP)]
pub struct DispatchArgs {
    /// Target machine ID.
    #[arg(long)]
    pub machine: MachineId,
    /// Explicit adapter name, exclusive with role.
    #[arg(long, required_unless_present = "role", conflicts_with = "role", value_parser = bounded)]
    pub agent: Option<String>,
    /// Configured role name, exclusive with agent.
    #[arg(long, value_parser = bounded)]
    pub role: Option<String>,
    /// Worker-local working directory.
    #[arg(long, value_parser = path)]
    pub cwd: PathBuf,
    /// Card title, at most 200 Unicode characters.
    #[arg(long, value_parser = title)]
    pub title: Option<String>,
    /// Configured project name or alias.
    #[arg(long, value_parser = bounded)]
    pub project: Option<String>,
    /// Session mode override.
    #[arg(long, value_parser = bounded)]
    pub mode: Option<String>,
    /// Session model override.
    #[arg(long, value_parser = bounded)]
    pub model: Option<String>,
    /// Append a hand-off step to an existing card.
    #[arg(long)]
    pub card: Option<CardId>,
    /// JSON array of servers for the worker session (file at most 64 KiB).
    #[arg(long, value_parser = path)]
    pub mcp_servers: Option<PathBuf>,
    /// Prompt source.
    #[command(flatten)]
    pub prompt: PromptArgs,
    /// Output and wait options.
    #[command(flatten)]
    pub follow: FollowArgs,
}
/// Continue work in a card or an explicitly addressed session.
#[derive(Args)]
#[command(after_help = EXIT_HELP)]
pub struct SendArgs {
    /// Card whose latest step supplies the session.
    #[arg(required_unless_present = "machine", value_parser = prompt)]
    pub card_id: Option<String>,
    /// Direct session's machine.
    #[arg(long, requires = "session")]
    pub machine: Option<MachineId>,
    /// Direct worker or adapter session ID.
    #[arg(long, requires = "machine", value_parser = bounded)]
    pub session: Option<String>,
    /// Agent for an external adapter session ID.
    #[arg(long, requires = "session", value_parser = bounded)]
    pub agent: Option<String>,
    /// Override the direct session's owning card.
    #[arg(long, requires = "session")]
    pub card: Option<CardId>,
    /// Prompt source.
    #[command(flatten)]
    pub prompt: PromptArgs,
    /// Output and wait options.
    #[command(flatten)]
    pub follow: FollowArgs,
}
/// Shared standalone waiting arguments.
#[derive(Args)]
pub struct WaitArgs {
    /// Wait only for this card's attention events.
    #[arg(long)]
    pub card: Option<CardId>,
    /// Exclude events at or below this sequence.
    #[arg(long)]
    pub after_seq: Option<u64>,
    /// Wait seconds (default 300); 0 means no limit.
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Print one JSON document.
    #[arg(long)]
    pub json: bool,
}
fn prompt(value: &str) -> Result<String, String> {
    if value.len() > 256 * 1024 || value.trim().is_empty() {
        return Err("prompt must be non-empty and at most 256 KiB".into());
    }
    Ok(value.into())
}
fn title(value: &str) -> Result<String, String> {
    if value.chars().count() > 200 {
        return Err("title exceeds 200 characters".into());
    }
    Ok(value.into())
}
fn bounded(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 4096 {
        return Err("value must be non-empty and at most 4096 bytes".into());
    }
    Ok(value.into())
}
fn path(value: &str) -> Result<PathBuf, String> {
    bounded(value).map(PathBuf::from)
}
