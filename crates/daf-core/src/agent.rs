//! Core agent types and the [`Agent`] trait.
//!
//! Every autonomous unit in DAF is an agent. Agents are identified by
//! time-ordered UUIDs, classified by [`AgentKind`], and transition through
//! a well-defined [`AgentStatus`] state machine. The [`Agent`] trait
//! defines the async lifecycle that every concrete agent must implement.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::DafResult;
use crate::message::Message;

// ---------------------------------------------------------------------------
// AgentId
// ---------------------------------------------------------------------------

/// Time-ordered agent identifier backed by UUID v7.
///
/// UUID v7 encodes a millisecond-precision Unix timestamp in the high bits,
/// guaranteeing that identifiers sort chronologically — which is essential
/// for log correlation, sharding, and distributed tracing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(Uuid);

impl AgentId {
    /// Generate a new time-ordered agent identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing [`Uuid`] as an [`AgentId`].
    ///
    /// No version check is performed — this is intentional so that IDs
    /// deserialized from storage or the network are accepted as-is.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Extract the embedded timestamp (works reliably for v7 UUIDs).
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        let ts = self.0.get_timestamp()?;
        let (secs, nanos) = ts.to_unix();
        DateTime::from_timestamp(secs as i64, nanos)
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentId({})", &self.0.to_string()[..8])
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for AgentId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<AgentId> for Uuid {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

// ---------------------------------------------------------------------------
// AgentKind
// ---------------------------------------------------------------------------

/// Classification of an agent's role in the framework.
///
/// The kind determines default resource limits, scheduling priority, and
/// which message channels the agent can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Top-level coordinator that decomposes goals into tasks and delegates
    /// to specialists/workers. There is typically one orchestrator per session.
    Orchestrator,
    /// Domain expert that owns a specific capability (e.g., code generation,
    /// security audit). Specialists receive focused task assignments.
    Specialist,
    /// General-purpose executor that runs tasks as instructed. Workers are
    /// the most numerous agent kind in a typical deployment.
    Worker,
    /// Passive observer that collects metrics, watches health, and emits
    /// alerts. Monitors never mutate application state.
    Monitor,
    /// Message routing agent that directs traffic between other agents
    /// based on content, priority, or load.
    Router,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Orchestrator => write!(f, "orchestrator"),
            Self::Specialist => write!(f, "specialist"),
            Self::Worker => write!(f, "worker"),
            Self::Monitor => write!(f, "monitor"),
            Self::Router => write!(f, "router"),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentStatus
// ---------------------------------------------------------------------------

/// State machine for agent lifecycle.
///
/// ```text
///  Spawning ──▶ Idle ──▶ Executing ──▶ Completed
///                │  ▲        │
///                │  │        ▼
///                │  └─── Waiting
///                │
///                └──────────────────▶ Failed
///                                       │
///               Terminated ◀────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent binary is loading and dependencies are being resolved.
    Spawning,
    /// Agent is alive but has no active task assignment.
    Idle,
    /// Agent is actively processing a task.
    Executing,
    /// Agent yielded execution and is waiting on an external signal
    /// (I/O, another agent's response, timer).
    Waiting,
    /// Agent finished its mission successfully.
    Completed,
    /// Agent encountered an unrecoverable error.
    Failed,
    /// Agent was explicitly shut down (graceful or forced).
    Terminated,
}

impl AgentStatus {
    /// Returns `true` when the agent is in a terminal state and will not
    /// transition again.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Terminated)
    }

    /// Returns `true` when the agent is alive and potentially doing work.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Executing | Self::Waiting)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Spawning => "spawning",
            Self::Idle => "idle",
            Self::Executing => "executing",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Terminated => "terminated",
        };
        write!(f, "{s}")
    }
}

// ---------------------------------------------------------------------------
// AgentCapability
// ---------------------------------------------------------------------------

/// A discrete capability that an agent advertises.
///
/// Capabilities are matched against [`TaskSpec`](crate::task::TaskSpec)
/// requirements during scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Machine-readable name (e.g. `"code_review"`, `"security_scan"`).
    pub name: String,
    /// Semantic version of this capability implementation.
    pub version: String,
    /// Human-readable description.
    pub description: String,
}

impl AgentCapability {
    /// Create a new capability descriptor.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
        }
    }
}

impl fmt::Display for AgentCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

// ---------------------------------------------------------------------------
// ResourceLimits (lightweight, full ResourcePool is in resource.rs)
// ---------------------------------------------------------------------------

/// Per-agent resource ceilings enforced by the runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum heap memory in bytes. `None` means unlimited.
    pub max_memory_bytes: Option<u64>,
    /// Maximum CPU time per task in milliseconds.
    pub max_cpu_ms: Option<u64>,
    /// Maximum concurrent outbound connections.
    pub max_connections: Option<u32>,
    /// Maximum messages the agent may buffer before back-pressure kicks in.
    pub max_message_queue: Option<u32>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: Some(512 * 1024 * 1024), // 512 MiB
            max_cpu_ms: Some(300_000),                  // 5 minutes
            max_connections: Some(64),
            max_message_queue: Some(1024),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentManifest
// ---------------------------------------------------------------------------

/// Complete declarative description of an agent.
///
/// Manifests are the unit of registration: an agent publishes its manifest
/// to the registry so the orchestrator can discover, match, and spawn it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Unique identifier for this agent instance.
    pub id: AgentId,
    /// Role classification.
    pub kind: AgentKind,
    /// Human-friendly name (e.g. `"rust-security-auditor"`).
    pub name: String,
    /// Set of capabilities this agent provides.
    pub capabilities: Vec<AgentCapability>,
    /// Resource ceilings.
    pub resource_limits: ResourceLimits,
    /// Arbitrary key-value metadata (labels, annotations, version tags).
    pub metadata: HashMap<String, String>,
}

impl AgentManifest {
    /// Create a minimal manifest. Use the builder methods to fill in
    /// optional fields.
    pub fn new(kind: AgentKind, name: impl Into<String>) -> Self {
        Self {
            id: AgentId::new(),
            kind,
            name: name.into(),
            capabilities: Vec::new(),
            resource_limits: ResourceLimits::default(),
            metadata: HashMap::new(),
        }
    }

    /// Add a capability.
    pub fn with_capability(mut self, cap: AgentCapability) -> Self {
        self.capabilities.push(cap);
        self
    }

    /// Set resource limits.
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = limits;
        self
    }

    /// Insert a metadata entry.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Returns `true` if the agent advertises the named capability.
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c.name == name)
    }
}

// ---------------------------------------------------------------------------
// AgentContext
// ---------------------------------------------------------------------------

/// Runtime context injected into an agent at initialization.
///
/// Carries identity, lineage, and environment information that the agent
/// needs but should not construct itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    /// This agent's identifier.
    pub agent_id: AgentId,
    /// Session identifier grouping a set of cooperating agents.
    pub session_id: Uuid,
    /// Wall-clock time when the agent was spawned.
    pub spawn_time: DateTime<Utc>,
    /// Identifier of the agent that spawned this one, if any.
    pub parent_id: Option<AgentId>,
    /// Environment variables visible to the agent.
    pub environment: HashMap<String, String>,
    /// The working directory assigned to this agent.
    pub work_dir: Option<std::path::PathBuf>,
}

impl AgentContext {
    /// Create a new context for a root-level agent (no parent).
    pub fn new(agent_id: AgentId, session_id: Uuid) -> Self {
        Self {
            agent_id,
            session_id,
            spawn_time: Utc::now(),
            parent_id: None,
            environment: HashMap::new(),
            work_dir: None,
        }
    }

    /// Create a child context, recording the parent lineage.
    pub fn child(parent: &AgentContext) -> Self {
        Self {
            agent_id: AgentId::new(),
            session_id: parent.session_id,
            spawn_time: Utc::now(),
            parent_id: Some(parent.agent_id),
            environment: parent.environment.clone(),
            work_dir: parent.work_dir.clone(),
        }
    }

    /// Insert an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Set the working directory.
    pub fn with_work_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.work_dir = Some(dir.into());
        self
    }

    /// Duration since the agent was spawned.
    pub fn uptime(&self) -> Duration {
        let now = Utc::now();
        (now - self.spawn_time)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }
}

// ---------------------------------------------------------------------------
// Agent trait
// ---------------------------------------------------------------------------

/// The core lifecycle trait that every DAF agent must implement.
///
/// All methods receive `&self` so that the agent's mutable state lives
/// behind interior mutability (e.g. `tokio::sync::Mutex`, `parking_lot`),
/// which is required for safe concurrent message handling.
#[async_trait::async_trait]
pub trait Agent: Send + Sync + 'static {
    /// One-time initialization called after the agent is spawned but before
    /// it receives any messages. Use this to open connections, load models,
    /// warm caches, etc.
    async fn initialize(&self, ctx: &AgentContext) -> DafResult<()>;

    /// Execute the agent's primary mission. This is the main work loop.
    ///
    /// The runtime calls `execute` exactly once per task assignment. The
    /// agent should return when the task is complete (success or failure).
    async fn execute(&self, ctx: &AgentContext) -> DafResult<serde_json::Value>;

    /// Handle an inbound message from another agent or the runtime.
    ///
    /// Messages can arrive at any time — including while `execute` is
    /// running — so implementations must be safe for concurrent invocation.
    async fn handle_message(&self, ctx: &AgentContext, msg: Message) -> DafResult<()>;

    /// Graceful shutdown hook. The runtime waits up to `timeout` for this
    /// method to return before force-killing the agent.
    async fn shutdown(&self, ctx: &AgentContext, timeout: Duration) -> DafResult<()>;

    /// Health probe. Returns `Ok(())` if the agent is healthy, or an error
    /// describing the degradation.
    async fn health_check(&self) -> DafResult<()>;

    /// Report the capabilities this agent provides.
    fn capabilities(&self) -> Vec<AgentCapability>;

    /// Report current status.
    fn status(&self) -> AgentStatus;

    /// Return the agent's manifest.
    fn manifest(&self) -> &AgentManifest;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_is_time_ordered() {
        let a = AgentId::new();
        // Small delay to ensure different timestamps.
        std::thread::sleep(Duration::from_millis(2));
        let b = AgentId::new();
        // v7 UUIDs sort lexicographically by time.
        assert!(a.as_uuid() < b.as_uuid(), "IDs should be time-ordered");
    }

    #[test]
    fn agent_id_display_is_full_uuid() {
        let id = AgentId::new();
        let display = id.to_string();
        // Full UUID is 36 chars with hyphens.
        assert_eq!(display.len(), 36);
    }

    #[test]
    fn agent_id_debug_is_short() {
        let id = AgentId::new();
        let debug = format!("{id:?}");
        assert!(debug.starts_with("AgentId("));
        // Should be truncated, not the full 36-char UUID.
        assert!(debug.len() < 20);
    }

    #[test]
    fn agent_id_roundtrip_serde() {
        let id = AgentId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn agent_status_terminal() {
        assert!(AgentStatus::Completed.is_terminal());
        assert!(AgentStatus::Failed.is_terminal());
        assert!(AgentStatus::Terminated.is_terminal());
        assert!(!AgentStatus::Executing.is_terminal());
        assert!(!AgentStatus::Idle.is_terminal());
    }

    #[test]
    fn agent_status_active() {
        assert!(AgentStatus::Executing.is_active());
        assert!(AgentStatus::Waiting.is_active());
        assert!(!AgentStatus::Idle.is_active());
    }

    #[test]
    fn manifest_builder() {
        let m = AgentManifest::new(AgentKind::Worker, "test-worker")
            .with_capability(AgentCapability::new("lint", "1.0.0", "Lint code"))
            .with_metadata("team", "platform");

        assert_eq!(m.kind, AgentKind::Worker);
        assert_eq!(m.name, "test-worker");
        assert!(m.has_capability("lint"));
        assert!(!m.has_capability("deploy"));
        assert_eq!(m.metadata.get("team").unwrap(), "platform");
    }

    #[test]
    fn agent_context_child_inherits_session() {
        let parent = AgentContext::new(AgentId::new(), Uuid::now_v7());
        let child = AgentContext::child(&parent);

        assert_eq!(child.session_id, parent.session_id);
        assert_eq!(child.parent_id, Some(parent.agent_id));
        assert_ne!(child.agent_id, parent.agent_id);
    }

    #[test]
    fn capability_display() {
        let cap = AgentCapability::new("code_gen", "2.1.0", "Generate code");
        assert_eq!(cap.to_string(), "code_gen@2.1.0");
    }
}
