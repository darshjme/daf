//! CLI subcommand modules.
//!
//! Each module implements one top-level `daf` subcommand and exposes:
//! - An `Args` or `Command` struct for clap parsing.
//! - An `exec` function that performs the work.

pub mod agent;
pub mod configure;
pub mod init;
pub mod log;
pub mod provision;
pub mod run;
pub mod status;
pub mod vault;

/// No synthetic success for commands without a runtime backend.
pub(crate) fn unavailable(operation: &str) -> anyhow::Result<()> {
    anyhow::bail!(
        "daf {operation} requires a connected cluster backend, which this build does not provide. Local commands are available through daf run; persistent secrets through daf vault."
    )
}
