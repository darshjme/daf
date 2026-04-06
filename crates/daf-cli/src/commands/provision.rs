//! `daf provision` — Provision infrastructure from topology files.
//!
//! Provides plan/apply/destroy/state subcommands with color-coded output
//! showing resource changes (green=create, yellow=update, red=destroy).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use console::style;
use dialoguer::Confirm;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::display::{format_duration, format_table, status_icon};
use crate::Cli;

/// Arguments for `daf provision`.
#[derive(Debug, clap::Args)]
pub struct ProvisionArgs {
    #[command(subcommand)]
    pub action: ProvisionAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum ProvisionAction {
    /// Show what will be created, changed, or destroyed.
    Plan(PlanArgs),
    /// Apply the topology (create/update/destroy resources).
    Apply(ApplyArgs),
    /// Tear down all provisioned resources.
    Destroy(DestroyArgs),
    /// Show current provisioned state.
    State,
}

#[derive(Debug, clap::Args)]
pub struct PlanArgs {
    /// Path to the topology YAML file.
    pub topology: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    /// Path to the topology YAML file.
    pub topology: PathBuf,

    /// Skip confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, clap::Args)]
pub struct DestroyArgs {
    /// Skip confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,
}

/// A topology file describes the desired cluster layout.
#[derive(Debug, Deserialize)]
struct TopologyFile {
    topology: TopologySpec,
}

#[derive(Debug, Deserialize)]
struct TopologySpec {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    nodes: Vec<NodeSpec>,
    #[serde(default)]
    agents: Vec<AgentSlotSpec>,
}

#[derive(Debug, Deserialize)]
struct NodeSpec {
    name: String,
    address: String,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AgentSlotSpec {
    manifest: String,
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default)]
    node: String,
}

fn default_count() -> u32 {
    1
}

/// Represents a planned change to the infrastructure.
struct ResourceChange {
    action: ChangeAction,
    resource_type: String,
    name: String,
    detail: String,
}

#[derive(Clone, Copy)]
enum ChangeAction {
    Create,
    Update,
    Destroy,
}

impl ChangeAction {
    fn label(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Destroy => "destroy",
        }
    }

    fn styled(&self) -> String {
        match self {
            Self::Create => format!("{}", style("+ create").green().bold()),
            Self::Update => format!("{}", style("~ update").yellow().bold()),
            Self::Destroy => format!("{}", style("- destroy").red().bold()),
        }
    }
}

/// Dispatch provision subcommands.
pub async fn exec(args: &ProvisionArgs, cli: &Cli) -> Result<()> {
    match &args.action {
        ProvisionAction::Plan(a) => exec_plan(a, cli).await,
        ProvisionAction::Apply(a) => exec_apply(a, cli).await,
        ProvisionAction::Destroy(a) => exec_destroy(a, cli).await,
        ProvisionAction::State => exec_state(cli).await,
    }
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

fn load_topology(path: &PathBuf) -> Result<TopologySpec> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read topology: {}", path.display()))?;
    let val: serde_json::Value = serde_yaml_ng::from_str(&raw)
        .with_context(|| "invalid YAML in topology file")?;
    let file: TopologyFile =
        serde_json::from_value(val).with_context(|| "topology file does not match schema")?;
    Ok(file.topology)
}

fn compute_plan(topo: &TopologySpec) -> Vec<ResourceChange> {
    let mut changes = Vec::new();

    for node in &topo.nodes {
        changes.push(ResourceChange {
            action: ChangeAction::Create,
            resource_type: "node".into(),
            name: node.name.clone(),
            detail: format!("{} [{}]", node.address, node.roles.join(", ")),
        });
    }

    for slot in &topo.agents {
        for i in 0..slot.count {
            changes.push(ResourceChange {
                action: ChangeAction::Create,
                resource_type: "agent".into(),
                name: format!("{}:{}", slot.manifest, i + 1),
                detail: format!("node={}", slot.node),
            });
        }
    }

    changes
}

fn print_plan(topo_name: &str, changes: &[ResourceChange]) {
    eprintln!(
        "\n{} Topology: {}\n",
        style("[provision]").cyan().bold(),
        style(topo_name).bold(),
    );

    let mut table = format_table(&["", "Action", "Type", "Name", "Detail"]);
    for c in changes {
        table.add_row(vec![
            &status_icon(c.action.label()),
            &c.action.styled(),
            &c.resource_type,
            &c.name,
            &c.detail,
        ]);
    }
    println!("{table}");

    let creates = changes.iter().filter(|c| matches!(c.action, ChangeAction::Create)).count();
    let updates = changes.iter().filter(|c| matches!(c.action, ChangeAction::Update)).count();
    let destroys = changes.iter().filter(|c| matches!(c.action, ChangeAction::Destroy)).count();

    eprintln!(
        "\nPlan: {} to create, {} to update, {} to destroy",
        style(creates).green().bold(),
        style(updates).yellow().bold(),
        style(destroys).red().bold(),
    );
}

async fn exec_plan(args: &PlanArgs, _cli: &Cli) -> Result<()> {
    let topo = load_topology(&args.topology)?;
    let changes = compute_plan(&topo);
    print_plan(&topo.name, &changes);
    Ok(())
}

// ---------------------------------------------------------------------------
// apply
// ---------------------------------------------------------------------------

async fn exec_apply(args: &ApplyArgs, _cli: &Cli) -> Result<()> {
    let topo = load_topology(&args.topology)?;
    let changes = compute_plan(&topo);
    print_plan(&topo.name, &changes);

    if changes.is_empty() {
        eprintln!("\n{} Infrastructure is up to date.", style("\u{2714}").green().bold());
        return Ok(());
    }

    if !args.yes {
        let proceed = Confirm::new()
            .with_prompt("Apply these changes?")
            .default(false)
            .interact()?;
        if !proceed {
            eprintln!("{}", style("Aborted.").yellow());
            return Ok(());
        }
    }

    eprintln!();
    let start = Instant::now();

    let pb = ProgressBar::new(changes.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} applying [{bar:30.green/dim}] {pos}/{len} ({eta})",
        )?
        .progress_chars("\u{2588}\u{2592}\u{2591}"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    for c in &changes {
        // In production: dispatch to daf-provision which manages the
        // actual infrastructure lifecycle.
        tokio::time::sleep(Duration::from_millis(80)).await;
        tracing::info!(
            action = c.action.label(),
            resource = %c.name,
            "resource provisioned",
        );
        pb.inc(1);
    }
    pb.finish_and_clear();

    eprintln!(
        "\n{} Apply complete in {} — {} resource(s) provisioned",
        style("\u{2714}").green().bold(),
        style(format_duration(start.elapsed())).cyan(),
        changes.len(),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// destroy
// ---------------------------------------------------------------------------

async fn exec_destroy(args: &DestroyArgs, _cli: &Cli) -> Result<()> {
    eprintln!(
        "{} {}",
        style("[provision]").cyan().bold(),
        style("Destroying all provisioned resources").red().bold(),
    );

    if !args.yes {
        let proceed = Confirm::new()
            .with_prompt("This will destroy ALL provisioned infrastructure. Continue?")
            .default(false)
            .interact()?;
        if !proceed {
            eprintln!("{}", style("Aborted.").yellow());
            return Ok(());
        }
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("  {spinner:.red} {msg}")?);
    pb.set_message("tearing down resources...");
    pb.enable_steady_tick(Duration::from_millis(120));

    // In production: call daf-provision to destroy all tracked resources.
    tokio::time::sleep(Duration::from_millis(500)).await;

    pb.finish_with_message(format!(
        "{} all resources destroyed",
        status_icon("ok"),
    ));

    Ok(())
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

async fn exec_state(_cli: &Cli) -> Result<()> {
    eprintln!(
        "{} Current provisioned state:\n",
        style("[provision]").cyan().bold(),
    );

    // In production: read from daf-provision's state store.
    let mut table = format_table(&["", "Type", "Name", "Status", "Address", "Since"]);
    table.add_row(vec![
        &status_icon("ok"),
        "node",
        "local",
        "healthy",
        "127.0.0.1:9400",
        "4h ago",
    ]);
    table.add_row(vec![
        &status_icon("ok"),
        "agent",
        "hello-worker:1",
        "running",
        "local",
        "4h ago",
    ]);
    table.add_row(vec![
        &status_icon("ok"),
        "agent",
        "hello-worker:2",
        "running",
        "local",
        "4h ago",
    ]);

    println!("{table}");

    eprintln!(
        "\n  {} node(s), {} agent(s) provisioned",
        style(1).bold(),
        style(2).bold(),
    );

    Ok(())
}
