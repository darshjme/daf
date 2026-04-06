//! # daf-core — Foundation types for the DAF Agent Framework
//!
//! This crate defines the core types, traits, and primitives that every
//! other crate in the DAF workspace depends on. It is intentionally
//! dependency-light and focuses on correctness, documentation, and
//! stability over feature richness.
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`agent`] | Agent identity, lifecycle, capabilities, and the [`Agent`](agent::Agent) trait |
//! | [`message`] | Inter-agent message types, builder, and envelope routing |
//! | [`task`] | Task specifications, state machine, results, and handles |
//! | [`resource`] | Resource budgets, pools, and RAII guards |
//! | [`error`] | Unified error types and the [`DafResult`](error::DafResult) alias |
//! | [`config`] | Runtime configuration structs with serde support |
//! | [`identity`] | Cryptographic identity: Ed25519 keys, blake3 node IDs, signed payloads |
//! | [`event`] | Event bus with publish/subscribe and filtering |
//!
//! ## Design Principles
//!
//! 1. **Zero-copy where possible** — binary payloads use [`bytes::Bytes`]
//!    for reference-counted, zero-copy slicing.
//! 2. **Time-ordered identifiers** — all IDs use UUID v7 so they sort
//!    chronologically without an external sequence generator.
//! 3. **Typed errors** — [`DafError`](error::DafError) covers every failure
//!    mode with enough context for actionable diagnostics.
//! 4. **Trait-based extension** — the [`Agent`](agent::Agent) and
//!    [`EventBus`](event::EventBus) traits allow plugging in custom
//!    implementations without touching core code.
//! 5. **Serde everywhere** — every public struct derives `Serialize` and
//!    `Deserialize` for config files, wire protocols, and persistence.

pub mod agent;
pub mod config;
pub mod error;
pub mod event;
pub mod identity;
pub mod message;
pub mod resource;
pub mod task;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use agent::{
    Agent, AgentCapability, AgentContext, AgentId, AgentKind, AgentManifest, AgentStatus,
    ResourceLimits,
};
pub use config::DafConfig;
pub use error::{DafError, DafResult, ErrorContext};
pub use event::{Event, EventBus, EventFilter, EventKind};
pub use identity::{Fingerprint, KeyPair, NodeId, SignedPayload};
pub use message::{Envelope, Message, MessageId, MessageKind, Priority};
pub use resource::{ResourceGuard, ResourceKind, ResourceLimit, ResourcePool};
pub use task::{TaskHandle, TaskId, TaskPriority, TaskResult, TaskSpec, TaskState};
