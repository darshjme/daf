//! Error types for the daf-graph crate.

use crate::node::NodeId;
use thiserror::Error;

/// Errors produced by the graph engine.
#[derive(Debug, Error)]
pub enum GraphError {
    /// A node was referenced that does not exist in the graph.
    #[error("node not found: {0}")]
    NodeNotFound(NodeId),

    /// Adding an edge would create a cycle, violating the DAG invariant.
    #[error("cycle detected: adding this edge would create a cycle")]
    CycleDetected,

    /// The graph is empty and cannot be executed.
    #[error("graph is empty")]
    EmptyGraph,

    /// A node exceeded its execution timeout.
    #[error("node {0} timed out after {1:?}")]
    NodeTimeout(NodeId, std::time::Duration),

    /// The entire graph execution exceeded the global timeout.
    #[error("global execution timeout after {0:?}")]
    GlobalTimeout(std::time::Duration),

    /// Execution was cancelled.
    #[error("execution cancelled")]
    Cancelled,

    /// A node failed during execution.
    #[error("node {0} failed: {1}")]
    NodeFailed(NodeId, String),

    /// Serialization or deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An invalid state transition was attempted.
    #[error("invalid state transition for node {0}: {1}")]
    InvalidTransition(NodeId, String),
}
