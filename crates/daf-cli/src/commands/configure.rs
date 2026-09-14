//! `daf configure` — Run configuration playbooks against agents.
//!
//! Static schema inspection is available. Runtime application and module discovery
//! fail explicitly because no execution backend is connected.

use std::path::PathBuf;

use anyhow::{Context, Result};
use console::style;
use serde::Deserialize;

use crate::Cli;
use crate::display::format_table;

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
    /// Parse and display playbook schema; does not validate target state or modules.
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
    #[serde(rename = "description")]
    _description: String,
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
        ConfigureAction::Run(_) => super::unavailable("configure run"),
        ConfigureAction::Check(a) => exec_check(a, cli).await,
        ConfigureAction::ListModules => super::unavailable("configure list-modules"),
    }
}

// ---------------------------------------------------------------------------
// run
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
        "\n{} Playbook schema parsed — {} task(s); module support, target state and execution are not validated",
        style("\u{2714}").green().bold(),
        playbook.tasks.len(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// list-modules
// ---------------------------------------------------------------------------
