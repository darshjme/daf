//! `daf log` — Query and stream agent logs, conversations, and episodes.
//!
//! Provides tail, search, conversation replay, KB extraction, and episode
//! listing with color-coded terminal output.

use std::time::Duration;

use anyhow::Result;
use console::style;

use crate::display::{format_table, status_icon};
use crate::Cli;

/// Subcommands for `daf log`.
#[derive(Debug, clap::Subcommand)]
pub enum LogCommand {
    /// Live-stream all agent conversations.
    Tail(TailArgs),
    /// Search log entries by query string.
    Search(SearchArgs),
    /// Show a full conversation transcript.
    Conversation(ConversationArgs),
    /// Extract knowledge base entries from a conversation.
    Extract(ExtractArgs),
    /// List recorded episodes.
    Episodes(EpisodesArgs),
}

#[derive(Debug, clap::Args)]
pub struct TailArgs {
    /// Number of recent entries to show before streaming.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub lines: usize,

    /// Filter by agent name or ID.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Filter by log level.
    #[arg(short, long)]
    pub level: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// Search query (supports regex).
    pub query: String,

    /// Maximum results to return.
    #[arg(short, long, default_value_t = 50)]
    pub max: usize,

    /// Filter by agent.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Time window (e.g., "1h", "30m", "7d").
    #[arg(short, long)]
    pub since: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct ConversationArgs {
    /// Conversation ID.
    pub id: String,

    /// Show raw message payloads.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, clap::Args)]
pub struct ExtractArgs {
    /// Conversation ID to extract from.
    pub conversation_id: String,

    /// Output format for extracted entries.
    #[arg(long, default_value = "yaml")]
    pub output: String,
}

#[derive(Debug, clap::Args)]
pub struct EpisodesArgs {
    /// Filter by agent.
    #[arg(short, long)]
    pub agent: Option<String>,

    /// Maximum episodes to list.
    #[arg(short, long, default_value_t = 20)]
    pub max: usize,
}

/// Dispatch log subcommands.
pub async fn exec(cmd: &LogCommand, cli: &Cli) -> Result<()> {
    match cmd {
        LogCommand::Tail(args) => exec_tail(args, cli).await,
        LogCommand::Search(args) => exec_search(args, cli).await,
        LogCommand::Conversation(args) => exec_conversation(args, cli).await,
        LogCommand::Extract(args) => exec_extract(args, cli).await,
        LogCommand::Episodes(args) => exec_episodes(args, cli).await,
    }
}

// ---------------------------------------------------------------------------
// tail
// ---------------------------------------------------------------------------

async fn exec_tail(args: &TailArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Streaming logs (last {} entries){}",
        style("[log]").cyan().bold(),
        args.lines,
        args.agent
            .as_ref()
            .map(|a| format!(" agent={}", style(a).cyan()))
            .unwrap_or_default(),
    );
    eprintln!();

    // Placeholder entries — in production these come from daf-logger's
    // ring buffer and then switch to live subscription.
    let entries = [
        ("10:14:02", "orchestrator-1", "INFO", "decomposing goal into 3 subtasks"),
        ("10:14:03", "code-reviewer", "DEBUG", "connected to transport tcp://127.0.0.1:9400"),
        ("10:14:05", "test-runner", "INFO", "received task: run-unit-tests"),
        ("10:15:12", "code-reviewer", "INFO", "reviewing PR #42 (1,247 lines)"),
        ("10:15:14", "code-reviewer", "WARN", "potential SQL injection in auth.rs:142"),
        ("10:15:15", "test-runner", "INFO", "all 47 tests passed"),
        ("10:15:16", "orchestrator-1", "INFO", "subtask complete: code-review (3 issues)"),
        ("10:15:17", "orchestrator-1", "INFO", "subtask complete: test-run (47/47 pass)"),
    ];

    for (ts, agent, level, msg) in &entries {
        if let Some(ref filter_agent) = args.agent {
            if !agent.contains(filter_agent.as_str()) {
                continue;
            }
        }
        if let Some(ref filter_level) = args.level {
            let level_num = level_rank(level);
            let filter_num = level_rank(filter_level);
            if level_num < filter_num {
                continue;
            }
        }

        print_log_entry(ts, agent, level, msg);
    }

    eprintln!(
        "\n{}",
        style("  (streaming... press Ctrl+C to stop)").dim(),
    );

    // In production: subscribe to daf-logger's broadcast channel and
    // print entries as they arrive.
    Ok(())
}

fn level_rank(level: &str) -> u8 {
    match level.to_uppercase().as_str() {
        "TRACE" => 0,
        "DEBUG" => 1,
        "INFO" => 2,
        "WARN" => 3,
        "ERROR" => 4,
        _ => 2,
    }
}

fn print_log_entry(ts: &str, agent: &str, level: &str, msg: &str) {
    let level_styled = match level.to_uppercase().as_str() {
        "ERROR" => style(format!("{:<5}", level)).red().bold().to_string(),
        "WARN" => style(format!("{:<5}", level)).yellow().bold().to_string(),
        "INFO" => style(format!("{:<5}", level)).green().to_string(),
        "DEBUG" => style(format!("{:<5}", level)).blue().to_string(),
        "TRACE" => style(format!("{:<5}", level)).dim().to_string(),
        _ => format!("{:<5}", level),
    };

    println!(
        "{} {} {} {}",
        style(ts).dim(),
        level_styled,
        style(format!("[{agent}]")).cyan(),
        msg,
    );
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

async fn exec_search(args: &SearchArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Searching logs for: {}\n",
        style("[log]").cyan().bold(),
        style(&args.query).yellow().bold(),
    );

    // Placeholder — in production this queries daf-logger's indexed store.
    let results = [
        ("10:15:14", "code-reviewer", "WARN", "potential SQL injection in auth.rs:142"),
        ("09:42:03", "security-scanner", "WARN", "SQL injection pattern detected in query builder"),
    ];

    for (ts, agent, level, msg) in &results {
        if let Some(ref filter_agent) = args.agent {
            if !agent.contains(filter_agent.as_str()) {
                continue;
            }
        }
        print_log_entry(ts, agent, level, msg);
    }

    eprintln!(
        "\n  {} result(s) found",
        style(results.len()).bold(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// conversation
// ---------------------------------------------------------------------------

async fn exec_conversation(args: &ConversationArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Conversation {}\n",
        style("[log]").cyan().bold(),
        style(&args.id).bold(),
    );

    // Placeholder transcript — in production this reads from daf-logger's
    // conversation store.
    let turns = [
        ("orchestrator-1", "system", "Decompose the goal: 'Review PR #42 for security issues and run tests'"),
        ("orchestrator-1", "assistant", "I will create 2 subtasks:\n  1. code-review: Review PR #42 for security vulnerabilities\n  2. test-run: Execute the full test suite"),
        ("code-reviewer", "system", "Review PR #42 for security vulnerabilities"),
        ("code-reviewer", "assistant", "Found 3 issues:\n  - WARN: potential SQL injection in auth.rs:142\n  - INFO: unused import in lib.rs:8\n  - INFO: missing error handling in api.rs:67"),
        ("test-runner", "system", "Execute the full test suite"),
        ("test-runner", "assistant", "All 47 tests passed (42 unit, 5 integration). Coverage: 78.3%"),
    ];

    for (agent, role, content) in &turns {
        let role_styled = match *role {
            "system" => style(format!("{:<9}", role)).yellow().to_string(),
            "assistant" => style(format!("{:<9}", role)).green().to_string(),
            "user" => style(format!("{:<9}", role)).blue().to_string(),
            _ => format!("{:<9}", role),
        };

        println!(
            "  {} {} {}",
            style(format!("[{agent}]")).cyan().bold(),
            role_styled,
            if args.raw { content.to_string() } else { content.replace('\n', "\n                          ") },
        );
        println!();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

async fn exec_extract(args: &ExtractArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Extracting KB entries from conversation {}",
        style("[log]").cyan().bold(),
        style(&args.conversation_id).bold(),
    );

    // In production: use daf-memory to extract and store KB entries from
    // the conversation transcript.
    let entries = [
        ("finding", "SQL injection risk in auth.rs:142 — parameterize queries"),
        ("metric", "Test coverage: 78.3% (47 tests, 42 unit + 5 integration)"),
        ("decision", "PR #42 requires fix before merge: SQL injection in auth module"),
    ];

    eprintln!();
    let mut table = format_table(&["", "Kind", "Content"]);
    for (kind, content) in &entries {
        table.add_row(vec![status_icon("ok"), kind.to_string(), content.to_string()]);
    }
    println!("{table}");

    eprintln!(
        "\n{} Extracted {} KB entries",
        style("\u{2714}").green().bold(),
        entries.len(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// episodes
// ---------------------------------------------------------------------------

async fn exec_episodes(args: &EpisodesArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Recorded episodes:\n",
        style("[log]").cyan().bold(),
    );

    // Placeholder — in production these come from daf-logger's episode store.
    let mut table = format_table(&[
        "", "Episode ID", "Mission", "Agents", "Duration", "Outcome", "Recorded",
    ]);

    let episodes = [
        ("ep-001", "review-pr-42", "3", "2m 14s", "completed", "2026-04-07 10:15"),
        ("ep-002", "deploy-staging", "5", "8m 03s", "completed", "2026-04-07 09:42"),
        ("ep-003", "security-scan", "2", "1m 47s", "failed", "2026-04-06 16:30"),
    ];

    for (id, mission, agents, duration, outcome, recorded) in &episodes {
        if let Some(ref filter_agent) = args.agent {
            // In a real implementation we would filter by participating agents.
            let _ = filter_agent;
        }

        table.add_row(vec![
            status_icon(outcome),
            id.to_string(),
            mission.to_string(),
            agents.to_string(),
            duration.to_string(),
            crate::display::colorize_status(outcome),
            recorded.to_string(),
        ]);
    }

    println!("{table}");

    Ok(())
}
