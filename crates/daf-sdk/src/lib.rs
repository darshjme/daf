//! # DAF SDK
//!
//! Developer-facing SDK for building custom DAF agents. This crate provides
//! the ergonomic layer on top of `daf-core`, `daf-ddal`, and the other
//! framework crates, making it as easy as possible to create, test, and
//! deploy agents.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use daf_sdk::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> DafResult<()> {
//!     let agent = AgentBuilder::new("my-worker")
//!         .kind(AgentKind::Worker)
//!         .capability("data_clean", "1.0.0", "Cleans raw data")
//!         .with_config(SdkConfig::development())
//!         .build()?;
//!
//!     agent.start().await?;
//!     // ... agent is running ...
//!     agent.stop().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Architecture
//!
//! ```text
//!  +----------------------------------------------------------+
//!  |                       daf-sdk                            |
//!  |                                                          |
//!  |  prelude    builder    handler    middleware   template   |
//!  |  config     lifecycle  testing    task_types              |
//!  +----------------------------------------------------------+
//!       |           |          |           |           |
//!  +----------+ +--------+ +----------+ +--------+ +--------+
//!  | daf-core | | daf-   | | daf-     | | daf-   | | daf-   |
//!  |          | | ddal   | | transport| | logger | | memory |
//!  +----------+ +--------+ +----------+ +--------+ +--------+
//! ```
//!
//! # Modules
//!
//! - [`prelude`] — One-line import for everything an agent developer needs.
//! - [`builder`] — Fluent [`AgentBuilder`](builder::AgentBuilder) API.
//! - [`handler`] — Message, task, and event handler traits with composition.
//! - [`middleware`] — Cross-cutting concerns: logging, metrics, retry, timeout, auth.
//! - [`lifecycle`] — Agent instance management: start, stop, restart, pause, health.
//! - [`config`] — SDK configuration with env/file/builder loading.
//! - [`template`] — Starter patterns for common agent archetypes.
//! - [`testing`] — Mocks, harnesses, and assertion helpers for agent tests.
//! - [`task_types`] — SDK-level task and event type wrappers.

pub mod builder;
pub mod config;
pub mod handler;
pub mod lifecycle;
pub mod middleware;
pub mod prelude;
pub mod task_types;
pub mod template;
pub mod testing;

// Re-export the builder at crate root for maximum discoverability.
pub use builder::AgentBuilder;
pub use config::SdkConfig;
pub use lifecycle::AgentInstance;
