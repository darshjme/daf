//! # daf-registry — Agent & Plugin Registry
//!
//! The registry crate is the phone book and HR department of the DAF agent
//! cluster. It handles:
//!
//! - **Discovery**: find agents that match capability requirements via semver
//!   constraints and best-match scoring.
//! - **Versioning**: full semantic versioning with caret, tilde, range, and
//!   exact constraints.
//! - **Capability matching**: set-theoretic operations on capability
//!   collections (intersection, union, difference) plus weighted scoring.
//! - **Health monitoring**: heartbeat tracking, consecutive failure detection,
//!   and auto-deregistration of silent agents.
//! - **Persistent catalog**: known agent types with performance statistics,
//!   templates, and JSON import/export.
//! - **Dynamic loading**: load manifests from files or directories with
//!   validation, hot-reload via blake3 checksums.
//!
//! ## Architecture
//!
//! ```text
//!                  ┌──────────────┐
//!                  │   Registry   │  ◄── DashMap<AgentId, RegistryEntry>
//!                  └──────┬───────┘
//!                         │
//!        ┌────────────────┼────────────────┐
//!        ▼                ▼                ▼
//!  ┌───────────┐   ┌────────────┐   ┌───────────┐
//!  │ Capability │   │  Health    │   │  Loader   │
//!  │  Matching  │   │  Monitor  │   │  (files)  │
//!  └───────────┘   └────────────┘   └───────────┘
//!        │                                │
//!        ▼                                ▼
//!  ┌───────────┐                   ┌───────────┐
//!  │  Version  │                   │  Catalog  │
//!  │  (semver) │                   │ (persist) │
//!  └───────────┘                   └───────────┘
//! ```

pub mod capability;
pub mod catalog;
pub mod health;
pub mod loader;
pub mod registry;
pub mod version;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use capability::{
    best_match, score_capabilities, Capability, CapabilityRequirement, CapabilitySet, MatchScore,
};
pub use catalog::{builtin_templates, AgentTemplate, Catalog, CatalogEntry};
pub use health::{
    AgentHealth, ClusterHealth, HealthCheck, HealthConfig, HealthMonitor, HealthStatus,
};
pub use loader::{
    AgentLoader, ChangeDetector, DirectoryLoader, ManifestChange, ManifestFile, ManifestLoader,
};
pub use registry::{AgentFilter, Registry, RegistryEntry};
pub use version::{SemVer, VersionConstraint, VersionParseError};
