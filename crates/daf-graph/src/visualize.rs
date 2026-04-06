//! Graph visualization: export to Graphviz DOT and Mermaid diagram formats.
//!
//! Color-codes nodes by state and labels edges with their kind.
//! Useful for debugging execution plans and generating documentation.

use crate::dag::ExecutionGraph;
use crate::edge::EdgeKind;
use crate::node::NodeState;

use petgraph::visit::EdgeRef;
use petgraph::Direction;
use std::fmt::Write;

// ---------------------------------------------------------------------------
// DOT export
// ---------------------------------------------------------------------------

/// Export the execution graph to Graphviz DOT format.
///
/// Nodes are colored by their current state:
/// - Green: Succeeded
/// - Red: Failed
/// - Yellow: Running
/// - Blue: Ready
/// - Gray: Pending / Noop
/// - Orange: Skipped
/// - Dark gray: Cancelled
///
/// Edges are labeled with their kind.
pub fn to_dot(graph: &ExecutionGraph) -> String {
    let mut out = String::new();
    writeln!(out, "digraph \"{}\" {{", graph.name).unwrap();
    writeln!(out, "    rankdir=TB;").unwrap();
    writeln!(out, "    node [shape=box, style=filled, fontname=\"Helvetica\"];").unwrap();
    writeln!(out, "    edge [fontname=\"Helvetica\", fontsize=10];").unwrap();
    writeln!(out).unwrap();

    let inner = graph.inner();

    // Nodes.
    for idx in inner.node_indices() {
        let node = &inner[idx];
        let color = state_color_dot(node.state);
        let font_color = state_font_color(node.state);
        writeln!(
            out,
            "    \"{}\" [label=\"{}\\n({})\\n[{}]\", fillcolor=\"{}\", fontcolor=\"{}\"];",
            node.id.0, node.name, node.kind, node.state, color, font_color
        )
        .unwrap();
    }

    writeln!(out).unwrap();

    // Edges.
    for idx in inner.node_indices() {
        for edge_ref in inner.edges_directed(idx, Direction::Outgoing) {
            let edge = edge_ref.weight();
            let source = &inner[edge_ref.source()];
            let target = &inner[edge_ref.target()];
            let style = edge_style_dot(&edge.kind);
            let label = format!("{}", edge.kind);
            writeln!(
                out,
                "    \"{}\" -> \"{}\" [label=\"{}\", {}];",
                source.id.0, target.id.0, label, style
            )
            .unwrap();
        }
    }

    writeln!(out, "}}").unwrap();
    out
}

/// Map node state to DOT fill color.
fn state_color_dot(state: NodeState) -> &'static str {
    match state {
        NodeState::Pending => "#e0e0e0",
        NodeState::Ready => "#bbdefb",
        NodeState::Running => "#fff9c4",
        NodeState::Succeeded => "#c8e6c9",
        NodeState::Failed => "#ffcdd2",
        NodeState::Skipped => "#ffe0b2",
        NodeState::Cancelled => "#bdbdbd",
    }
}

/// Map node state to DOT font color for contrast.
fn state_font_color(state: NodeState) -> &'static str {
    match state {
        NodeState::Pending => "#616161",
        NodeState::Ready => "#1565c0",
        NodeState::Running => "#f57f17",
        NodeState::Succeeded => "#2e7d32",
        NodeState::Failed => "#c62828",
        NodeState::Skipped => "#e65100",
        NodeState::Cancelled => "#424242",
    }
}

/// Map edge kind to DOT style attributes.
fn edge_style_dot(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::DependsOn => "style=solid, color=\"#333333\"",
        EdgeKind::Triggers => "style=dashed, color=\"#1976d2\"",
        EdgeKind::Blocks => "style=bold, color=\"#c62828\"",
        EdgeKind::DataFlow => "style=dotted, color=\"#388e3c\"",
        EdgeKind::Conditional => "style=dashed, color=\"#f57c00\"",
    }
}

// ---------------------------------------------------------------------------
// Mermaid export
// ---------------------------------------------------------------------------

/// Export the execution graph to Mermaid flowchart syntax.
///
/// Nodes are styled with CSS classes based on their state.
/// Compatible with GitHub-flavored Markdown Mermaid blocks.
pub fn to_mermaid(graph: &ExecutionGraph) -> String {
    let mut out = String::new();
    writeln!(out, "flowchart TD").unwrap();

    let inner = graph.inner();

    // Nodes.
    for idx in inner.node_indices() {
        let node = &inner[idx];
        let id = sanitize_mermaid_id(&node.id.0.to_string());
        let label = format!("{}<br/><small>{} | {}</small>", node.name, node.kind, node.state);
        // Use different node shapes based on kind.
        let shape = match node.kind {
            crate::node::NodeKind::Task => format!("    {id}[\"{label}\"]"),
            crate::node::NodeKind::Gate => format!("    {id}{{\"{label}\"}}"),
            crate::node::NodeKind::Fork => format!("    {id}([\"{label}\"])"),
            crate::node::NodeKind::Join => format!("    {id}([\"{label}\"])"),
            crate::node::NodeKind::Checkpoint => format!("    {id}[[\"{label}\"]]"),
            crate::node::NodeKind::Noop => format!("    {id}(\"{label}\")"),
        };
        writeln!(out, "{shape}").unwrap();
    }

    writeln!(out).unwrap();

    // Edges.
    for idx in inner.node_indices() {
        for edge_ref in inner.edges_directed(idx, Direction::Outgoing) {
            let edge = edge_ref.weight();
            let source = &inner[edge_ref.source()];
            let target = &inner[edge_ref.target()];
            let src_id = sanitize_mermaid_id(&source.id.0.to_string());
            let tgt_id = sanitize_mermaid_id(&target.id.0.to_string());
            let label = format!("{}", edge.kind);
            let arrow = match edge.kind {
                EdgeKind::DependsOn => "-->",
                EdgeKind::Triggers => "-.->",
                EdgeKind::Blocks => "==>",
                EdgeKind::DataFlow => "-.->",
                EdgeKind::Conditional => "-.->",
            };
            writeln!(out, "    {src_id} {arrow}|{label}| {tgt_id}").unwrap();
        }
    }

    writeln!(out).unwrap();

    // Style classes.
    writeln!(out, "    classDef pending fill:#e0e0e0,stroke:#999,color:#616161").unwrap();
    writeln!(out, "    classDef ready fill:#bbdefb,stroke:#1976d2,color:#1565c0").unwrap();
    writeln!(out, "    classDef running fill:#fff9c4,stroke:#f9a825,color:#f57f17").unwrap();
    writeln!(out, "    classDef succeeded fill:#c8e6c9,stroke:#4caf50,color:#2e7d32").unwrap();
    writeln!(out, "    classDef failed fill:#ffcdd2,stroke:#ef5350,color:#c62828").unwrap();
    writeln!(out, "    classDef skipped fill:#ffe0b2,stroke:#ff9800,color:#e65100").unwrap();
    writeln!(out, "    classDef cancelled fill:#bdbdbd,stroke:#757575,color:#424242").unwrap();

    // Apply classes.
    for idx in inner.node_indices() {
        let node = &inner[idx];
        let id = sanitize_mermaid_id(&node.id.0.to_string());
        let class = state_class_mermaid(node.state);
        writeln!(out, "    class {id} {class}").unwrap();
    }

    out
}

/// Map node state to Mermaid CSS class name.
fn state_class_mermaid(state: NodeState) -> &'static str {
    match state {
        NodeState::Pending => "pending",
        NodeState::Ready => "ready",
        NodeState::Running => "running",
        NodeState::Succeeded => "succeeded",
        NodeState::Failed => "failed",
        NodeState::Skipped => "skipped",
        NodeState::Cancelled => "cancelled",
    }
}

/// Sanitize a UUID string into a valid Mermaid node ID.
/// Replaces hyphens with underscores and prepends 'n' to avoid
/// starting with a digit.
fn sanitize_mermaid_id(uuid_str: &str) -> String {
    format!("n{}", uuid_str.replace('-', "_"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::{Edge, EdgeKind};
    use crate::node::{Node, NodeKind, NodeState};

    fn make_task(name: &str) -> Node {
        Node::new(NodeKind::Task, name)
    }

    fn build_test_graph() -> ExecutionGraph {
        let mut g = ExecutionGraph::new("test-viz");
        let a = g.add_node(make_task("fetch-data"));
        let b = g.add_node(make_task("process"));
        let c = g.add_node(make_task("store"));
        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::DataFlow)).unwrap();

        // Simulate some state changes.
        g.get_node_mut(a).unwrap().state = NodeState::Succeeded;
        g.get_node_mut(b).unwrap().state = NodeState::Running;

        g
    }

    #[test]
    fn dot_output_is_valid() {
        let g = build_test_graph();
        let dot = to_dot(&g);

        assert!(dot.starts_with("digraph"));
        assert!(dot.contains("fetch-data"));
        assert!(dot.contains("process"));
        assert!(dot.contains("store"));
        assert!(dot.contains("depends_on"));
        assert!(dot.contains("data_flow"));
        assert!(dot.contains("#c8e6c9")); // succeeded color
        assert!(dot.contains("#fff9c4")); // running color
        assert!(dot.ends_with("}\n"));
    }

    #[test]
    fn mermaid_output_is_valid() {
        let g = build_test_graph();
        let mmd = to_mermaid(&g);

        assert!(mmd.starts_with("flowchart TD"));
        assert!(mmd.contains("fetch-data"));
        assert!(mmd.contains("process"));
        assert!(mmd.contains("store"));
        assert!(mmd.contains("depends_on"));
        assert!(mmd.contains("data_flow"));
        assert!(mmd.contains("classDef succeeded"));
        assert!(mmd.contains("classDef running"));
    }

    #[test]
    fn mermaid_id_sanitization() {
        let id = sanitize_mermaid_id("550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(id, "n550e8400_e29b_41d4_a716_446655440000");
        // Should not start with a digit.
        assert!(id.starts_with('n'));
        // Should not contain hyphens.
        assert!(!id.contains('-'));
    }

    #[test]
    fn empty_graph_dot() {
        let g = ExecutionGraph::new("empty");
        let dot = to_dot(&g);
        assert!(dot.contains("digraph"));
        assert!(dot.contains('}'));
    }

    #[test]
    fn empty_graph_mermaid() {
        let g = ExecutionGraph::new("empty");
        let mmd = to_mermaid(&g);
        assert!(mmd.contains("flowchart TD"));
    }

    #[test]
    fn gate_node_mermaid_shape() {
        let mut g = ExecutionGraph::new("gate");
        g.add_node(Node::new(NodeKind::Gate, "check-auth"));
        let mmd = to_mermaid(&g);
        // Gate nodes use diamond shape {}.
        assert!(mmd.contains('{'));
    }

    #[test]
    fn all_edge_kinds_in_dot() {
        let mut g = ExecutionGraph::new("all-edges");
        let a = g.add_node(make_task("a"));
        let b = g.add_node(make_task("b"));
        let c = g.add_node(make_task("c"));
        let d = g.add_node(make_task("d"));
        let e = g.add_node(make_task("e"));
        let f = g.add_node(make_task("f"));

        g.add_edge(Edge::new(a, b, EdgeKind::DependsOn)).unwrap();
        g.add_edge(Edge::new(b, c, EdgeKind::Triggers)).unwrap();
        g.add_edge(Edge::new(c, d, EdgeKind::Blocks)).unwrap();
        g.add_edge(Edge::new(d, e, EdgeKind::DataFlow)).unwrap();
        g.add_edge(Edge::new(e, f, EdgeKind::Conditional)).unwrap();

        let dot = to_dot(&g);
        assert!(dot.contains("depends_on"));
        assert!(dot.contains("triggers"));
        assert!(dot.contains("blocks"));
        assert!(dot.contains("data_flow"));
        assert!(dot.contains("conditional"));
    }
}
