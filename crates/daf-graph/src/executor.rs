//! Graph execution engine.
//!
//! Drives the full lifecycle of an execution graph: plans waves via the
//! scheduler, spawns tokio tasks for parallel execution within each wave,
//! tracks progress, handles failures, and supports cancellation.

use crate::dag::ExecutionGraph;
use crate::error::GraphError;
use crate::node::{NodeId, NodeState};
use crate::scheduler::WaveScheduler;

use parking_lot::RwLock;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

// ---------------------------------------------------------------------------
// Callback types
// ---------------------------------------------------------------------------

/// Boxed async callback for node lifecycle events.
pub type NodeCallback = Box<
    dyn Fn(NodeId) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Boxed async callback for node failure events (includes error message).
pub type NodeFailCallback = Box<
    dyn Fn(NodeId, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Boxed async callback for wave completion events.
pub type WaveCallback = Box<
    dyn Fn(usize) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Task handler: given a node ID, execute the node's work.
/// Returns `Ok(())` on success or `Err(message)` on failure.
pub type TaskHandler = Arc<
    dyn Fn(NodeId) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Real-time progress snapshot of graph execution.
#[derive(Debug, Clone)]
pub struct ExecutionProgress {
    /// Number of nodes that have reached a terminal state.
    pub nodes_completed: usize,
    /// Total number of nodes in the graph.
    pub nodes_total: usize,
    /// Current wave being executed (0-indexed).
    pub current_wave: usize,
    /// Total number of waves in the plan.
    pub total_waves: usize,
    /// Wall-clock time since execution started.
    pub elapsed: Duration,
    /// Number of nodes currently running.
    pub nodes_running: usize,
    /// Number of nodes that failed.
    pub nodes_failed: usize,
    /// Number of nodes that were skipped.
    pub nodes_skipped: usize,
}

impl ExecutionProgress {
    /// Completion percentage (0.0 to 1.0).
    pub fn completion_ratio(&self) -> f64 {
        if self.nodes_total == 0 {
            1.0
        } else {
            self.nodes_completed as f64 / self.nodes_total as f64
        }
    }
}

// ---------------------------------------------------------------------------
// GraphExecutor
// ---------------------------------------------------------------------------

/// Executes an `ExecutionGraph` by driving waves of parallel tasks.
///
/// The executor owns the graph (wrapped in `Arc<RwLock>` for shared access
/// from spawned tasks) and a task handler that maps node IDs to actual work.
pub struct GraphExecutor {
    /// The execution graph, shared with spawned tasks.
    graph: Arc<RwLock<ExecutionGraph>>,
    /// The function that executes a node's work.
    task_handler: TaskHandler,
    /// Cancellation token for cooperative shutdown.
    cancel_token: CancellationToken,
    /// Maximum concurrency (limits how many tasks run in parallel).
    max_concurrency: Option<usize>,
    /// Per-node timeout. If `None`, nodes can run indefinitely.
    node_timeout: Option<Duration>,
    /// Global execution timeout. If `None`, execution can run indefinitely.
    global_timeout: Option<Duration>,

    // Callbacks — all optional.
    on_node_start: Option<NodeCallback>,
    on_node_complete: Option<NodeCallback>,
    on_node_fail: Option<NodeFailCallback>,
    on_wave_complete: Option<WaveCallback>,
}

impl GraphExecutor {
    /// Create a new executor for the given graph and task handler.
    pub fn new(graph: ExecutionGraph, task_handler: TaskHandler) -> Self {
        Self {
            graph: Arc::new(RwLock::new(graph)),
            task_handler,
            cancel_token: CancellationToken::new(),
            max_concurrency: None,
            node_timeout: None,
            global_timeout: None,
            on_node_start: None,
            on_node_complete: None,
            on_node_fail: None,
            on_wave_complete: None,
        }
    }

    /// Set the cancellation token (allows external cancellation).
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = token;
        self
    }

    /// Set maximum concurrency.
    pub fn with_max_concurrency(mut self, max: usize) -> Self {
        self.max_concurrency = Some(max);
        self
    }

    /// Set per-node timeout.
    pub fn with_node_timeout(mut self, timeout: Duration) -> Self {
        self.node_timeout = Some(timeout);
        self
    }

    /// Set global execution timeout.
    pub fn with_global_timeout(mut self, timeout: Duration) -> Self {
        self.global_timeout = Some(timeout);
        self
    }

    /// Set callback for node start events.
    pub fn on_node_start(mut self, cb: NodeCallback) -> Self {
        self.on_node_start = Some(cb);
        self
    }

    /// Set callback for node completion events.
    pub fn on_node_complete(mut self, cb: NodeCallback) -> Self {
        self.on_node_complete = Some(cb);
        self
    }

    /// Set callback for node failure events.
    pub fn on_node_fail(mut self, cb: NodeFailCallback) -> Self {
        self.on_node_fail = Some(cb);
        self
    }

    /// Set callback for wave completion events.
    pub fn on_wave_complete(mut self, cb: WaveCallback) -> Self {
        self.on_wave_complete = Some(cb);
        self
    }

    /// Get the cancellation token (for external code to trigger cancellation).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Get a snapshot of current execution progress.
    pub fn progress(&self) -> ExecutionProgress {
        let graph = self.graph.read();
        let mut completed = 0;
        let mut running = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let total = graph.node_count();

        for node in graph.nodes() {
            match node.state {
                NodeState::Succeeded => completed += 1,
                NodeState::Failed => {
                    completed += 1;
                    failed += 1;
                }
                NodeState::Skipped => {
                    completed += 1;
                    skipped += 1;
                }
                NodeState::Cancelled => {
                    completed += 1;
                }
                NodeState::Running => running += 1,
                _ => {}
            }
        }

        ExecutionProgress {
            nodes_completed: completed,
            nodes_total: total,
            current_wave: 0, // Updated during execution.
            total_waves: 0,
            elapsed: Duration::ZERO,
            nodes_running: running,
            nodes_failed: failed,
            nodes_skipped: skipped,
        }
    }

    /// Execute the full graph, respecting dependencies, concurrency limits,
    /// timeouts, and cancellation.
    ///
    /// Returns the final execution progress on success, or a `GraphError`
    /// if the execution failed fatally (timeout, cancellation, etc.).
    #[instrument(skip(self), fields(graph_name))]
    pub async fn execute(&self) -> Result<ExecutionProgress, GraphError> {
        let start = Instant::now();
        let graph_name = self.graph.read().name.clone();
        info!(graph = %graph_name, "starting graph execution");

        // Plan waves.
        let scheduler = match self.max_concurrency {
            Some(max) => WaveScheduler::with_max_concurrency(max),
            None => WaveScheduler::new(),
        };

        let plan = {
            let graph = self.graph.read();
            scheduler.plan_waves(&graph)?
        };

        if plan.is_empty() {
            info!("graph is empty or fully completed, nothing to execute");
            return Ok(self.progress());
        }

        info!(
            waves = plan.waves.len(),
            nodes = plan.total_nodes(),
            estimated = ?plan.total_estimated_duration,
            parallelism = plan.parallelism_factor,
            "execution plan ready"
        );

        let concurrency_semaphore = self
            .max_concurrency
            .map(|max| Arc::new(Semaphore::new(max)));

        // Execute wave by wave.
        for wave in &plan.waves {
            // Check cancellation.
            if self.cancel_token.is_cancelled() {
                warn!("execution cancelled before wave {}", wave.wave_number);
                self.cancel_remaining_nodes();
                return Err(GraphError::Cancelled);
            }

            // Check global timeout.
            if let Some(global_timeout) = self.global_timeout {
                if start.elapsed() > global_timeout {
                    warn!("global timeout exceeded");
                    self.cancel_remaining_nodes();
                    return Err(GraphError::GlobalTimeout(global_timeout));
                }
            }

            debug!(wave = wave.wave_number, nodes = wave.nodes.len(), "executing wave");

            // Mark all wave nodes as Running.
            {
                let mut graph = self.graph.write();
                for &node_id in &wave.nodes {
                    if let Some(node) = graph.get_node_mut(node_id) {
                        // Only run nodes that are still Pending (skip already-skipped ones).
                        if node.state == NodeState::Pending {
                            node.state = NodeState::Ready;
                        }
                    }
                }
            }

            // Filter to only nodes that are Ready (not skipped by cascade).
            let runnable: Vec<NodeId> = {
                let graph = self.graph.read();
                wave.nodes
                    .iter()
                    .copied()
                    .filter(|id| {
                        graph
                            .get_node(*id)
                            .is_some_and(|n| n.state == NodeState::Ready)
                    })
                    .collect()
            };

            // Spawn tasks for all runnable nodes in this wave.
            let mut handles = Vec::with_capacity(runnable.len());
            for node_id in runnable {
                let graph = Arc::clone(&self.graph);
                let handler = Arc::clone(&self.task_handler);
                let cancel = self.cancel_token.clone();
                let node_timeout = self.node_timeout;
                let semaphore = concurrency_semaphore.clone();

                // Fire on_node_start callback.
                if let Some(ref cb) = self.on_node_start {
                    cb(node_id).await;
                }

                // Mark as Running.
                {
                    let mut g = graph.write();
                    if let Some(node) = g.get_node_mut(node_id) {
                        node.state = NodeState::Running;
                    }
                }

                let handle = tokio::spawn(async move {
                    // Acquire semaphore permit if concurrency-limited.
                    let _permit = if let Some(ref sem) = semaphore {
                        Some(sem.acquire().await.expect("semaphore closed"))
                    } else {
                        None
                    };

                    let node_start = Instant::now();

                    // Execute with optional timeout and cancellation.
                    let result = tokio::select! {
                        _ = cancel.cancelled() => {
                            Err("cancelled".to_string())
                        }
                        result = async {
                            if let Some(timeout) = node_timeout {
                                match tokio::time::timeout(timeout, handler(node_id)).await {
                                    Ok(r) => r,
                                    Err(_) => Err(format!("timed out after {timeout:?}")),
                                }
                            } else {
                                handler(node_id).await
                            }
                        } => {
                            result
                        }
                    };

                    let elapsed = node_start.elapsed();

                    // Update node state.
                    let mut g = graph.write();
                    match &result {
                        Ok(()) => {
                            let _ = g.mark_complete(node_id, NodeState::Succeeded, Some(elapsed));
                        }
                        Err(msg) => {
                            if msg == "cancelled" {
                                let _ = g.mark_complete(node_id, NodeState::Cancelled, Some(elapsed));
                            } else {
                                let _ = g.mark_complete(node_id, NodeState::Failed, Some(elapsed));
                            }
                        }
                    }

                    (node_id, result, elapsed)
                });

                handles.push(handle);
            }

            // Wait for all tasks in this wave to complete.
            for handle in handles {
                match handle.await {
                    Ok((node_id, result, _elapsed)) => match result {
                        Ok(()) => {
                            if let Some(ref cb) = self.on_node_complete {
                                cb(node_id).await;
                            }
                        }
                        Err(msg) => {
                            if let Some(ref cb) = self.on_node_fail {
                                cb(node_id, msg).await;
                            }
                        }
                    },
                    Err(e) => {
                        error!("task panicked: {e}");
                    }
                }
            }

            // Fire on_wave_complete callback.
            if let Some(ref cb) = self.on_wave_complete {
                cb(wave.wave_number).await;
            }
        }

        let elapsed = start.elapsed();
        info!(elapsed = ?elapsed, "graph execution complete");

        let mut progress = self.progress();
        progress.elapsed = elapsed;
        progress.total_waves = plan.waves.len();
        Ok(progress)
    }

    /// Mark all non-terminal nodes as Cancelled.
    fn cancel_remaining_nodes(&self) {
        let mut graph = self.graph.write();
        let ids: Vec<NodeId> = graph
            .nodes()
            .filter(|n| !n.state.is_terminal() && n.state != NodeState::Running)
            .map(|n| n.id)
            .collect();
        for id in ids {
            if let Some(node) = graph.get_node_mut(id) {
                node.state = NodeState::Cancelled;
            }
        }
    }

    /// Get a read-only reference to the underlying graph.
    pub fn graph(&self) -> &Arc<RwLock<ExecutionGraph>> {
        &self.graph
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn noop_handler() -> TaskHandler {
        Arc::new(|_| Box::pin(async { Ok(()) }))
    }

    fn counting_handler(counter: Arc<AtomicUsize>) -> TaskHandler {
        Arc::new(move |_| {
            let c = counter.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })
    }

    fn make_task(name: &str) -> Node {
        Node::new(NodeKind::Task, name)
    }

    #[tokio::test]
    async fn execute_empty_graph() {
        let g = ExecutionGraph::new("empty");
        let executor = GraphExecutor::new(g, noop_handler());
        let progress = executor.execute().await.unwrap();
        assert_eq!(progress.nodes_total, 0);
    }

    #[tokio::test]
    async fn execute_single_node() {
        let mut g = ExecutionGraph::new("single");
        g.add_node(make_task("only"));
        let counter = Arc::new(AtomicUsize::new(0));
        let executor = GraphExecutor::new(g, counting_handler(counter.clone()));
        let progress = executor.execute().await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(progress.nodes_completed, 1);
    }

    #[tokio::test]
    async fn execute_linear_chain() {
        let mut g = ExecutionGraph::new("chain");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DependsOn)).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        let executor = GraphExecutor::new(g, counting_handler(counter.clone()));
        let progress = executor.execute().await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert_eq!(progress.nodes_completed, 3);
    }

    #[tokio::test]
    async fn execute_with_failure() {
        let mut g = ExecutionGraph::new("fail");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();

        // Handler that fails on "a".
        let fail_a = a;
        let handler: TaskHandler = Arc::new(move |id| {
            Box::pin(async move {
                if id == fail_a {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            })
        });

        let executor = GraphExecutor::new(g, handler);
        let progress = executor.execute().await.unwrap();
        // a failed, b should be skipped.
        assert_eq!(progress.nodes_failed, 1);
        assert_eq!(progress.nodes_skipped, 1);
    }

    #[tokio::test]
    async fn execute_cancellation() {
        let mut g = ExecutionGraph::new("cancel");
        g.add_node(make_task("slow"));

        // Handler that sleeps forever.
        let handler: TaskHandler = Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(())
            })
        });

        let token = CancellationToken::new();
        let executor = GraphExecutor::new(g, handler)
            .with_cancel_token(token.clone());

        // Cancel after a short delay.
        let cancel_token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_token.cancel();
        });

        let result = executor.execute().await;
        // The node itself may have been cancelled or the wave loop caught it.
        // Either way, we should not hang.
        assert!(
            result.is_ok() || matches!(result, Err(GraphError::Cancelled)),
            "expected ok or cancelled, got {result:?}"
        );
    }

    #[tokio::test]
    async fn execute_with_node_timeout() {
        let mut g = ExecutionGraph::new("timeout");
        g.add_node(make_task("slow"));

        let handler: TaskHandler = Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(())
            })
        });

        let executor = GraphExecutor::new(g, handler)
            .with_node_timeout(Duration::from_millis(50));

        let progress = executor.execute().await.unwrap();
        assert_eq!(progress.nodes_failed, 1);
    }

    #[tokio::test]
    async fn progress_completion_ratio() {
        let progress = ExecutionProgress {
            nodes_completed: 3,
            nodes_total: 10,
            current_wave: 1,
            total_waves: 3,
            elapsed: Duration::from_secs(5),
            nodes_running: 2,
            nodes_failed: 0,
            nodes_skipped: 0,
        };
        assert!((progress.completion_ratio() - 0.3).abs() < f64::EPSILON);
    }
}
