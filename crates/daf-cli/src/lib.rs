//! # DAF CLI
//!
//! Library root for the `daf` command-line binary. The actual entry point
//! lives in `main.rs`; this module re-exports shared utilities so they can
//! be used in integration tests.

pub mod commands;
pub mod display;

use clap::{Parser, Subcommand, ValueEnum};

/// Output format for command results.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    /// Pretty tables with color (default).
    Table,
    /// Machine-readable JSON.
    Json,
    /// Plain text, one record per line.
    Text,
}

/// DAF — Darshj's Agent Framework.
///
/// Orchestrate, provision, and observe AI agent clusters from a single CLI.
#[derive(Debug, Parser)]
#[command(
    name = "daf",
    version,
    about = "Darshj's Agent Framework — orchestrate AI agents at scale",
    long_about = None,
    propagate_version = true,
    arg_required_else_help = true,
)]
pub struct Cli {
    /// Path to a DAF configuration file (default: ./daf.yml).
    #[arg(long, global = true, default_value = "daf.yml")]
    pub config: String,

    /// Enable verbose logging (repeat for trace: -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress all output except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize a new DAF project directory.
    Init(commands::init::InitArgs),

    /// Run a mission from a YAML file.
    Run(commands::run::RunArgs),

    /// Manage agents (list, spawn, inspect, terminate, logs).
    #[command(subcommand)]
    Agent(commands::agent::AgentCommand),

    /// Provision infrastructure from a topology file.
    Provision(commands::provision::ProvisionArgs),

    /// Run configuration playbooks against agents.
    Configure(commands::configure::ConfigureArgs),

    /// Query and stream agent logs.
    #[command(subcommand)]
    Log(commands::log::LogCommand),

    /// Manage the encrypted secrets vault.
    #[command(subcommand)]
    Vault(commands::vault::VaultCommand),

    /// Show cluster status dashboard.
    Status(commands::status::StatusArgs),
}
