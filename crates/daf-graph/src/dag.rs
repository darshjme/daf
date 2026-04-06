//! Core DAG implementation for the DAF execution engine.
//!
//! Wraps `petgraph::DiGraph` to provide a validated, serializable execution
//! graph with dependency resolution, topological ordering, critical path
//! analysis, and subgraph extraction. This is the single source of truth
//! for what runs when and in what order.

use crate::edge::{Edge, EdgeCondition};
use crate::error::GraphError;
use crate::node::{Node, NodeId, NodeState};

use petgraph::algo::{is_cyclic_directed, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use tracing::{debug, instrument, warn};

// ---------------------------------------------------------------------------
// ExecutionGraph
// ---------------------------------------------------------------------------

/// A validated directed acyclic graph of execution nodes.
///
/// Internally maps `NodeId` to petgraph `NodeIndex` for efficient traversal.
/// All mutations go through methods that maintain invariants (no cycles,
/// consistent dependency sets).
#[derive(Debug, Clone)]
pub struct ExecutionGraph {
    /// The underlying petgraph directed graph.
    graph: DiGraph<Node, Edge>,
    /// Fast lookup from logical NodeId to petgraph NodeIndex.
    index_map: HashMap<NodeId, NodeIndex>,
    /// Human-readable name for this graph.
    pub name: String,
}

/// Wire format for serializing/deserializing an `ExecutionGraph`.
#[derive(Serialize, Deserialize)]
struct GraphWire {
    name: String,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Serialize for ExecutionGraph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = GraphWire {
            name: self.name.clone(),
            nodes: self.graph.node_weights().cloned().collect(),
            edges: self.graph.edge_weights().cloned().collect(),
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExecutionGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = GraphWire::deserialize(deserializer)?;
        let mut g = ExecutionGraph::new(wire.name);
        for node in wire.nodes {
            g.add_node(node);
        }
        for edge in wire.edges {
            g.add_edge(edge).map_err(serde::de::Error::custom)?;
        }
        Ok(g)
    }
}

impl ExecutionGraph {
    /// Create a new empty execution graph with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            graph: DiGraph::new(),
            index_map: HashMap::new(),
            name: name.into(),
        }
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Add a node to the graph. Returns the node's ID.
    #[instrument(skip(self, node), fields(node_name = %node.name, node_kind = %node.kind))]
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = node.id;
        let idx = self.graph.add_node(node);
        self.index_map.insert(id, idx);
        debug!("added node {id}");
        id
    }

    /// Add a directed edge between two nodes.
    ///
    /// Returns an error if either node doesn't exist or if adding the edge
    /// would create a cycle.
    #[instrument(skip(self))]
    pub fn add_edge(&mut self, edge: Edge) -> Result<(), GraphError> {
        let src_idx = self
            .index_map
            .get(&edge.source)
            .copied()
            .ok_or(GraphError::NodeNotFound(edge.source))?;
        let tgt_idx = self
            .index_map
            .get(&edge.target)
            .copied()
            .ok_or(GraphError::NodeNotFound(edge.target))?;

        // Temporarily add the edge, check for cycles, remove if invalid.
        let edge_idx = self.graph.add_edge(src_idx, tgt_idx, edge);
        if is_cyclic_directed(&self.graph) {
            self.graph.remove_edge(edge_idx);
            return Err(GraphError::CycleDetected);
        }

        // Update the target node's dependency set.
        let source_id = self.graph[src_idx].id;
        self.graph[tgt_idx].dependencies.insert(source_id);

        debug!("added edge {src_idx:?} -> {tgt_idx:?}");
        Ok(())
    }

    /// Remove a node and all its connected edges from the graph.
    ///
    /// Also cleans up dependency references in other nodes.
    pub fn remove_node(&mut self, id: NodeId) -> Result<Node, GraphError> {
        let idx = self
            .index_map
            .remove(&id)
            .ok_or(GraphError::NodeNotFound(id))?;

        // Clean up dependency references in all nodes that depended on this one.
        let dependents: Vec<NodeIndex> = self
            .graph
            .neighbors_directed(idx, Direction::Outgoing)
            .collect();
        for dep_idx in dependents {
            self.graph[dep_idx].dependencies.remove(&id);
        }

        // petgraph swaps the last node into the removed slot — fix up index_map.
        let removed = self.graph.remove_node(idx).ok_or(GraphError::NodeNotFound(id))?;

        // Rebuild index_map for any node that may have been swapped.
        // After remove_node, the node that was at the last index is now at `idx`.
        if idx.index() < self.graph.node_count() {
            let swapped_id = self.graph[idx].id;
            self.index_map.insert(swapped_id, idx);
        }

        debug!("removed node {id}");
        Ok(removed)
    }

    /// Get an immutable reference to a node by ID.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.index_map.get(&id).map(|&idx| &self.graph[idx])
    }

    /// Get a mutable reference to a node by ID.
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.index_map
            .get(&id)
            .copied()
            .map(move |idx| &mut self.graph[idx])
    }

    /// Validate the graph: check for cycles and structural invariants.
    pub fn validate(&self) -> Result<(), GraphError> {
        if is_cyclic_directed(&self.graph) {
            return Err(GraphError::CycleDetected);
        }
        Ok(())
    }

    /// Return nodes in topological order (dependencies before dependents).
    pub fn topological_sort(&self) -> Result<Vec<NodeId>, GraphError> {
        let sorted = toposort(&self.graph, None).map_err(|_| GraphError::CycleDetected)?;
        Ok(sorted.into_iter().map(|idx| self.graph[idx].id).collect())
    }

    /// Return all nodes whose dependencies are fully satisfied (in a terminal
    /// success state) and that are currently `Pending`.
    ///
    /// These are the nodes eligible to transition to `Ready` for scheduling.
    pub fn ready_nodes(&self) -> Vec<NodeId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                let node = &self.graph[idx];
                if node.state != NodeState::Pending {
                    return false;
                }
                // Check all incoming edges (predecessors).
                self.graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .all(|pred_idx| {
                        let pred = &self.graph[pred_idx];
                        // Find the edge from pred to this node to check conditions.
                        let edge = self
                            .graph
                            .edges_connecting(pred_idx, idx)
                            .next();
                        match edge {
                            Some(e) => Self::edge_satisfied(pred, e.weight()),
                            None => pred.state.is_success(),
                        }
                    })
            })
            .map(|idx| self.graph[idx].id)
            .collect()
    }

    /// Check whether an edge is satisfied given the predecessor's state.
    fn edge_satisfied(pred: &Node, edge: &Edge) -> bool {
        match edge.effective_condition() {
            EdgeCondition::Always => pred.state.is_terminal(),
            EdgeCondition::OnSuccess => pred.state.is_success(),
            EdgeCondition::OnFailure => pred.state == NodeState::Failed,
            EdgeCondition::OnOutput(_) => {
                // Output-based conditions are considered satisfied on success
                // (the executor checks the actual output content).
                pred.state.is_success()
            }
        }
    }

    /// Mark a node as complete with the given state. Returns the IDs of any
    /// nodes that have become newly ready as a result.
    #[instrument(skip(self))]
    pub fn mark_complete(
        &mut self,
        id: NodeId,
        new_state: NodeState,
        actual_duration: Option<Duration>,
    ) -> Result<Vec<NodeId>, GraphError> {
        let idx = self
            .index_map
            .get(&id)
            .copied()
            .ok_or(GraphError::NodeNotFound(id))?;

        let node = &mut self.graph[idx];
        node.state = new_state;
        node.actual_duration = actual_duration;

        debug!("node {id} marked as {new_state}");

        // If the node failed, cascade skips to dependents (unless edge says OnFailure).
        if new_state == NodeState::Failed {
            self.cascade_skip(idx);
        }

        Ok(self.ready_nodes())
    }

    /// Recursively skip all downstream nodes that can no longer execute
    /// because a predecessor failed and the edge requires success.
    fn cascade_skip(&mut self, failed_idx: NodeIndex) {
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        // Collect direct dependents.
        for succ_idx in self
            .graph
            .neighbors_directed(failed_idx, Direction::Outgoing)
            .collect::<Vec<_>>()
        {
            // Check if the edge to this successor requires success.
            let should_skip = self
                .graph
                .edges_connecting(failed_idx, succ_idx)
                .any(|e| {
                    matches!(
                        e.weight().effective_condition(),
                        EdgeCondition::Always | EdgeCondition::OnSuccess
                    )
                });
            if should_skip && self.graph[succ_idx].state == NodeState::Pending {
                self.graph[succ_idx].state = NodeState::Skipped;
                warn!("skipping node {} due to upstream failure", self.graph[succ_idx].id);
                queue.push_back(succ_idx);
            }
        }
        // BFS cascade.
        while let Some(skipped_idx) = queue.pop_front() {
            for succ_idx in self
                .graph
                .neighbors_directed(skipped_idx, Direction::Outgoing)
                .collect::<Vec<_>>()
            {
                if self.graph[succ_idx].state == NodeState::Pending {
                    self.graph[succ_idx].state = NodeState::Skipped;
                    warn!("cascade skip: node {}", self.graph[succ_idx].id);
                    queue.push_back(succ_idx);
                }
            }
        }
    }

    /// Calculate the critical path — the longest path through the graph by
    /// estimated duration. Returns the ordered node IDs and total duration.
    ///
    /// Uses dynamic programming on the topological order.
    pub fn critical_path(&self) -> Result<(Vec<NodeId>, Duration), GraphError> {
        let topo = toposort(&self.graph, None).map_err(|_| GraphError::CycleDetected)?;

        if topo.is_empty() {
            return Ok((vec![], Duration::ZERO));
        }

        // dist[idx] = longest path ending at idx, predecessor[idx] = previous node on that path.
        let mut dist: HashMap<NodeIndex, Duration> = HashMap::new();
        let mut predecessor: HashMap<NodeIndex, NodeIndex> = HashMap::new();

        for &idx in &topo {
            let node_dur = self.graph[idx]
                .estimated_duration
                .unwrap_or(Duration::from_secs(1));
            let mut best = Duration::ZERO;
            let mut best_pred = None;

            for pred_idx in self.graph.neighbors_directed(idx, Direction::Incoming) {
                let pred_dist = dist.get(&pred_idx).copied().unwrap_or(Duration::ZERO);
                if pred_dist > best {
                    best = pred_dist;
                    best_pred = Some(pred_idx);
                }
            }

            dist.insert(idx, best + node_dur);
            if let Some(p) = best_pred {
                predecessor.insert(idx, p);
            }
        }

        // Find the node with the maximum distance.
        let (&end_idx, &total) = dist
            .iter()
            .max_by_key(|(_, d)| **d)
            .expect("non-empty graph");

        // Trace back the path.
        let mut path = vec![end_idx];
        let mut current = end_idx;
        while let Some(&pred) = predecessor.get(&current) {
            path.push(pred);
            current = pred;
        }
        path.reverse();

        let path_ids = path.into_iter().map(|idx| self.graph[idx].id).collect();
        Ok((path_ids, total))
    }

    /// Extract a connected subgraph containing the specified node and all its
    /// transitive dependencies and dependents.
    pub fn subgraph(&self, root: NodeId) -> Result<ExecutionGraph, GraphError> {
        let root_idx = self
            .index_map
            .get(&root)
            .copied()
            .ok_or(GraphError::NodeNotFound(root))?;

        let mut visited: HashSet<NodeIndex> = HashSet::new();
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();

        // BFS in both directions.
        queue.push_back(root_idx);
        visited.insert(root_idx);
        while let Some(idx) = queue.pop_front() {
            for neighbor in self
                .graph
                .neighbors_directed(idx, Direction::Incoming)
                .chain(self.graph.neighbors_directed(idx, Direction::Outgoing))
            {
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }

        // Build the subgraph.
        let mut sub = ExecutionGraph::new(format!("{}_sub_{root}", self.name));
        let mut old_to_new: HashMap<NodeIndex, NodeId> = HashMap::new();

        for &idx in &visited {
            let node = self.graph[idx].clone();
            let new_id = sub.add_node(node);
            old_to_new.insert(idx, new_id);
        }

        for &idx in &visited {
            for edge_ref in self.graph.edges_directed(idx, Direction::Outgoing) {
                if visited.contains(&edge_ref.target()) {
                    let edge = edge_ref.weight().clone();
                    // Remap source/target to subgraph node IDs.
                    let new_source = old_to_new[&idx];
                    let new_target = old_to_new[&edge_ref.target()];
                    let new_edge = Edge {
                        source: new_source,
                        target: new_target,
                        ..edge
                    };
                    // Ignore cycle errors — subgraph of a DAG is still a DAG.
                    let _ = sub.add_edge(new_edge);
                }
            }
        }

        Ok(sub)
    }

    /// Return all node IDs with no incoming edges (graph entry points).
    pub fn roots(&self) -> Vec<NodeId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .next()
                    .is_none()
            })
            .map(|idx| self.graph[idx].id)
            .collect()
    }

    /// Return all node IDs with no outgoing edges (graph exit points).
    pub fn leaves(&self) -> Vec<NodeId> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, Direction::Outgoing)
                    .next()
                    .is_none()
            })
            .map(|idx| self.graph[idx].id)
            .collect()
    }

    /// Get all outgoing edges from a node.
    pub fn outgoing_edges(&self, id: NodeId) -> Vec<&Edge> {
        if let Some(&idx) = self.index_map.get(&id) {
            self.graph
                .edges_directed(idx, Direction::Outgoing)
                .map(|e| e.weight())
                .collect()
        } else {
            vec![]
        }
    }

    /// Get all incoming edges to a node.
    pub fn incoming_edges(&self, id: NodeId) -> Vec<&Edge> {
        if let Some(&idx) = self.index_map.get(&id) {
            self.graph
                .edges_directed(idx, Direction::Incoming)
                .map(|e| e.weight())
                .collect()
        } else {
            vec![]
        }
    }

    /// Iterate over all nodes in the graph.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.graph.node_weights()
    }

    /// Iterate over all edges in the graph.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.graph.edge_weights()
    }

    /// Serialize the graph to JSON for persistence.
    pub fn to_json(&self) -> Result<String, GraphError> {
        serde_json::to_string_pretty(self).map_err(|e| GraphError::Serialization(e.to_string()))
    }

    /// Deserialize a graph from JSON.
    pub fn from_json(json: &str) -> Result<Self, GraphError> {
        let mut graph: ExecutionGraph =
            serde_json::from_str(json).map_err(|e| GraphError::Serialization(e.to_string()))?;

        // Rebuild the index_map from the deserialized graph.
        graph.index_map.clear();
        for idx in graph.graph.node_indices() {
            let id = graph.graph[idx].id;
            graph.index_map.insert(id, idx);
        }

        Ok(graph)
    }

    /// Access the underlying petgraph for advanced traversals.
    pub(crate) fn inner(&self) -> &DiGraph<Node, Edge> {
        &self.graph
    }

    /// Get the petgraph NodeIndex for a NodeId.
    pub(crate) fn node_index(&self, id: NodeId) -> Option<NodeIndex> {
        self.index_map.get(&id).copied()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::EdgeKind;
    use crate::node::NodeKind;

    fn make_task(name: &str) -> Node {
        Node::new(NodeKind::Task, name)
    }

    #[test]
    fn add_nodes_and_edges() {
        let mut g = ExecutionGraph::new("test");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();

        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn cycle_detection() {
        let mut g = ExecutionGraph::new("cycle-test");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        let result = g.add_edge(Edge::new(b, a, EdgeKind::DependsOn));
        assert!(matches!(result, Err(GraphError::CycleDetected)));
    }

    #[test]
    fn topological_sort_linear() {
        let mut g = ExecutionGraph::new("topo");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        let sorted = g.topological_sort().unwrap();
        assert_eq!(sorted, vec![a, b, c]);
    }

    #[test]
    fn ready_nodes_initial() {
        let mut g = ExecutionGraph::new("ready");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(a, c, EdgeKind::DependsOn)).unwrap();

        let ready = g.ready_nodes();
        // Only "a" has no dependencies.
        assert_eq!(ready, vec![a]);
    }

    #[test]
    fn mark_complete_unlocks_dependents() {
        let mut g = ExecutionGraph::new("complete");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(a, c, EdgeKind::DependsOn)).unwrap();

        let newly_ready = g
            .mark_complete(a, NodeState::Succeeded, Some(Duration::from_secs(1)))
            .unwrap();
        assert!(newly_ready.contains(&b));
        assert!(newly_ready.contains(&c));
    }

    #[test]
    fn failure_cascades_skip() {
        let mut g = ExecutionGraph::new("cascade");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        g.mark_complete(a, NodeState::Failed, None).unwrap();

        assert_eq!(g.get_node(b).unwrap().state, NodeState::Skipped);
        assert_eq!(g.get_node(c).unwrap().state, NodeState::Skipped);
    }

    #[test]
    fn critical_path_linear() {
        let mut g = ExecutionGraph::new("critical");
        let mut a = make_task("a");
        a.estimated_duration = Some(Duration::from_secs(10));
        let mut b = make_task("b");
        b.estimated_duration = Some(Duration::from_secs(20));
        let mut c = make_task("c");
        c.estimated_duration = Some(Duration::from_secs(5));

        let a_id = g.add_node(a);
        let b_id = g.add_node(b);
        let c_id = g.add_node(c);
        g.add_edge(Edge::new(a_id, b_id, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b_id, c_id, EdgeKind::DependsOn)).unwrap();

        let (path, total) = g.critical_path().unwrap();
        assert_eq!(path, vec![a_id, b_id, c_id]);
        assert_eq!(total, Duration::from_secs(35));
    }

    #[test]
    fn roots_and_leaves() {
        let mut g = ExecutionGraph::new("root-leaf");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        assert_eq!(g.roots(), vec![a]);
        assert_eq!(g.leaves(), vec![c]);
    }

    #[test]
    fn remove_node_cleans_deps() {
        let mut g = ExecutionGraph::new("remove");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        g.remove_node(b).unwrap();
        assert_eq!(g.node_count(), 2);
        // c should no longer have b in its dependencies.
        assert!(g.get_node(c).unwrap().dependencies.is_empty());
    }

    #[test]
    fn json_roundtrip() {
        let mut g = ExecutionGraph::new("json");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();

        let json = g.to_json().unwrap();
        let g2 = ExecutionGraph::from_json(&json).unwrap();

        assert_eq!(g2.node_count(), 2);
        assert_eq!(g2.edge_count(), 1);
        assert_eq!(g2.name, "json");
        assert!(g2.get_node(a).is_some());
        assert!(g2.get_node(b).is_some());
    }

    #[test]
    fn subgraph_extraction() {
        let mut g = ExecutionGraph::new("subgraph");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        let d = g.add_node(make_task("d")); // disconnected
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        let sub = g.subgraph(b).unwrap();
        // Should contain a, b, c but not d.
        assert_eq!(sub.node_count(), 3);
        assert!(sub.get_node(d).is_none());
    }

    #[test]
    fn conditional_edge_on_failure() {
        let mut g = ExecutionGraph::new("conditional");
        let a = g.add_node(make_task("main-task"));
        let b = g.add_node(make_task("fallback"));
        g.add_edge(
            Edge::new(a, b, EdgeKind::Conditional)
                .with_condition(EdgeCondition::OnFailure),
        )
        .unwrap();

        // When a succeeds, b should NOT become ready (edge requires failure).
        g.mark_complete(a, NodeState::Succeeded, None).unwrap();
        let ready = g.ready_nodes();
        assert!(ready.is_empty());
    }

    #[test]
    fn conditional_edge_on_failure_triggers() {
        let mut g = ExecutionGraph::new("conditional-fire");
        let a = g.add_node(make_task("main-task"));
        let b = g.add_node(make_task("fallback"));
        g.add_edge(
            Edge::new(a, b, EdgeKind::Conditional)
                .with_condition(EdgeCondition::OnFailure),
        )
        .unwrap();

        // When a fails, b SHOULD become ready.
        // But we also need to prevent cascade skip for OnFailure edges.
        // Mark a as failed — cascade_skip checks the edge condition.
        let _ = g.mark_complete(a, NodeState::Failed, None);
        // b should not have been skipped because the edge is OnFailure.
        // However our cascade_skip skips on Always|OnSuccess edges.
        // Since this edge is OnFailure, b should remain Pending and become ready.
        assert_eq!(g.get_node(b).unwrap().state, NodeState::Pending);
        let ready = g.ready_nodes();
        assert!(ready.contains(&b));
    }
}
