//! # DAF CLI
//!
//! The `daf` command — primary interface for managing agent clusters, running
//! missions, inspecting logs, and provisioning topologies.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{fmt, EnvFilter};

use daf::{Cli, Command};
use daf::display::banner;

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        config,
        verbose,
        quiet,
        format,
        command,
    } = Cli::parse();

    // ---------- tracing --------------------------------------------------
    let filter = match (quiet, verbose) {
        (true, _) => "error",
        (_, 0) => "info",
        (_, 1) => "debug",
        _ => "trace",
    };

    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter)),
        )
        .with_target(false)
        .init();

    // ---------- banner ---------------------------------------------------
    if !quiet {
        banner();
    }

    // Reconstruct a Cli with a dummy command so exec functions can read
    // global flags (config, verbose, quiet, format). The real command
    // has been moved out for dispatch below.
    let cli = Cli {
        config,
        verbose,
        quiet,
        format,
        command: Command::Status(daf::commands::status::StatusArgs { extended: false }),
    };

    // ---------- dispatch -------------------------------------------------
    match &command {
        Command::Init(args) => daf::commands::init::exec(args, &cli).await,
        Command::Run(args) => daf::commands::run::exec(args, &cli).await,
        Command::Agent(cmd) => daf::commands::agent::exec(cmd, &cli).await,
        Command::Provision(args) => daf::commands::provision::exec(args, &cli).await,
        Command::Configure(args) => daf::commands::configure::exec(args, &cli).await,
        Command::Log(cmd) => daf::commands::log::exec(cmd, &cli).await,
        Command::Vault(cmd) => daf::commands::vault::exec(cmd, &cli).await,
        Command::Status(args) => daf::commands::status::exec(args, &cli).await,
    }
}
