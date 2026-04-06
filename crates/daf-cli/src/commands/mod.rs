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
