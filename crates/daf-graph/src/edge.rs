//! Graph edge types for the DAF execution DAG.
//!
//! Edges encode the relationships between nodes: dependencies, triggers,
//! data flow, blocking constraints, and conditional transitions. Each edge
//! carries a condition that determines whether it should fire based on the
//! source node's outcome.

use crate::node::NodeId;
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// EdgeKind
// ---------------------------------------------------------------------------

/// The semantic relationship an edge encodes between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Target cannot start until source completes (standard dependency).
    DependsOn,
    /// Source completion triggers target to become ready.
    Triggers,
    /// Source blocks target from executing while source is running.
    Blocks,
    /// Data flows from source's output to target's input.
    DataFlow,
    /// Conditional edge — only fires when the attached condition evaluates true.
    Conditional,
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DependsOn => write!(f, "depends_on"),
            Self::Triggers => write!(f, "triggers"),
            Self::Blocks => write!(f, "blocks"),
            Self::DataFlow => write!(f, "data_flow"),
            Self::Conditional => write!(f, "conditional"),
        }
    }
}

// ---------------------------------------------------------------------------
// EdgeCondition
// ---------------------------------------------------------------------------

/// Condition that determines whether an edge should fire after the source
/// node reaches a terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeCondition {
    /// Always fire regardless of source outcome.
    Always,
    /// Only fire if the source node succeeded.
    OnSuccess,
    /// Only fire if the source node failed.
    OnFailure,
    /// Fire if the source node's output contains the specified key.
    /// Used for routing based on output content (e.g., branch on classification).
    OnOutput(String),
}

impl Default for EdgeCondition {
    fn default() -> Self {
        Self::Always
    }
}

impl fmt::Display for EdgeCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Always => write!(f, "always"),
            Self::OnSuccess => write!(f, "on_success"),
            Self::OnFailure => write!(f, "on_failure"),
            Self::OnOutput(key) => write!(f, "on_output({key})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Edge
// ---------------------------------------------------------------------------

/// A directed edge in the execution DAG.
///
/// Connects a source node to a target node with a semantic relationship,
/// an optional firing condition, and a priority weight for scheduling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// The node this edge originates from.
    pub source: NodeId,
    /// The node this edge points to.
    pub target: NodeId,
    /// What kind of relationship this edge represents.
    pub kind: EdgeKind,
    /// Condition that must be met for this edge to fire.
    /// `None` is treated as `EdgeCondition::Always`.
    pub condition: Option<EdgeCondition>,
    /// Priority weight — higher values indicate higher priority edges
    /// when multiple edges compete for scheduling order.
    pub weight: i32,
}

impl Edge {
    /// Create a new edge with default condition (Always) and weight (0).
    pub fn new(source: NodeId, target: NodeId, kind: EdgeKind) -> Self {
        Self {
            source,
            target,
            kind,
            condition: None,
            weight: 0,
        }
    }

    /// Builder: set the firing condition.
    pub fn with_condition(mut self, condition: EdgeCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Builder: set the priority weight.
    pub fn with_weight(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }

    /// Returns the effective condition, defaulting to `Always` if none set.
    pub fn effective_condition(&self) -> &EdgeCondition {
        self.condition.as_ref().unwrap_or(&EdgeCondition::Always)
    }
}

impl fmt::Display for Edge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} --[{}]--> {} ({})",
            self.source,
            self.kind,
            self.target,
            self.effective_condition()
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_default_condition() {
        let src = NodeId::new();
        let tgt = NodeId::new();
        let edge = Edge::new(src, tgt, EdgeKind::DependsOn);
        assert_eq!(*edge.effective_condition(), EdgeCondition::Always);
        assert!(edge.condition.is_none());
    }

    #[test]
    fn edge_with_condition() {
        let src = NodeId::new();
        let tgt = NodeId::new();
        let edge = Edge::new(src, tgt, EdgeKind::Conditional)
            .with_condition(EdgeCondition::OnSuccess)
            .with_weight(5);

        assert_eq!(edge.condition, Some(EdgeCondition::OnSuccess));
        assert_eq!(edge.weight, 5);
    }

    #[test]
    fn edge_on_output_condition() {
        let cond = EdgeCondition::OnOutput("classification".into());
        assert_eq!(format!("{cond}"), "on_output(classification)");
    }

    #[test]
    fn edge_display() {
        let src = NodeId::new();
        let tgt = NodeId::new();
        let edge = Edge::new(src, tgt, EdgeKind::DataFlow)
            .with_condition(EdgeCondition::OnFailure);
        let display = format!("{edge}");
        assert!(display.contains("data_flow"));
        assert!(display.contains("on_failure"));
    }

    #[test]
    fn edge_kind_display() {
        assert_eq!(format!("{}", EdgeKind::DependsOn), "depends_on");
        assert_eq!(format!("{}", EdgeKind::Triggers), "triggers");
        assert_eq!(format!("{}", EdgeKind::Blocks), "blocks");
        assert_eq!(format!("{}", EdgeKind::DataFlow), "data_flow");
        assert_eq!(format!("{}", EdgeKind::Conditional), "conditional");
    }
}
