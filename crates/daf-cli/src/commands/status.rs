//! `daf status` — Cluster health dashboard.
//!
//! Shows a summary of cluster state: overall health, agent counts by kind,
//! active missions, memory usage, uptime, and version info.

use anyhow::Result;
use console::style;

use crate::display::{colorize_status, format_bytes, format_duration, format_table, kv, section, status_icon};
use crate::Cli;

/// Arguments for `daf status`.
#[derive(Debug, clap::Args)]
pub struct StatusArgs {
    /// Show extended details (per-agent breakdown).
    #[arg(short, long)]
    pub extended: bool,
}

/// Execute the `status` command.
pub async fn exec(args: &StatusArgs, _cli: &Cli) -> Result<()> {
    // In production: query daf-runtime, daf-registry, daf-orchestrator,
    // and daf-memory for live cluster state.

    // ---- header --------------------------------------------------------
    let overall_health = "healthy";
    eprintln!(
        "{} Cluster Status: {} {}\n",
        style("[status]").cyan().bold(),
        status_icon(overall_health),
        colorize_status(overall_health),
    );

    // ---- version & uptime ----------------------------------------------
    section("System");
    kv("DAF version", env!("CARGO_PKG_VERSION"));
    kv("Rust edition", "2024");
    kv("Config", "daf.yml");
    kv("Uptime", &format_duration(std::time::Duration::from_secs(4 * 3600 + 51 * 60 + 23)));
    kv("Transport", "tcp://127.0.0.1:9400");

    // ---- agent summary -------------------------------------------------
    section("Agents");

    let mut agent_table = format_table(&["Kind", "Total", "Active", "Idle", "Failed"]);
    let agent_stats: Vec<(&str, u32, u32, u32, u32)> = vec![
        ("orchestrator", 1, 1, 0, 0),
        ("specialist", 3, 2, 1, 0),
        ("worker", 8, 5, 3, 0),
        ("monitor", 2, 2, 0, 0),
        ("router", 1, 1, 0, 0),
    ];

    let mut total_agents = 0u32;
    let mut total_active = 0u32;

    for (kind, total, active, idle, failed) in &agent_stats {
        total_agents += total;
        total_active += active;

        let failed_str = if *failed > 0 {
            format!("{}", style(failed).red().bold())
        } else {
            format!("{}", style(failed).dim())
        };

        agent_table.add_row(vec![
            kind.to_string(),
            total.to_string(),
            format!("{}", style(active).green()),
            format!("{}", style(idle).dim()),
            failed_str.clone(),
        ]);
    }
    println!("{agent_table}");

    eprintln!(
        "  {} total agents, {} active\n",
        style(total_agents).bold(),
        style(total_active).green().bold(),
    );

    // ---- missions & tasks ----------------------------------------------
    section("Missions");

    let mut mission_table = format_table(&["", "Mission", "Tasks", "Progress", "Started"]);
    mission_table.add_row(vec![
        status_icon("running"),
        "review-pr-42".to_string(),
        "2/3".to_string(),
        "67%".to_string(),
        "12m ago".to_string(),
    ]);
    mission_table.add_row(vec![
        status_icon("completed"),
        "deploy-staging".to_string(),
        "5/5".to_string(),
        "100%".to_string(),
        "1h ago".to_string(),
    ]);
    println!("{mission_table}");

    // ---- memory --------------------------------------------------------
    section("Memory");
    kv("KB entries", "1,847");
    kv("Vector index", &format!("{} (all-mpnet-base-v2)", format_bytes(256 * 1024 * 1024)));
    kv("Conversation log", &format_bytes(128 * 1024 * 1024));
    kv("Episode store", &format!("{} ({} episodes)", format_bytes(64 * 1024 * 1024), 42));

    // ---- vault ---------------------------------------------------------
    section("Vault");
    kv("Status", &colorize_status("ok"));
    kv("Secrets", "4 stored");
    kv("Last rotation", "2026-04-05 14:22 UTC");

    // ---- extended: per-agent detail ------------------------------------
    if args.extended {
        section("Agent Details");

        let mut detail_table = format_table(&[
            "", "ID", "Name", "Kind", "Status", "Uptime", "Memory", "Tasks",
        ]);

        let agents = [
            ("ok", "c9d0e1f2", "orchestrator-1", "orchestrator", "executing", "4h 51m", "87 MiB", "1"),
            ("ok", "a1b2c3d4", "code-reviewer", "specialist", "executing", "2h 14m", "128 MiB", "1"),
            ("ok", "e5f6a7b8", "test-runner", "worker", "idle", "1h 03m", "64 MiB", "0"),
            ("ok", "f3a4b5c6", "security-scanner", "specialist", "idle", "3h 22m", "96 MiB", "0"),
        ];

        for (status, id, name, kind, agent_status, uptime, memory, tasks) in &agents {
            detail_table.add_row(vec![
                status_icon(status),
                id.to_string(),
                name.to_string(),
                kind.to_string(),
                colorize_status(agent_status),
                uptime.to_string(),
                memory.to_string(),
                tasks.to_string(),
            ]);
        }
        println!("{detail_table}");
    }

    eprintln!();
    Ok(())
}
