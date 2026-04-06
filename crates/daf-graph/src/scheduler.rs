//! Wave-based parallel scheduler for the DAF execution DAG.
//!
//! Groups ready nodes into "waves" — sets of nodes that can execute in
//! parallel because they have no mutual dependencies. Respects concurrency
//! limits and priority ordering. Produces an `ExecutionPlan` that the
//! executor consumes.

use crate::dag::ExecutionGraph;
use crate::error::GraphError;
use crate::node::{NodeId, NodeState};

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, instrument};

// ---------------------------------------------------------------------------
// Wave
// ---------------------------------------------------------------------------

/// A single wave of parallel execution — all nodes in a wave can run
/// concurrently because none depend on each other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wave {
    /// Sequential wave number (0-indexed).
    pub wave_number: usize,
    /// Nodes to execute in this wave.
    pub nodes: Vec<NodeId>,
    /// Estimated duration is the maximum estimated duration of any node in
    /// this wave (since they run in parallel).
    pub estimated_duration: Duration,
}

// ---------------------------------------------------------------------------
// ExecutionPlan
// ---------------------------------------------------------------------------

/// A complete execution plan: an ordered sequence of waves with timing
/// estimates and parallelism metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// The waves to execute, in order.
    pub waves: Vec<Wave>,
    /// Sum of all wave estimated durations (wall-clock estimate).
    pub total_estimated_duration: Duration,
    /// Average number of nodes per wave — a measure of how much parallelism
    /// the graph topology allows.
    pub parallelism_factor: f64,
}

impl ExecutionPlan {
    /// Total number of nodes across all waves.
    pub fn total_nodes(&self) -> usize {
        self.waves.iter().map(|w| w.nodes.len()).sum()
    }

    /// True if the plan has no waves (empty or fully-completed graph).
    pub fn is_empty(&self) -> bool {
        self.waves.is_empty()
    }
}

// ---------------------------------------------------------------------------
// WaveScheduler
// ---------------------------------------------------------------------------

/// Schedules graph nodes into waves for parallel execution.
///
/// The scheduler does not execute anything — it only produces a plan.
/// The `GraphExecutor` consumes the plan and drives actual execution.
pub struct WaveScheduler {
    /// Maximum number of nodes that can run concurrently in a single wave.
    /// `None` means unlimited.
    max_concurrency: Option<usize>,
}

impl WaveScheduler {
    /// Create a scheduler with no concurrency limit.
    pub fn new() -> Self {
        Self {
            max_concurrency: None,
        }
    }

    /// Create a scheduler with a maximum concurrency limit.
    pub fn with_max_concurrency(max: usize) -> Self {
        Self {
            max_concurrency: Some(max),
        }
    }

    /// Plan waves from the current graph state.
    ///
    /// Simulates execution by marking nodes as "planned" (without actually
    /// changing the graph) to determine wave groupings. Nodes within each
    /// wave are sorted by priority (descending).
    #[instrument(skip(self, graph))]
    pub fn plan_waves(&self, graph: &ExecutionGraph) -> Result<ExecutionPlan, GraphError> {
        if graph.node_count() == 0 {
            return Ok(ExecutionPlan {
                waves: vec![],
                total_estimated_duration: Duration::ZERO,
                parallelism_factor: 0.0,
            });
        }

        // Work on a clone so we don't mutate the real graph.
        let mut sim = graph.clone();
        let mut waves = Vec::new();
        let mut wave_number = 0;
        let mut total_planned = 0;

        loop {
            let mut ready = sim.ready_nodes();
            if ready.is_empty() {
                break;
            }

            // Sort by priority (highest first), then by name for determinism.
            ready.sort_by(|a, b| {
                let pa = sim.get_node(*a).map(|n| n.priority).unwrap_or(0);
                let pb = sim.get_node(*b).map(|n| n.priority).unwrap_or(0);
                pb.cmp(&pa).then_with(|| {
                    let na = sim.get_node(*a).map(|n| n.name.as_str()).unwrap_or("");
                    let nb = sim.get_node(*b).map(|n| n.name.as_str()).unwrap_or("");
                    na.cmp(nb)
                })
            });

            // Apply concurrency limit.
            if let Some(max) = self.max_concurrency {
                ready.truncate(max);
            }

            // Calculate estimated duration for this wave (max of all nodes).
            let estimated_duration = ready
                .iter()
                .filter_map(|id| {
                    sim.get_node(*id)
                        .and_then(|n| n.estimated_duration)
                })
                .max()
                .unwrap_or(Duration::from_secs(1));

            let wave = Wave {
                wave_number,
                nodes: ready.clone(),
                estimated_duration,
            };

            debug!(
                wave = wave_number,
                nodes = ready.len(),
                "planned wave"
            );

            total_planned += ready.len();

            // Simulate completion of all nodes in this wave.
            for id in &ready {
                let _ = sim.mark_complete(*id, NodeState::Succeeded, None);
            }

            waves.push(wave);
            wave_number += 1;

            // Safety valve: prevent infinite loops.
            if wave_number > graph.node_count() {
                break;
            }
        }

        let total_estimated_duration: Duration =
            waves.iter().map(|w| w.estimated_duration).sum();
        let parallelism_factor = if waves.is_empty() {
            0.0
        } else {
            total_planned as f64 / waves.len() as f64
        };

        Ok(ExecutionPlan {
            waves,
            total_estimated_duration,
            parallelism_factor,
        })
    }

    /// Re-plan after a failure: given the current graph state (some nodes
    /// failed/skipped), produce a new plan for the remaining work.
    ///
    /// This is used for recovery — skip dependents of failed nodes and
    /// schedule whatever is still executable.
    #[instrument(skip(self, graph))]
    pub fn replan_after_failure(
        &self,
        graph: &ExecutionGraph,
    ) -> Result<ExecutionPlan, GraphError> {
        // The graph already has failure/skip states applied by
        // `ExecutionGraph::mark_complete`. We just plan from current state.
        self.plan_waves(graph)
    }
}

impl Default for WaveScheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::{Edge, EdgeKind};
    use crate::node::{Node, NodeKind};

    fn make_task(name: &str) -> Node {
        Node::new(NodeKind::Task, name)
    }

    #[test]
    fn empty_graph_plan() {
        let g = ExecutionGraph::new("empty");
        let scheduler = WaveScheduler::new();
        let plan = scheduler.plan_waves(&g).unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.total_nodes(), 0);
    }

    #[test]
    fn single_node_plan() {
        let mut g = ExecutionGraph::new("single");
        g.add_node(make_task("only"));
        let plan = WaveScheduler::new().plan_waves(&g).unwrap();
        assert_eq!(plan.waves.len(), 1);
        assert_eq!(plan.total_nodes(), 1);
    }

    #[test]
    fn linear_chain_waves() {
        let mut g = ExecutionGraph::new("linear");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        let plan = WaveScheduler::new().plan_waves(&g).unwrap();
        assert_eq!(plan.waves.len(), 3);
        assert_eq!(plan.waves[0].nodes, vec![a]);
        assert_eq!(plan.waves[1].nodes, vec![b]);
        assert_eq!(plan.waves[2].nodes, vec![c]);
        // Linear chain has parallelism factor of 1.0.
        assert!((plan.parallelism_factor - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn diamond_graph_waves() {
        // A -> B, A -> C, B -> D, C -> D
        let mut g = ExecutionGraph::new("diamond");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        let d = g.add_node(make_task("d"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(a, c, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, d, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(c, d, EdgeKind::DependsOn)).unwrap();

        let plan = WaveScheduler::new().plan_waves(&g).unwrap();
        assert_eq!(plan.waves.len(), 3);
        assert_eq!(plan.waves[0].nodes.len(), 1); // a
        assert_eq!(plan.waves[1].nodes.len(), 2); // b, c in parallel
        assert_eq!(plan.waves[2].nodes.len(), 1); // d
    }

    #[test]
    fn concurrency_limit() {
        // Three independent nodes, concurrency limit of 2.
        let mut g = ExecutionGraph::new("limited");
        g.add_node(make_task("a"));
        g.add_node(make_task("b"));
        g.add_node(make_task("c"));

        let plan = WaveScheduler::with_max_concurrency(2)
            .plan_waves(&g)
            .unwrap();
        // Should split into 2 waves: [a,b] and [c].
        assert_eq!(plan.waves.len(), 2);
        assert!(plan.waves[0].nodes.len() <= 2);
        assert_eq!(plan.total_nodes(), 3);
    }

    #[test]
    fn priority_ordering() {
        let mut g = ExecutionGraph::new("priority");
        let low = g.add_node(make_task("low").with_priority(1));
        let high = g.add_node(make_task("high").with_priority(100));
        let mid = g.add_node(make_task("mid").with_priority(50));

        let plan = WaveScheduler::new().plan_waves(&g).unwrap();
        assert_eq!(plan.waves.len(), 1);
        // All in one wave, ordered by priority descending.
        assert_eq!(plan.waves[0].nodes[0], high);
        assert_eq!(plan.waves[0].nodes[1], mid);
        assert_eq!(plan.waves[0].nodes[2], low);
    }
}
