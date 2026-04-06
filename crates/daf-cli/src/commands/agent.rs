//! `daf agent` — Manage agents: list, spawn, inspect, terminate, stream logs.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::display::{colorize_status, format_bytes, format_duration, format_table, kv, section, status_icon};
use crate::Cli;

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

/// Minimal manifest deserialization for the spawn path.
#[derive(Debug, Deserialize)]
struct ManifestFile {
    agent: AgentDef,
    #[serde(default)]
    capabilities: Vec<CapDef>,
    #[serde(default)]
    metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AgentDef {
    name: String,
    kind: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct CapDef {
    name: String,
    version: String,
    #[serde(default)]
    description: String,
}

/// Dispatch agent subcommands.
pub async fn exec(cmd: &AgentCommand, cli: &Cli) -> Result<()> {
    match cmd {
        AgentCommand::List(args) => exec_list(args, cli).await,
        AgentCommand::Spawn(args) => exec_spawn(args, cli).await,
        AgentCommand::Inspect(args) => exec_inspect(args, cli).await,
        AgentCommand::Terminate(args) => exec_terminate(args, cli).await,
        AgentCommand::Logs(args) => exec_logs(args, cli).await,
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

async fn exec_list(args: &ListArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Querying agent registry...",
        style("[agent]").cyan().bold(),
    );

    // In production this queries daf-registry. For now we demonstrate the
    // table formatting with placeholder data.
    let mut table = format_table(&["", "ID", "Name", "Kind", "Status", "Capabilities", "Uptime"]);

    // Placeholder entries — replaced by real registry queries once
    // daf-registry exposes its gRPC/socket API.
    let agents: Vec<(&str, &str, &str, &str, &str, &str)> = vec![
        ("a1b2c3d4", "code-reviewer", "specialist", "executing", "code_review@1.0", "2h 14m"),
        ("e5f6a7b8", "test-runner", "worker", "idle", "test@2.0", "1h 03m"),
        ("c9d0e1f2", "orchestrator-1", "orchestrator", "executing", "plan@1.0, delegate@1.0", "4h 51m"),
    ];

    for (id, name, kind, status, caps, uptime) in &agents {
        // Apply filters.
        if let Some(ref k) = args.kind {
            if kind != k {
                continue;
            }
        }
        if let Some(ref s) = args.status {
            if status != s {
                continue;
            }
        }

        table.add_row(vec![
            status_icon(status),
            id.to_string(),
            name.to_string(),
            kind.to_string(),
            colorize_status(status),
            caps.to_string(),
            uptime.to_string(),
        ]);
    }

    println!("{table}");
    Ok(())
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

async fn exec_spawn(args: &SpawnArgs, _cli: &Cli) -> Result<()> {
    let raw = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("cannot read manifest: {}", args.manifest.display()))?;

    let manifest: ManifestFile = serde_json::from_value(
        serde_yaml_ng::from_str::<serde_json::Value>(&raw)
            .with_context(|| "invalid YAML in agent manifest")?,
    )
    .with_context(|| "manifest does not match expected schema")?;

    let name = args.name.as_deref().unwrap_or(&manifest.agent.name);

    eprintln!(
        "{} Spawning {} instance(s) of agent {}",
        style("[agent]").cyan().bold(),
        style(args.count).bold(),
        style(name).green().bold(),
    );

    let pb = ProgressBar::new(args.count as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} spawning [{bar:30.green/dim}] {pos}/{len} ({eta})",
        )?
        .progress_chars("\u{2588}\u{2592}\u{2591}"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    for i in 0..args.count {
        // In production: call daf-runtime to spawn the agent process,
        // register with daf-registry, and set up transport channels.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let agent_id = uuid::Uuid::now_v7();
        tracing::info!(
            agent_id = %agent_id,
            name = name,
            kind = %manifest.agent.kind,
            instance = i + 1,
            "agent spawned",
        );
        pb.inc(1);
    }

    pb.finish_and_clear();

    let caps: Vec<String> = manifest
        .capabilities
        .iter()
        .map(|c| format!("{}@{}", c.name, c.version))
        .collect();

    eprintln!(
        "\n{} Spawned {} agent(s): {} [{}] ({})",
        style("\u{2714}").green().bold(),
        args.count,
        style(name).green().bold(),
        style(&manifest.agent.kind).dim(),
        if caps.is_empty() {
            "no capabilities".to_string()
        } else {
            caps.join(", ")
        },
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

async fn exec_inspect(args: &InspectArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Inspecting agent {}",
        style("[agent]").cyan().bold(),
        style(&args.id).bold(),
    );

    // Placeholder — in production, fetches from daf-registry + daf-runtime.
    section("Agent Details");
    kv("ID", &args.id);
    kv("Name", "code-reviewer");
    kv("Kind", "specialist");
    kv("Status", &colorize_status("executing"));
    kv("Uptime", "2h 14m 8s");
    kv("Parent", "orchestrator-1 (c9d0e1f2)");

    section("Capabilities");
    kv("code_review", "1.0.0 — Review code for quality and security");

    section("Resources");
    kv("Memory", &format!("{} / {}", format_bytes(128 * 1024 * 1024), format_bytes(512 * 1024 * 1024)));
    kv("CPU time", "42s / 300s");
    kv("Connections", "3 / 64");
    kv("Message queue", "12 / 1024");

    section("Active Tasks");
    let mut task_table = format_table(&["Task ID", "Name", "Status", "Started"]);
    task_table.add_row(vec!["t-001", "review-pr-42", "executing", "2m ago"]);
    println!("{task_table}");

    section("Recent Metrics");
    kv("Messages sent", "847");
    kv("Messages received", "1,203");
    kv("Avg response time", "145ms");
    kv("Error rate", "0.2%");

    Ok(())
}

// ---------------------------------------------------------------------------
// terminate
// ---------------------------------------------------------------------------

async fn exec_terminate(args: &TerminateArgs, _cli: &Cli) -> Result<()> {
    let mode = if args.force { "force-killing" } else { "gracefully stopping" };

    eprintln!(
        "{} {} agent {}",
        style("[agent]").cyan().bold(),
        mode,
        style(&args.id).bold(),
    );

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} {msg}")?.tick_strings(&[
            "\u{25CB}",
            "\u{25D4}",
            "\u{25D1}",
            "\u{25D5}",
            "\u{25CF}",
        ]),
    );
    pb.set_message(format!("waiting for shutdown (timeout: {}s)...", args.timeout));
    pb.enable_steady_tick(Duration::from_millis(120));

    // In production: send shutdown signal via daf-transport, wait up to
    // timeout, then force-kill if needed.
    tokio::time::sleep(Duration::from_millis(300)).await;

    pb.finish_with_message(format!(
        "{} agent {} terminated",
        status_icon("ok"),
        style(&args.id).bold(),
    ));

    Ok(())
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

async fn exec_logs(args: &LogsArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Streaming logs for agent {} (tail={}, follow={})",
        style("[agent]").cyan().bold(),
        style(&args.id).bold(),
        args.tail,
        args.follow,
    );

    if let Some(ref level) = args.level {
        eprintln!("  filter: level >= {}", style(level).yellow());
    }

    eprintln!();

    // Placeholder log entries — in production these come from daf-logger.
    let entries = [
        ("2026-04-07T10:14:02Z", "INFO", "agent initialized, capabilities registered"),
        ("2026-04-07T10:14:03Z", "DEBUG", "connected to transport layer on tcp://127.0.0.1:9400"),
        ("2026-04-07T10:14:05Z", "INFO", "received task assignment: review-pr-42"),
        ("2026-04-07T10:15:12Z", "DEBUG", "fetching diff from repository (1,247 lines)"),
        ("2026-04-07T10:15:14Z", "INFO", "code review complete — 3 issues found"),
    ];

    for (ts, level, msg) in &entries {
        let level_styled = match *level {
            "ERROR" => style(*level).red().bold().to_string(),
            "WARN" => style(*level).yellow().bold().to_string(),
            "INFO" => style(*level).green().to_string(),
            "DEBUG" => style(*level).blue().to_string(),
            "TRACE" => style(*level).dim().to_string(),
            _ => level.to_string(),
        };
        println!(
            "{} {} {}  {}",
            style(ts).dim(),
            level_styled,
            style(format!("[{}]", args.id)).cyan(),
            msg,
        );
    }

    if args.follow {
        eprintln!("\n{}", style("  (streaming... press Ctrl+C to stop)").dim());
        // In production: subscribe to the agent's log channel via
        // daf-logger and print entries as they arrive.
    }

    Ok(())
}
