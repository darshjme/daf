//! # DAF Runtime
//!
//! The executable heart of the Darshj's Agent Framework. This crate boots the
//! entire system — initializing transport, memory, registry, logger, and
//! orchestrator subsystems — then keeps everything running until a graceful
//! shutdown signal arrives.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use daf_runtime::{RuntimeConfig, bootstrap};
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = RuntimeConfig::development();
//!     let runtime = bootstrap(config).await.expect("bootstrap failed");
//!     runtime.start().await.expect("runtime failed");
//! }
//! ```

pub mod bootstrap;
pub mod config;
pub mod health;
pub mod metrics;
pub mod node;
pub mod runtime;
pub mod signal;

// Re-export the most-used types at crate root.
pub use bootstrap::bootstrap;
pub use config::{
    LogConfig, MemoryConfig, OrchestratorConfig, RegistryConfig, RuntimeConfig, TransportConfig,
};
pub use health::{HealthStatus, SubsystemHealth, SubsystemKind, SystemHealth};
pub use metrics::RuntimeMetrics;
pub use node::{Node, NodeInfo, PeerStatus};
pub use runtime::{Runtime, RuntimeBuilder, RuntimeState};
pub use signal::ShutdownSignal;

/// Crate version, pulled from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Human-readable crate name.
pub const NAME: &str = "daf-runtime";
