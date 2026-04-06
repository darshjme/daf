//! `daf configure` — Run configuration playbooks against agents.
//!
//! Executes declarative playbooks that apply configuration changes to one or
//! more agents, similar to Ansible's push-based model. Supports dry-run
//! validation and module listing.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::display::{colorize_status, format_duration, format_table, status_icon};
use crate::Cli;

/// Arguments for `daf configure`.
#[derive(Debug, clap::Args)]
pub struct ConfigureArgs {
    #[command(subcommand)]
    pub action: ConfigureAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum ConfigureAction {
    /// Execute a playbook against agents.
    Run(RunPlaybookArgs),
    /// Dry-run: validate a playbook without applying changes.
    Check(CheckPlaybookArgs),
    /// List available configuration modules.
    ListModules,
}

#[derive(Debug, clap::Args)]
pub struct RunPlaybookArgs {
    /// Path to the playbook YAML file.
    pub playbook: PathBuf,

    /// Limit execution to specific agent names (comma-separated).
    #[arg(short, long)]
    pub limit: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct CheckPlaybookArgs {
    /// Path to the playbook YAML file.
    pub playbook: PathBuf,
}

/// Playbook file structure.
#[derive(Debug, Deserialize)]
struct PlaybookFile {
    playbook: PlaybookSpec,
}

#[derive(Debug, Deserialize)]
struct PlaybookSpec {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tasks: Vec<PlaybookTask>,
}

#[derive(Debug, Deserialize)]
struct PlaybookTask {
    name: String,
    module: String,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    params: serde_json::Value,
}

/// Result of a single playbook task.
struct TaskOutcome {
    play: String,
    task: String,
    agent: String,
    status: String,
    changed: bool,
    duration: Duration,
}

fn load_playbook(path: &PathBuf) -> Result<PlaybookSpec> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read playbook: {}", path.display()))?;
    let val: serde_json::Value =
        serde_yaml_ng::from_str(&raw).with_context(|| "invalid YAML in playbook")?;
    let file: PlaybookFile =
        serde_json::from_value(val).with_context(|| "playbook does not match expected schema")?;
    Ok(file.playbook)
}

/// Dispatch configure subcommands.
pub async fn exec(args: &ConfigureArgs, cli: &Cli) -> Result<()> {
    match &args.action {
        ConfigureAction::Run(a) => exec_run(a, cli).await,
        ConfigureAction::Check(a) => exec_check(a, cli).await,
        ConfigureAction::ListModules => exec_list_modules(cli).await,
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

async fn exec_run(args: &RunPlaybookArgs, _cli: &Cli) -> Result<()> {
    let playbook = load_playbook(&args.playbook)?;

    eprintln!(
        "{} PLAY [{}]",
        style("[configure]").cyan().bold(),
        style(&playbook.name).bold(),
    );
    if !playbook.description.is_empty() {
        eprintln!("  {}", style(&playbook.description).dim());
    }
    eprintln!();

    let start = Instant::now();
    let mut outcomes: Vec<TaskOutcome> = Vec::new();
    let limit: Option<Vec<&str>> = args.limit.as_deref().map(|l| l.split(',').collect());

    for task in &playbook.tasks {
        let targets: Vec<String> = if task.targets.is_empty() || task.targets.contains(&"all".to_string()) {
            vec!["all-agents".into()]
        } else {
            task.targets.clone()
        };

        for target in &targets {
            // Apply --limit filter.
            if let Some(ref lim) = limit {
                if !lim.iter().any(|l| target.contains(l)) && target != "all-agents" {
                    continue;
                }
            }

            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template("  {spinner:.cyan} TASK [{prefix}] {msg}")?
                    .tick_strings(&["\u{25CB}", "\u{25D4}", "\u{25D1}", "\u{25D5}", "\u{25CF}"]),
            );
            pb.set_prefix(task.name.clone());
            pb.set_message(format!(
                "{} on {}",
                style(&task.module).dim(),
                style(target).cyan(),
            ));
            pb.enable_steady_tick(Duration::from_millis(120));

            let task_start = Instant::now();

            // In production: dispatch to daf-configure which applies the
            // module against the target agent via daf-transport.
            tokio::time::sleep(Duration::from_millis(100)).await;

            let elapsed = task_start.elapsed();
            let status = "ok";
            let changed = task.module != "health";

            pb.finish_with_message(format!(
                "{} {} on {} ({})",
                status_icon(if changed { "changed" } else { status }),
                colorize_status(if changed { "changed" } else { "ok" }),
                style(target).cyan(),
                format_duration(elapsed),
            ));

            outcomes.push(TaskOutcome {
                play: playbook.name.clone(),
                task: task.name.clone(),
                agent: target.clone(),
                status: if changed { "changed".into() } else { "ok".into() },
                changed,
                duration: elapsed,
            });
        }
    }

    // ---- recap ---------------------------------------------------------
    let total = start.elapsed();
    let ok_count = outcomes.iter().filter(|o| o.status == "ok").count();
    let changed_count = outcomes.iter().filter(|o| o.changed).count();
    let failed_count = outcomes.iter().filter(|o| o.status == "failed").count();

    eprintln!(
        "\n{} PLAY RECAP {}",
        style("[configure]").cyan().bold(),
        style(format!("({})", format_duration(total))).dim(),
    );

    let mut recap = format_table(&["", "Play", "Task", "Agent", "Status", "Duration"]);
    for o in &outcomes {
        recap.add_row(vec![
            &status_icon(&o.status),
            &o.play,
            &o.task,
            &o.agent,
            &colorize_status(&o.status),
            &format_duration(o.duration),
        ]);
    }
    println!("{recap}");

    eprintln!(
        "\n  ok={} changed={} failed={}",
        style(ok_count).green(),
        style(changed_count).yellow(),
        if failed_count > 0 {
            style(failed_count).red().bold()
        } else {
            style(failed_count).dim()
        },
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// check (dry-run)
// ---------------------------------------------------------------------------

async fn exec_check(args: &CheckPlaybookArgs, _cli: &Cli) -> Result<()> {
    let playbook = load_playbook(&args.playbook)?;

    eprintln!(
        "{} CHECK [{}] (dry-run)\n",
        style("[configure]").cyan().bold(),
        style(&playbook.name).bold(),
    );

    let mut table = format_table(&["#", "Task", "Module", "Targets", "Params"]);
    for (i, task) in playbook.tasks.iter().enumerate() {
        let targets_str = if task.targets.is_empty() {
            "all".to_string()
        } else {
            task.targets.join(", ")
        };
        let params_str = if task.params.is_null() {
            "-".to_string()
        } else {
            serde_json::to_string(&task.params).unwrap_or_else(|_| "-".into())
        };
        table.add_row(vec![
            &(i + 1).to_string(),
            &task.name,
            &task.module,
            &targets_str,
            &params_str,
        ]);
    }
    println!("{table}");

    eprintln!(
        "\n{} Playbook is valid — {} task(s) would be executed",
        style("\u{2714}").green().bold(),
        playbook.tasks.len(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// list-modules
// ---------------------------------------------------------------------------

async fn exec_list_modules(_cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Available configuration modules:\n",
        style("[configure]").cyan().bold(),
    );

    let mut table = format_table(&["Module", "Description", "Idempotent"]);
    let modules = [
        ("config", "Set agent configuration values", "yes"),
        ("health", "Run health checks against agents", "yes"),
        ("capability", "Register or unregister agent capabilities", "yes"),
        ("resource", "Adjust agent resource limits", "yes"),
        ("restart", "Restart an agent process", "no"),
        ("upgrade", "Upgrade agent to a newer manifest version", "no"),
        ("script", "Execute an arbitrary script on the agent", "no"),
        ("env", "Set environment variables for an agent", "yes"),
        ("log-level", "Change the agent's log verbosity at runtime", "yes"),
        ("drain", "Drain an agent's task queue before maintenance", "no"),
    ];

    for (name, desc, idempotent) in &modules {
        table.add_row(vec![name, desc, idempotent]);
    }
    println!("{table}");

    Ok(())
}
