//! Graph traversal utilities.
//!
//! Provides DFS and BFS iterators, plus convenience functions for querying
//! transitive dependencies (ancestors) and dependents (descendants), and
//! identifying root and leaf nodes.

use crate::dag::ExecutionGraph;
use crate::node::{Node, NodeId};

use petgraph::graph::NodeIndex;
use petgraph::Direction;
use std::collections::{HashSet, VecDeque};

// ---------------------------------------------------------------------------
// DepthFirst
// ---------------------------------------------------------------------------

/// Depth-first traversal iterator over graph nodes.
///
/// Visits nodes in DFS order starting from the given root set. If no roots
/// are specified, starts from all graph roots (nodes with no incoming edges).
pub struct DepthFirst<'g> {
    graph: &'g ExecutionGraph,
    stack: Vec<NodeIndex>,
    visited: HashSet<NodeIndex>,
}

impl<'g> DepthFirst<'g> {
    /// Create a DFS iterator starting from the specified roots.
    pub fn new(graph: &'g ExecutionGraph, roots: &[NodeId]) -> Self {
        let mut stack = Vec::new();
        let visited = HashSet::new();
        for &root in roots.iter().rev() {
            if let Some(idx) = graph.node_index(root) {
                stack.push(idx);
            }
        }
        Self {
            graph,
            stack,
            visited,
        }
    }

    /// Create a DFS iterator starting from all graph roots.
    pub fn from_roots(graph: &'g ExecutionGraph) -> Self {
        let roots = graph.roots();
        Self::new(graph, &roots)
    }
}

impl<'g> Iterator for DepthFirst<'g> {
    type Item = &'g Node;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(idx) = self.stack.pop() {
            if !self.visited.insert(idx) {
                continue;
            }
            // Push successors (outgoing neighbors) onto the stack.
            let inner = self.graph.inner();
            let successors: Vec<NodeIndex> = inner
                .neighbors_directed(idx, Direction::Outgoing)
                .collect();
            for succ in successors.into_iter().rev() {
                if !self.visited.contains(&succ) {
                    self.stack.push(succ);
                }
            }
            return Some(&inner[idx]);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// BreadthFirst
// ---------------------------------------------------------------------------

/// Breadth-first traversal iterator over graph nodes.
///
/// Visits nodes level by level starting from the given root set.
pub struct BreadthFirst<'g> {
    graph: &'g ExecutionGraph,
    queue: VecDeque<NodeIndex>,
    visited: HashSet<NodeIndex>,
}

impl<'g> BreadthFirst<'g> {
    /// Create a BFS iterator starting from the specified roots.
    pub fn new(graph: &'g ExecutionGraph, roots: &[NodeId]) -> Self {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        for &root in roots {
            if let Some(idx) = graph.node_index(root) {
                if visited.insert(idx) {
                    queue.push_back(idx);
                }
            }
        }
        Self {
            graph,
            queue,
            visited,
        }
    }

    /// Create a BFS iterator starting from all graph roots.
    pub fn from_roots(graph: &'g ExecutionGraph) -> Self {
        let roots = graph.roots();
        Self::new(graph, &roots)
    }
}

impl<'g> Iterator for BreadthFirst<'g> {
    type Item = &'g Node;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(idx) = self.queue.pop_front() {
            let inner = self.graph.inner();
            for succ in inner.neighbors_directed(idx, Direction::Outgoing) {
                if self.visited.insert(succ) {
                    self.queue.push_back(succ);
                }
            }
            Some(&inner[idx])
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Return all transitive dependencies of a node (ancestors in the DAG).
///
/// Traverses incoming edges recursively. The result does NOT include the
/// starting node itself.
pub fn ancestors(graph: &ExecutionGraph, id: NodeId) -> HashSet<NodeId> {
    let mut result = HashSet::new();
    let Some(start_idx) = graph.node_index(id) else {
        return result;
    };

    let inner = graph.inner();
    let mut queue = VecDeque::new();
    queue.push_back(start_idx);
    let mut visited = HashSet::new();
    visited.insert(start_idx);

    while let Some(idx) = queue.pop_front() {
        for pred in inner.neighbors_directed(idx, Direction::Incoming) {
            if visited.insert(pred) {
                result.insert(inner[pred].id);
                queue.push_back(pred);
            }
        }
    }

    result
}

/// Return all transitive dependents of a node (descendants in the DAG).
///
/// Traverses outgoing edges recursively. The result does NOT include the
/// starting node itself.
pub fn descendants(graph: &ExecutionGraph, id: NodeId) -> HashSet<NodeId> {
    let mut result = HashSet::new();
    let Some(start_idx) = graph.node_index(id) else {
        return result;
    };

    let inner = graph.inner();
    let mut queue = VecDeque::new();
    queue.push_back(start_idx);
    let mut visited = HashSet::new();
    visited.insert(start_idx);

    while let Some(idx) = queue.pop_front() {
        for succ in inner.neighbors_directed(idx, Direction::Outgoing) {
            if visited.insert(succ) {
                result.insert(inner[succ].id);
                queue.push_back(succ);
            }
        }
    }

    result
}

/// Return all root nodes (nodes with no incoming edges).
pub fn roots(graph: &ExecutionGraph) -> Vec<NodeId> {
    graph.roots()
}

/// Return all leaf nodes (nodes with no outgoing edges).
pub fn leaves(graph: &ExecutionGraph) -> Vec<NodeId> {
    graph.leaves()
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

    fn build_diamond() -> (ExecutionGraph, NodeId, NodeId, NodeId, NodeId) {
        let mut g = ExecutionGraph::new("diamond");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        let d = g.add_node(make_task("d"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(a, c, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, d, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(c, d, EdgeKind::DependsOn)).unwrap();
        (g, a, b, c, d)
    }

    #[test]
    fn dfs_visits_all_nodes() {
        let (g, _, _, _, _) = build_diamond();
        let visited: Vec<String> = DepthFirst::from_roots(&g)
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(visited.len(), 4);
        // DFS should visit a first.
        assert_eq!(visited[0], "a");
    }

    #[test]
    fn bfs_visits_all_nodes() {
        let (g, _, _, _, _) = build_diamond();
        let visited: Vec<String> = BreadthFirst::from_roots(&g)
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(visited.len(), 4);
        assert_eq!(visited[0], "a");
        // b and c should be in positions 1 and 2 (same level).
        let level1: HashSet<&str> = [visited[1].as_str(), visited[2].as_str()]
            .into_iter()
            .collect();
        assert!(level1.contains("b"));
        assert!(level1.contains("c"));
        assert_eq!(visited[3], "d");
    }

    #[test]
    fn ancestors_of_leaf() {
        let (g, a, b, c, d) = build_diamond();
        let anc = ancestors(&g, d);
        assert_eq!(anc.len(), 3);
        assert!(anc.contains(&a));
        assert!(anc.contains(&b));
        assert!(anc.contains(&c));
    }

    #[test]
    fn ancestors_of_root() {
        let (g, a, _, _, _) = build_diamond();
        let anc = ancestors(&g, a);
        assert!(anc.is_empty());
    }

    #[test]
    fn descendants_of_root() {
        let (g, a, b, c, d) = build_diamond();
        let desc = descendants(&g, a);
        assert_eq!(desc.len(), 3);
        assert!(desc.contains(&b));
        assert!(desc.contains(&c));
        assert!(desc.contains(&d));
    }

    #[test]
    fn descendants_of_leaf() {
        let (g, _, _, _, d) = build_diamond();
        let desc = descendants(&g, d);
        assert!(desc.is_empty());
    }

    #[test]
    fn roots_and_leaves() {
        let (g, a, _, _, d) = build_diamond();
        assert_eq!(roots(&g), vec![a]);
        assert_eq!(leaves(&g), vec![d]);
    }

    #[test]
    fn dfs_from_specific_root() {
        let (g, _, b, _, _) = build_diamond();
        let visited: Vec<String> = DepthFirst::new(&g, &[b])
            .map(|n| n.name.clone())
            .collect();
        // Starting from b, should visit b and d.
        assert_eq!(visited.len(), 2);
        assert!(visited.contains(&"b".to_string()));
        assert!(visited.contains(&"d".to_string()));
    }

    #[test]
    fn bfs_from_specific_root() {
        let (g, _, _, c, _) = build_diamond();
        let visited: Vec<String> = BreadthFirst::new(&g, &[c])
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(visited.len(), 2);
        assert_eq!(visited[0], "c");
        assert_eq!(visited[1], "d");
    }

    #[test]
    fn disconnected_graph_traversal() {
        let mut g = ExecutionGraph::new("disconnected");
        let _a = g.add_node(make_task("a"));
        let _b = g.add_node(make_task("b")); // no edges

        let bfs: Vec<String> = BreadthFirst::from_roots(&g)
            .map(|n| n.name.clone())
            .collect();
        // Both are roots, both should be visited.
        assert_eq!(bfs.len(), 2);

        let dfs: Vec<String> = DepthFirst::from_roots(&g)
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(dfs.len(), 2);
    }
}
