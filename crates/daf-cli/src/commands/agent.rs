//! `daf agent` — Manage agents: list, spawn, inspect, terminate, stream logs.

use crate::Cli;
use anyhow::Result;
use std::path::PathBuf;

/// Subcommands for `daf agent`.
#[derive(Debug, clap::Subcommand)]
pub enum AgentCommand {
    /// List all registered agents.
    List(ListArgs),
    /// Spawn a new agent from a manifest file.
    Spawn(SpawnArgs),
    /// Show detailed information about an agent.
    Inspect(InspectArgs),
    /// Gracefully terminate an agent.
    Terminate(TerminateArgs),
    /// Stream an agent's log entries.
    Logs(LogsArgs),
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Filter by agent kind (orchestrator, specialist, worker, monitor, router).
    #[arg(short, long)]
    pub kind: Option<String>,

    /// Filter by status.
    #[arg(short, long)]
    pub status: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct SpawnArgs {
    /// Path to the agent manifest YAML.
    pub manifest: PathBuf,

    /// Override the agent name.
    #[arg(short, long)]
    pub name: Option<String>,

    /// Number of instances to spawn.
    #[arg(short, long, default_value_t = 1)]
    pub count: u32,
}

#[derive(Debug, clap::Args)]
pub struct InspectArgs {
    /// Agent ID (UUID or short prefix).
    pub id: String,
}

#[derive(Debug, clap::Args)]
pub struct TerminateArgs {
    /// Agent ID to terminate.
    pub id: String,

    /// Force kill without waiting for graceful shutdown.
    #[arg(short, long)]
    pub force: bool,

    /// Shutdown timeout in seconds.
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
}

#[derive(Debug, clap::Args)]
pub struct LogsArgs {
    /// Agent ID to stream logs for.
    pub id: String,

    /// Number of recent lines to show before streaming.
    #[arg(short = 'n', long, default_value_t = 50)]
    pub tail: usize,

    /// Follow mode (stream continuously).
    #[arg(short, long)]
    pub follow: bool,

    /// Filter by log level (trace, debug, info, warn, error).
    #[arg(short, long)]
    pub level: Option<String>,
}

pub async fn exec(_args: &AgentCommand, _cli: &Cli) -> Result<()> {
    super::unavailable("agent")
}
