//! Standalone worker argument parsing and output wiring.
use anyhow::{Context, Result};
use clap::Subcommand;
use navarch_acp::{AgentEvent, NewSession};
use navarch_proto::{Body, PermissionDecision, PermissionRequest, TaskId};
use navarch_worker::{EscalationHandler, Worker, WorkerStreams};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
};
/// Standalone worker commands.
#[derive(Subcommand)]
pub enum WorkerCommand {
    /// Serve this worker over the local hub link, reconnecting after restarts.
    Serve,
    /// Start a new session and stream one turn.
    Run {
        /// Configured agent name.
        #[arg(long)]
        agent: String,
        /// Working directory inside an allowed root.
        #[arg(long)]
        cwd: PathBuf,
        /// Permission mode override.
        #[arg(long)]
        mode: Option<String>,
        /// Model override.
        #[arg(long)]
        model: Option<String>,
        /// Prompt text.
        prompt: String,
    },
    /// Resume a session from this or a previous process.
    Send {
        /// Session ID printed by run.
        #[arg(long)]
        session: String,
        /// Prompt text.
        prompt: String,
    },
    /// List local persistent sessions.
    Sessions {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Probe configured adapters and repositories (starts adapters).
    Probe {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Diagnose configuration and executable availability without starting agents.
    Doctor,
    /// Write example configuration and policy files if absent.
    Init,
}
struct TerminalEscalation {
    input: tokio::sync::Mutex<()>,
}
#[async_trait::async_trait]
impl EscalationHandler for TerminalEscalation {
    async fn ask(&self, request: PermissionRequest) -> PermissionDecision {
        if !io::stdin().is_terminal() {
            return navarch_worker::policy_answer(navarch_policy::Decision::Deny, &request);
        }
        let _guard = self.input.lock().await;
        eprintln!("\nPermission: {}\n{}", request.tool, request.detail);
        for (i, option) in request.options.iter().enumerate() {
            eprintln!("  {}: {} ({:?})", i + 1, option.name, option.kind);
        }
        eprint!("Choose an option number (Enter cancels): ");
        let _ = io::stderr().flush();
        let Ok(input) = super::terminal::read_line().await else {
            return PermissionDecision::Cancelled;
        };
        input
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| request.options.get(i))
            .map(|option| PermissionDecision::Selected {
                option_id: option.id.clone(),
            })
            .unwrap_or(PermissionDecision::Cancelled)
    }
}
/// Execute standalone worker wiring. Returns the requested process exit status.
pub async fn run(command: WorkerCommand) -> Result<u8> {
    let home = crate::home::home()?;
    if matches!(command, WorkerCommand::Init) {
        navarch_worker::config::init(&home)?;
        eprintln!("Configuration examples are in {}", home.display());
        return Ok(0);
    }
    let user_home =
        std::env::home_dir().context("cannot resolve user home for policy expansion")?;
    let escalation = if matches!(command, WorkerCommand::Serve) {
        None
    } else {
        Some(Arc::new(TerminalEscalation {
            input: tokio::sync::Mutex::new(()),
        }) as Arc<dyn EscalationHandler>)
    };
    let (worker, streams) = Worker::load(&home, &user_home, escalation)?;
    match command {
        WorkerCommand::Serve => {
            let identity = navarch_crypto::Identity::load_or_create(&home)?;
            let config = navarch_worker::config::Config::load(&home)?;
            let connector = navarch_transport::LocalConnector {
                path: home.join("hub-worker.sock"),
                hello: navarch_transport::LocalHello {
                    public_key: identity.public_key(),
                    display_name: config.display_name,
                },
            };
            tokio::select! {result = navarch_worker::serve(worker, streams, identity.machine_id().parse()?, Box::new(connector)) => result?, result = tokio::signal::ctrl_c() => result?}
            Ok(0)
        }
        WorkerCommand::Run {
            agent,
            cwd,
            mode,
            model,
            prompt,
        } => {
            let task = TaskId::new();
            worker
                .dispatch(
                    task,
                    &agent,
                    NewSession {
                        cwd,
                        mode,
                        model,
                        mcp_servers: vec![],
                    },
                    prompt,
                )
                .await?;
            stream(streams, task).await
        }
        WorkerCommand::Send { session, prompt } => {
            let task = TaskId::new();
            worker.send(task, &session, prompt).await?;
            stream(streams, task).await
        }
        WorkerCommand::Sessions { json } => {
            let sessions = worker.sessions()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                for s in sessions {
                    println!(
                        "{}\t{}\t{:?}\t{}",
                        s.session_id,
                        s.agent,
                        s.state,
                        s.config.cwd.display()
                    );
                }
            }
            Ok(0)
        }
        WorkerCommand::Probe { json } => {
            let report = worker.probe().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} {}: {} running turns",
                    report.os, report.arch, report.running_turns
                );
                for agent in report.agents {
                    println!(
                        "{}: present={}, version={}, models={}{}",
                        agent.name,
                        agent.present,
                        agent.version.as_deref().unwrap_or("unknown"),
                        agent.models.join(", "),
                        agent
                            .error
                            .map(|e| format!(", error={e}"))
                            .unwrap_or_default()
                    );
                }
                for repo in report.repositories {
                    println!("repository: {}", repo.display());
                }
            }
            Ok(0)
        }
        WorkerCommand::Doctor => {
            for line in worker.doctor() {
                println!("{line}");
            }
            Ok(0)
        }
        WorkerCommand::Init => Ok(0),
    }
}
fn print_event(event: AgentEvent) -> Result<()> {
    if let AgentEvent::MessageChunk(text) = event {
        print!("{text}");
        io::stdout().flush()?;
    }
    Ok(())
}
async fn stream(mut streams: WorkerStreams, task: TaskId) -> Result<u8> {
    loop {
        tokio::select! {
            Some(event)=streams.events.recv()=>{if event.task_id==task{print_event(event.event)?;}},
            message=streams.messages.recv()=>match message.context("worker stopped before finishing the turn")?{
                Body::Accepted{session_id,..}=>eprintln!("Session: {session_id}"),
                Body::Finished{task_id,stop_reason,..} if task_id==task=>{while let Ok(event)=streams.events.try_recv(){if event.task_id==task{print_event(event.event)?;}}eprintln!("\nStop reason: {stop_reason}");return Ok(if stop_reason=="end_turn"{0}else{1});},
                Body::Failed{task_id,error} if task_id==task=>anyhow::bail!("{error}"),
                Body::Rejected{task_id,reason} if task_id==task=>anyhow::bail!("{reason}"),
                _=>{},
            }
        }
    }
}
