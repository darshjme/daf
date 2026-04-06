//! # DAF Graph
//!
//! DAG-based execution engine for the Darshj's Agent Framework.
//!
//! This crate provides the execution backbone — it determines what runs when
//! and in what order. Like Terraform's resource graph but for agent tasks,
//! it handles:
//!
//! - **Dependency resolution** — nodes declare dependencies; the graph enforces
//!   that predecessors complete before successors start.
//! - **Cycle detection** — the graph is validated as a DAG; cycles are rejected
//!   at edge-insertion time.
//! - **Topological ordering** — nodes are sorted so dependencies come first.
//! - **Wave-based parallel scheduling** — independent nodes are grouped into
//!   waves that execute concurrently, respecting concurrency limits.
//! - **Critical path analysis** — the longest path through the graph gives an
//!   ETA for the full execution.
//! - **Graph execution** — a tokio-based executor drives waves, tracks progress,
//!   handles timeouts, cancellation, and failure cascading.
//! - **Traversal utilities** — DFS/BFS iterators, ancestor/descendant queries.
//! - **Visualization** — export to Graphviz DOT and Mermaid diagram formats.
//!
//! # Quick start
//!
//! ```rust
//! use daf_graph::dag::ExecutionGraph;
//! use daf_graph::node::{Node, NodeKind};
//! use daf_graph::edge::{Edge, EdgeKind};
//! use daf_graph::scheduler::WaveScheduler;
//!
//! let mut graph = ExecutionGraph::new("my-pipeline");
//!
//! let fetch = graph.add_node(Node::new(NodeKind::Task, "fetch-data"));
//! let process = graph.add_node(Node::new(NodeKind::Task, "process"));
//! let store = graph.add_node(Node::new(NodeKind::Task, "store-results"));
//!
//! graph.add_edge(Edge::new(fetch, process, EdgeKind::DependsOn)).unwrap();
//! graph.add_edge(Edge::new(process, store, EdgeKind::DependsOn)).unwrap();
//!
//! // Plan execution waves.
//! let plan = WaveScheduler::new().plan_waves(&graph).unwrap();
//! assert_eq!(plan.waves.len(), 3); // linear chain = 3 sequential waves
//! ```

pub mod dag;
pub mod edge;
pub mod error;
pub mod executor;
pub mod node;
pub mod scheduler;
pub mod visualize;
pub mod walker;

// Re-export the most commonly used types at crate root.
pub use dag::ExecutionGraph;
pub use edge::{Edge, EdgeCondition, EdgeKind};
pub use error::GraphError;
pub use executor::{ExecutionProgress, GraphExecutor, TaskHandler};
pub use node::{Node, NodeHandle, NodeId, NodeKind, NodeState, TaskSpec};
pub use scheduler::{ExecutionPlan, Wave, WaveScheduler};
pub use visualize::{to_dot, to_mermaid};
pub use walker::{ancestors, descendants, leaves, roots, BreadthFirst, DepthFirst};
