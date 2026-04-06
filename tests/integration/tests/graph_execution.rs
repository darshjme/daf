//! Graph execution integration tests.
//!
//! Tests DAG dependency ordering, parallel wave execution, failure
//! propagation, and mid-execution cancellation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use uuid::Uuid;

use daf_integration_tests::init_tracing;

// ---------------------------------------------------------------------------
// Minimal DAG types (mirrors expected daf-graph API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NodeId(Uuid);

impl NodeId {
    fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone)]
struct TaskNode {
    id: NodeId,
    name: String,
    deps: Vec<NodeId>,
    state: NodeState,
    /// Simulated execution duration in ms.
    duration_ms: u64,
    /// Whether this node should fail when executed.
    should_fail: bool,
}

impl TaskNode {
    fn new(name: &str) -> Self {
        Self {
            id: NodeId::new(),
            name: name.to_string(),
            deps: Vec::new(),
            state: NodeState::Pending,
            duration_ms: 10,
            should_fail: false,
        }
    }

    fn with_dep(mut self, dep: NodeId) -> Self {
        self.deps.push(dep);
        self
    }

    fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    fn failing(mut self) -> Self {
        self.should_fail = true;
        self
    }
}

struct TaskDag {
    nodes: Vec<TaskNode>,
}

impl TaskDag {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn add(&mut self, node: TaskNode) -> NodeId {
        let id = node.id;
        self.nodes.push(node);
        id
    }

    fn get(&self, id: NodeId) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn get_mut(&mut self, id: NodeId) -> Option<&mut TaskNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Kahn's algorithm topological sort.
    fn topological_sort(&self) -> Vec<NodeId> {
        let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
        let mut children: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for node in &self.nodes {
            in_degree.entry(node.id).or_insert(0);
            children.entry(node.id).or_default();
            for &dep in &node.deps {
                children.entry(dep).or_default().push(node.id);
                *in_degree.entry(node.id).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<NodeId> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::new();
        while let Some(node) = queue.pop_front() {
            sorted.push(node);
            for &child in children.get(&node).unwrap_or(&Vec::new()) {
                let deg = in_degree.get_mut(&child).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(child);
                }
            }
        }
        sorted
    }

    /// Partition into execution waves (nodes in same wave are independent).
    fn plan_waves(&self) -> Vec<Vec<NodeId>> {
        let sorted = self.topological_sort();
        let mut depth: HashMap<NodeId, usize> = HashMap::new();

        for &id in &sorted {
            let node = self.get(id).unwrap();
            let d = if node.deps.is_empty() {
                0
            } else {
                node.deps.iter().map(|dep| depth[dep] + 1).max().unwrap()
            };
            depth.insert(id, d);
        }

        let max_depth = depth.values().copied().max().unwrap_or(0);
        let mut waves: Vec<Vec<NodeId>> = vec![Vec::new(); max_depth + 1];
        for (&id, &d) in &depth {
            waves[d].push(id);
        }
        waves
    }

    /// Execute the DAG, tracking order of execution. Returns the execution
    /// log and whether the entire DAG succeeded.
    async fn execute(
        &mut self,
        cancel: Arc<AtomicBool>,
    ) -> (Vec<(NodeId, NodeState)>, bool) {
        let waves = self.plan_waves();
        let mut log = Vec::new();
        let mut failed_nodes: HashSet<NodeId> = HashSet::new();
        let mut all_ok = true;

        for wave in &waves {
            if cancel.load(Ordering::Relaxed) {
                // Mark remaining as cancelled.
                for &id in wave {
                    self.get_mut(id).unwrap().state = NodeState::Cancelled;
                    log.push((id, NodeState::Cancelled));
                }
                all_ok = false;
                continue;
            }

            let mut handles = Vec::new();
            for &id in wave {
                let node = self.get(id).unwrap();

                // Skip if any dependency failed.
                let dep_failed = node.deps.iter().any(|dep| failed_nodes.contains(dep));
                if dep_failed {
                    self.get_mut(id).unwrap().state = NodeState::Skipped;
                    log.push((id, NodeState::Skipped));
                    all_ok = false;
                    continue;
                }

                let should_fail = node.should_fail;
                let duration = node.duration_ms;

                handles.push(tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(duration)).await;
                    if should_fail {
                        (id, NodeState::Failed)
                    } else {
                        (id, NodeState::Completed)
                    }
                }));
            }

            for handle in handles {
                let (id, state) = handle.await.expect("task join");
                self.get_mut(id).unwrap().state = state;
                log.push((id, state));
                if state == NodeState::Failed {
                    failed_nodes.insert(id);
                    all_ok = false;
                }
            }
        }

        (log, all_ok)
    }
}

// ---------------------------------------------------------------------------
// Test: DAG executes in correct dependency order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dag_executes_in_dependency_order() {
    init_tracing();

    // Arrange: A -> B -> C (linear chain)
    let mut dag = TaskDag::new();
    let a = dag.add(TaskNode::new("A").with_duration(10));
    let b = dag.add(TaskNode::new("B").with_dep(a).with_duration(10));
    let c = dag.add(TaskNode::new("C").with_dep(b).with_duration(10));

    // Act
    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    // Assert: all completed, in order A before B before C.
    assert!(success);
    assert_eq!(log.len(), 3);

    let positions: HashMap<NodeId, usize> = log
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    assert!(positions[&a] < positions[&b], "A must execute before B");
    assert!(positions[&b] < positions[&c], "B must execute before C");
}

// ---------------------------------------------------------------------------
// Test: Parallel wave execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parallel_wave_execution() {
    init_tracing();

    // Arrange: diamond DAG
    //     A
    //    / \
    //   B   C
    //    \ /
    //     D
    let mut dag = TaskDag::new();
    let a = dag.add(TaskNode::new("A"));
    let b = dag.add(TaskNode::new("B").with_dep(a));
    let c = dag.add(TaskNode::new("C").with_dep(a));
    let d = dag.add(TaskNode::new("D").with_dep(b).with_dep(c));

    // Verify wave structure.
    let waves = dag.plan_waves();
    assert_eq!(waves.len(), 3, "should have 3 waves");
    assert!(waves[0].contains(&a), "wave 0 should contain A");
    assert!(
        waves[1].contains(&b) && waves[1].contains(&c),
        "wave 1 should contain B and C"
    );
    assert!(waves[2].contains(&d), "wave 2 should contain D");

    // Act
    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    // Assert: all completed.
    assert!(success);
    for (_, state) in &log {
        assert_eq!(*state, NodeState::Completed);
    }
}

// ---------------------------------------------------------------------------
// Test: Failure handling — dependents of failed node are skipped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failure_skips_dependent_nodes() {
    init_tracing();

    // Arrange: A -> B(fail) -> C, A -> D
    let mut dag = TaskDag::new();
    let a = dag.add(TaskNode::new("A"));
    let b = dag.add(TaskNode::new("B-fail").with_dep(a).failing());
    let c = dag.add(TaskNode::new("C").with_dep(b)); // should be skipped
    let d = dag.add(TaskNode::new("D").with_dep(a)); // should still execute

    // Act
    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    // Assert
    assert!(!success, "DAG should report failure");

    let states: HashMap<NodeId, NodeState> = log.into_iter().collect();
    assert_eq!(states[&a], NodeState::Completed, "A should complete");
    assert_eq!(states[&b], NodeState::Failed, "B should fail");
    assert_eq!(states[&c], NodeState::Skipped, "C should be skipped");
    assert_eq!(states[&d], NodeState::Completed, "D should complete");
}

// ---------------------------------------------------------------------------
// Test: Cancellation mid-execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancellation_mid_execution() {
    init_tracing();

    // Arrange: chain of 5 nodes, cancel after first wave.
    let mut dag = TaskDag::new();
    let n1 = dag.add(TaskNode::new("N1").with_duration(5));
    let n2 = dag.add(TaskNode::new("N2").with_dep(n1).with_duration(5));
    let n3 = dag.add(TaskNode::new("N3").with_dep(n2).with_duration(5));
    let n4 = dag.add(TaskNode::new("N4").with_dep(n3).with_duration(5));
    let n5 = dag.add(TaskNode::new("N5").with_dep(n4).with_duration(5));

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();

    // Schedule cancellation after a short delay (enough for first 2 waves).
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancel_clone.store(true, Ordering::Relaxed);
    });

    // Act
    let (log, success) = dag.execute(cancel).await;

    // Assert: not all nodes completed; at least some were cancelled.
    assert!(!success, "should not succeed after cancellation");

    let cancelled_count = log
        .iter()
        .filter(|(_, s)| *s == NodeState::Cancelled)
        .count();
    assert!(cancelled_count > 0, "at least one node should be cancelled");

    // Early nodes should have completed.
    let states: HashMap<NodeId, NodeState> = log.into_iter().collect();
    assert_eq!(states[&n1], NodeState::Completed, "N1 should complete before cancel");
}

// ---------------------------------------------------------------------------
// Test: Empty DAG
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_dag_succeeds() {
    init_tracing();

    let mut dag = TaskDag::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    assert!(success);
    assert!(log.is_empty());
}

// ---------------------------------------------------------------------------
// Test: Single node DAG
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_node_dag() {
    init_tracing();

    let mut dag = TaskDag::new();
    let solo = dag.add(TaskNode::new("solo"));

    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    assert!(success);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, solo);
    assert_eq!(log[0].1, NodeState::Completed);
}

// ---------------------------------------------------------------------------
// Test: Wide fan-out (many independent nodes)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wide_fanout_all_parallel() {
    init_tracing();

    let mut dag = TaskDag::new();
    let root = dag.add(TaskNode::new("root"));

    let mut leaves = Vec::new();
    for i in 0..20 {
        let leaf = dag.add(TaskNode::new(&format!("leaf-{i}")).with_dep(root));
        leaves.push(leaf);
    }

    let waves = dag.plan_waves();
    assert_eq!(waves.len(), 2, "root wave + leaf wave");
    assert_eq!(waves[1].len(), 20, "all leaves should be in one wave");

    let cancel = Arc::new(AtomicBool::new(false));
    let (log, success) = dag.execute(cancel).await;

    assert!(success);
    assert_eq!(log.len(), 21); // root + 20 leaves
}
