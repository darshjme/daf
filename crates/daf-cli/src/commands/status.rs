//! `daf status` — Cluster health dashboard.
//!
//! Live state is unavailable until a runtime backend is connected.

use crate::Cli;
use anyhow::Result;

/// Arguments for `daf status`.
#[derive(Debug, clap::Args)]
pub struct StatusArgs {
    /// Show extended details (per-agent breakdown).
    #[arg(short, long)]
    pub extended: bool,
}

/// Execute the `status` command.
pub async fn exec(_args: &StatusArgs, _cli: &Cli) -> Result<()> {
    super::unavailable("status")
}
