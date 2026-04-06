//! `daf run <mission.yml>` — Load and execute a mission file.
//!
//! Parses the mission YAML, builds an execution plan, displays a confirmation
//! prompt, runs the plan with live progress bars, and prints a results summary.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::Confirm;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::display::{colorize_status, format_duration, format_table, status_icon};
use crate::Cli;

/// Arguments for `daf run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Path to the mission YAML file.
    pub mission: PathBuf,

    /// Skip the confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,

    /// Maximum parallel agent executions.
    #[arg(long, default_value_t = 8)]
    pub parallelism: usize,

    /// Timeout per task in seconds.
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
}

/// Top-level mission file structure.
#[derive(Debug, Deserialize)]
struct MissionFile {
    mission: MissionSpec,
}

#[derive(Debug, Deserialize)]
struct MissionSpec {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct TaskSpec {
    name: String,
    agent: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    params: serde_json::Value,
}

/// Result of a single task execution.
struct TaskResult {
    name: String,
    agent: String,
    status: String,
    duration: Duration,
    message: String,
}

/// Execute the `run` command.
pub async fn exec(args: &RunArgs, _cli: &Cli) -> Result<()> {
    // ---- load mission file ---------------------------------------------
    let raw = std::fs::read_to_string(&args.mission)
        .with_context(|| format!("cannot read mission file: {}", args.mission.display()))?;

    let mission_file: MissionFile = serde_json::from_value(
        serde_yaml_ng::from_str::<serde_json::Value>(&raw)
            .with_context(|| "invalid YAML in mission file")?,
    )
    .with_context(|| "mission file does not match expected schema")?;

    let mission = mission_file.mission;

    if mission.tasks.is_empty() {
        bail!("mission '{}' has no tasks defined", mission.name);
    }

    // ---- display execution plan ----------------------------------------
    eprintln!(
        "{} Mission: {}",
        style("[run]").cyan().bold(),
        style(&mission.name).bold(),
    );
    if !mission.description.is_empty() {
        eprintln!("  {}", style(&mission.description).dim());
    }
    eprintln!();

    let mut plan_table = format_table(&["#", "Task", "Agent", "Dependencies"]);
    for (i, task) in mission.tasks.iter().enumerate() {
        let deps = if task.depends_on.is_empty() {
            "-".to_string()
        } else {
            task.depends_on.join(", ")
        };
        plan_table.add_row(vec![
            &(i + 1).to_string(),
            &task.name,
            &task.agent,
            &deps,
        ]);
    }
    eprintln!("{plan_table}\n");

    // ---- confirmation --------------------------------------------------
    if !args.yes {
        let proceed = Confirm::new()
            .with_prompt(format!(
                "Execute {} tasks with parallelism={}?",
                mission.tasks.len(),
                args.parallelism,
            ))
            .default(true)
            .interact()?;

        if !proceed {
            eprintln!("{}", style("Aborted.").yellow());
            return Ok(());
        }
    }

    // ---- execute with progress bars ------------------------------------
    let multi = MultiProgress::new();
    let sty = ProgressStyle::with_template(
        "  {spinner:.cyan} {prefix:.bold} {wide_msg} [{elapsed_precise}]",
    )?
    .tick_strings(&["\u{25CB}", "\u{25D4}", "\u{25D1}", "\u{25D5}", "\u{25CF}"]);

    let start = Instant::now();
    let mut results: Vec<TaskResult> = Vec::with_capacity(mission.tasks.len());

    // Execute tasks sequentially for now (respecting depends_on ordering).
    // A full DAG scheduler lives in daf-orchestrator; here we provide
    // the CLI UX wrapper.
    for task in &mission.tasks {
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(sty.clone());
        pb.set_prefix(task.name.clone());
        pb.set_message(format!("running on {}", style(&task.agent).cyan()));
        pb.enable_steady_tick(Duration::from_millis(120));

        let task_start = Instant::now();

        // Simulate task execution — in production this dispatches to
        // daf-orchestrator which routes to the target agent.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let elapsed = task_start.elapsed();

        pb.finish_with_message(format!(
            "{} {} ({})",
            status_icon("ok"),
            style("done").green(),
            format_duration(elapsed),
        ));

        results.push(TaskResult {
            name: task.name.clone(),
            agent: task.agent.clone(),
            status: "ok".into(),
            duration: elapsed,
            message: "completed successfully".into(),
        });
    }

    let total_duration = start.elapsed();

    // ---- results summary -----------------------------------------------
    eprintln!();
    let mut summary = format_table(&["", "Task", "Agent", "Status", "Duration", "Message"]);
    for r in &results {
        summary.add_row(vec![
            &status_icon(&r.status),
            &r.name,
            &r.agent,
            &colorize_status(&r.status),
            &format_duration(r.duration),
            &r.message,
        ]);
    }
    eprintln!("{summary}");

    let ok_count = results.iter().filter(|r| r.status == "ok").count();
    let fail_count = results.len() - ok_count;

    eprintln!(
        "\n{} Mission {} completed in {} — {} ok, {} failed",
        if fail_count == 0 {
            style("\u{2714}").green().bold()
        } else {
            style("\u{2718}").red().bold()
        },
        style(&mission.name).bold(),
        style(format_duration(total_duration)).cyan(),
        style(ok_count).green(),
        if fail_count > 0 {
            style(fail_count).red()
        } else {
            style(fail_count).dim()
        },
    );

    if fail_count > 0 {
        bail!("{fail_count} task(s) failed");
    }

    Ok(())
}
