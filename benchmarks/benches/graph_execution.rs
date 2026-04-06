//! Graph execution benchmarks.
//!
//! Measures DAG construction, topological sort, wave planning, and critical
//! path calculation across varying graph sizes.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Graph sizes under test
// ---------------------------------------------------------------------------

const GRAPH_SIZES: &[usize] = &[10, 100, 1_000];

// ---------------------------------------------------------------------------
// Minimal in-benchmark DAG (mirrors the API daf-graph will provide)
// ---------------------------------------------------------------------------

/// A lightweight node for benchmarking graph algorithms.
#[derive(Debug, Clone)]
struct BenchNode {
    id: Uuid,
    deps: Vec<Uuid>,
}

/// A simple DAG for benchmarking.
#[derive(Debug, Clone)]
struct BenchDag {
    nodes: Vec<BenchNode>,
    adjacency: HashMap<Uuid, Vec<Uuid>>,   // parent -> children
    in_degree: HashMap<Uuid, usize>,
}

impl BenchDag {
    /// Build a DAG with `n` nodes. Each node (except the first) depends on
    /// one or two earlier nodes, creating a realistic dependency fan-out.
    fn build(n: usize) -> Self {
        let mut nodes = Vec::with_capacity(n);
        let mut adjacency: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        let mut in_degree: HashMap<Uuid, usize> = HashMap::new();

        for i in 0..n {
            let id = Uuid::now_v7();
            let mut deps = Vec::new();

            if i > 0 {
                // Depend on the immediately preceding node.
                let parent = nodes[i - 1].id;
                deps.push(parent);
                adjacency.entry(parent).or_default().push(id);

                // If there are enough nodes, add a second dependency for fan-in.
                if i > 2 && i % 3 == 0 {
                    let parent2 = nodes[i / 2].id;
                    if !deps.contains(&parent2) {
                        deps.push(parent2);
                        adjacency.entry(parent2).or_default().push(id);
                    }
                }
            }

            in_degree.insert(id, deps.len());
            adjacency.entry(id).or_default(); // ensure key exists
            nodes.push(BenchNode { id, deps });
        }

        Self { nodes, adjacency, in_degree }
    }

    /// Kahn's algorithm topological sort.
    fn topological_sort(&self) -> Vec<Uuid> {
        let mut in_deg = self.in_degree.clone();
        let mut queue: VecDeque<Uuid> = in_deg
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut sorted = Vec::with_capacity(self.nodes.len());

        while let Some(node) = queue.pop_front() {
            sorted.push(node);
            if let Some(children) = self.adjacency.get(&node) {
                for &child in children {
                    let deg = in_deg.get_mut(&child).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child);
                    }
                }
            }
        }
        sorted
    }

    /// Partition the sorted nodes into execution waves. Nodes in the same
    /// wave have no mutual dependencies and can execute in parallel.
    fn plan_waves(&self) -> Vec<Vec<Uuid>> {
        let mut depth: HashMap<Uuid, usize> = HashMap::new();
        let sorted = self.topological_sort();

        for &node_id in &sorted {
            let node = self.nodes.iter().find(|n| n.id == node_id).unwrap();
            let d = if node.deps.is_empty() {
                0
            } else {
                node.deps.iter().map(|dep| depth[dep] + 1).max().unwrap()
            };
            depth.insert(node_id, d);
        }

        let max_depth = depth.values().copied().max().unwrap_or(0);
        let mut waves: Vec<Vec<Uuid>> = vec![Vec::new(); max_depth + 1];
        for (&id, &d) in &depth {
            waves[d].push(id);
        }
        waves
    }

    /// Compute the critical path length (longest path through the DAG).
    fn critical_path_length(&self) -> usize {
        let mut longest: HashMap<Uuid, usize> = HashMap::new();
        let sorted = self.topological_sort();

        for &node_id in &sorted {
            let node = self.nodes.iter().find(|n| n.id == node_id).unwrap();
            let l = if node.deps.is_empty() {
                1
            } else {
                node.deps.iter().map(|dep| longest[dep]).max().unwrap() + 1
            };
            longest.insert(node_id, l);
        }

        longest.values().copied().max().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_dag_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/dag_construction");

    for &size in GRAPH_SIZES {
        group.bench_with_input(
            BenchmarkId::new("build", size),
            &size,
            |b, &n| {
                b.iter(|| {
                    let dag = BenchDag::build(n);
                    criterion::black_box(dag);
                });
            },
        );
    }
    group.finish();
}

fn bench_topological_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/topological_sort");

    for &size in GRAPH_SIZES {
        let dag = BenchDag::build(size);

        group.bench_with_input(
            BenchmarkId::new("kahn", size),
            &dag,
            |b, dag| {
                b.iter(|| {
                    let sorted = dag.topological_sort();
                    criterion::black_box(sorted);
                });
            },
        );
    }
    group.finish();
}

fn bench_wave_planning(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/wave_planning");

    for &size in GRAPH_SIZES {
        let dag = BenchDag::build(size);

        group.bench_with_input(
            BenchmarkId::new("plan_waves", size),
            &dag,
            |b, dag| {
                b.iter(|| {
                    let waves = dag.plan_waves();
                    criterion::black_box(waves);
                });
            },
        );
    }
    group.finish();
}

fn bench_critical_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/critical_path");

    for &size in GRAPH_SIZES {
        let dag = BenchDag::build(size);

        group.bench_with_input(
            BenchmarkId::new("longest_path", size),
            &dag,
            |b, dag| {
                b.iter(|| {
                    let length = dag.critical_path_length();
                    criterion::black_box(length);
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Supplementary: node lookup and adjacency traversal
// ---------------------------------------------------------------------------

fn bench_graph_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph/traversal");

    for &size in GRAPH_SIZES {
        let dag = BenchDag::build(size);

        // BFS from root
        group.bench_with_input(
            BenchmarkId::new("bfs_full", size),
            &dag,
            |b, dag| {
                let root = dag.nodes[0].id;
                b.iter(|| {
                    let mut visited = HashSet::new();
                    let mut queue = VecDeque::new();
                    queue.push_back(root);
                    while let Some(current) = queue.pop_front() {
                        if visited.insert(current) {
                            if let Some(children) = dag.adjacency.get(&current) {
                                for &child in children {
                                    queue.push_back(child);
                                }
                            }
                        }
                    }
                    criterion::black_box(visited.len());
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_dag_construction,
    bench_topological_sort,
    bench_wave_planning,
    bench_critical_path,
    bench_graph_traversal,
);
criterion_main!(benches);
