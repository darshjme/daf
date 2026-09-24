//! Graph node types for the DAF execution DAG.
//!
//! Each node in the execution graph represents a unit of work (task, gate,
//! fork/join synchronization point, checkpoint, or no-op). Nodes carry their
//! own state machine and optional task specification from `daf-core`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

/// Unique identifier for a graph node, backed by UUID v7 (time-ordered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub Uuid);

impl NodeId {
    /// Create a new time-ordered node identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// NodeKind
// ---------------------------------------------------------------------------

/// The type of work a node represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    /// A concrete task to execute (e.g., run an agent, call an API).
    Task,
    /// A conditional gate — downstream nodes only proceed if the gate passes.
    Gate,
    /// A fork point — splits execution into parallel branches.
    Fork,
    /// A join point — waits for all incoming branches to complete.
    Join,
    /// A checkpoint — persists graph state for resumption after failure.
    Checkpoint,
    /// A no-op placeholder used for graph structuring.
    Noop,
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task => write!(f, "Task"),
            Self::Gate => write!(f, "Gate"),
            Self::Fork => write!(f, "Fork"),
            Self::Join => write!(f, "Join"),
            Self::Checkpoint => write!(f, "Checkpoint"),
            Self::Noop => write!(f, "Noop"),
        }
    }
}

// ---------------------------------------------------------------------------
// NodeState
// ---------------------------------------------------------------------------

/// Lifecycle state of a node in the execution graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    /// Not yet eligible for execution — dependencies not satisfied.
    Pending,
    /// All dependencies satisfied, waiting to be scheduled.
    Ready,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Succeeded,
    /// Execution failed.
    Failed,
    /// Skipped (e.g., downstream of a failed node, or conditional edge not taken).
    Skipped,
    /// Cancelled by the user or orchestrator.
    Cancelled,
}

impl NodeState {
    /// Returns `true` if this is a terminal state (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }

    /// Returns `true` if the node completed successfully.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

impl fmt::Display for NodeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Ready => write!(f, "Ready"),
            Self::Running => write!(f, "Running"),
            Self::Succeeded => write!(f, "Succeeded"),
            Self::Failed => write!(f, "Failed"),
            Self::Skipped => write!(f, "Skipped"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskSpec
// ---------------------------------------------------------------------------

/// Specification for a task to be executed by a node.
///
/// This is a self-contained description that the executor can interpret.
/// It intentionally avoids coupling to any specific agent runtime — the
/// executor maps these fields to whatever backend handles the work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// The type of task (e.g., `"agent.run"`, `"tool.invoke"`, `"http.request"`).
    pub task_type: String,
    /// Serialized parameters for the task.
    pub params: serde_json::Value,
    /// Maximum duration of each handler attempt; timeout drops the handler future.
    /// The shorter of this limit and the executor timeout applies.
    pub timeout: Option<Duration>,
    /// Additional attempts after handler error or timeout (0 = no retries).
    /// Cancellation is never retried. Handlers must tolerate repeated execution.
    pub max_retries: u32,
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// A single node in the execution DAG.
///
/// Combines identity, kind, state, task specification, dependency tracking,
/// and timing information into one cohesive structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Unique identifier for this node.
    pub id: NodeId,
    /// What kind of node this is.
    pub kind: NodeKind,
    /// Human-readable name (e.g., `"fetch-user-data"`, `"summarize-results"`).
    pub name: String,
    /// Current lifecycle state.
    pub state: NodeState,
    /// Optional task specification — only meaningful for `NodeKind::Task`.
    pub task_spec: Option<TaskSpec>,
    /// Set of node IDs this node depends on (must complete before this runs).
    pub dependencies: HashSet<NodeId>,
    /// Free-form metadata (tags, labels, routing hints).
    pub metadata: serde_json::Value,
    /// Estimated duration for scheduling heuristics.
    pub estimated_duration: Option<Duration>,
    /// Actual duration recorded after execution completes.
    pub actual_duration: Option<Duration>,
    /// Priority weight — higher values are scheduled first within a wave.
    pub priority: i32,
}

impl Node {
    /// Create a new node with the given kind and name. Starts in `Pending` state.
    pub fn new(kind: NodeKind, name: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(),
            kind,
            name: name.into(),
            state: NodeState::Pending,
            task_spec: None,
            dependencies: HashSet::new(),
            metadata: serde_json::Value::Null,
            estimated_duration: None,
            actual_duration: None,
            priority: 0,
        }
    }

    /// Builder: attach a task specification.
    pub fn with_task_spec(mut self, spec: TaskSpec) -> Self {
        self.task_spec = Some(spec);
        self
    }

    /// Builder: set estimated duration.
    pub fn with_estimated_duration(mut self, dur: Duration) -> Self {
        self.estimated_duration = Some(dur);
        self
    }

    /// Builder: set priority.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: attach metadata.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Add a dependency on another node.
    pub fn depends_on(&mut self, dep: NodeId) {
        self.dependencies.insert(dep);
    }

    /// Check if all dependencies are in terminal-success states given a lookup fn.
    pub fn dependencies_satisfied(&self, is_done: impl Fn(&NodeId) -> bool) -> bool {
        self.dependencies.iter().all(|dep| is_done(dep))
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {} ({}) — {}", self.id, self.name, self.kind, self.state)
    }
}

// ---------------------------------------------------------------------------
// NodeHandle
// ---------------------------------------------------------------------------

/// Lightweight external reference to a node, suitable for passing across
/// API boundaries without cloning the full `Node`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeHandle {
    /// The node's unique identifier.
    pub id: NodeId,
    /// The node's human-readable name (snapshot at handle creation time).
    pub name: String,
    /// The node's kind.
    pub kind: NodeKind,
}

impl NodeHandle {
    /// Create a handle from a full node.
    pub fn from_node(node: &Node) -> Self {
        Self {
            id: node.id,
            name: node.name.clone(),
            kind: node.kind.clone(),
        }
    }
}

impl fmt::Display for NodeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.name, self.id)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_uniqueness() {
        let a = NodeId::new();
        let b = NodeId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn node_starts_pending() {
        let node = Node::new(NodeKind::Task, "test-task");
        assert_eq!(node.state, NodeState::Pending);
        assert!(node.dependencies.is_empty());
    }

    #[test]
    fn dependencies_satisfied_empty() {
        let node = Node::new(NodeKind::Task, "no-deps");
        assert!(node.dependencies_satisfied(|_| false));
    }

    #[test]
    fn dependencies_satisfied_check() {
        let dep_id = NodeId::new();
        let mut node = Node::new(NodeKind::Task, "has-deps");
        node.depends_on(dep_id);

        assert!(!node.dependencies_satisfied(|_| false));
        assert!(node.dependencies_satisfied(|id| *id == dep_id));
    }

    #[test]
    fn terminal_states() {
        assert!(NodeState::Succeeded.is_terminal());
        assert!(NodeState::Failed.is_terminal());
        assert!(NodeState::Skipped.is_terminal());
        assert!(NodeState::Cancelled.is_terminal());
        assert!(!NodeState::Pending.is_terminal());
        assert!(!NodeState::Ready.is_terminal());
        assert!(!NodeState::Running.is_terminal());
    }

    #[test]
    fn node_builder_chain() {
        let node = Node::new(NodeKind::Task, "builder-test")
            .with_priority(10)
            .with_estimated_duration(Duration::from_secs(30))
            .with_metadata(serde_json::json!({"team": "infra"}));

        assert_eq!(node.priority, 10);
        assert_eq!(node.estimated_duration, Some(Duration::from_secs(30)));
        assert_eq!(node.metadata["team"], "infra");
    }

    #[test]
    fn node_handle_from_node() {
        let node = Node::new(NodeKind::Gate, "my-gate");
        let handle = NodeHandle::from_node(&node);
        assert_eq!(handle.id, node.id);
        assert_eq!(handle.name, "my-gate");
        assert_eq!(handle.kind, NodeKind::Gate);
    }

    #[test]
    fn node_display() {
        let node = Node::new(NodeKind::Fork, "split-work");
        let display = format!("{node}");
        assert!(display.contains("split-work"));
        assert!(display.contains("Fork"));
        assert!(display.contains("Pending"));
    }
}
