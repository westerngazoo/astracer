//! # lcw-core
//!
//! Shared domain types for Live Code Walk & Analysis. This crate is the
//! **API boundary** (Manifesto Principle I): every other crate depends on it,
//! and it depends on nothing heavy. Types here are deliberately flat and
//! `Copy` where possible (Principle II: data-driven, cache-friendly).

pub mod adapter;
pub mod diagnostic;
pub mod fragment;
pub mod graph;
pub mod ids;
pub mod metric;
pub mod report;
pub mod source;
pub mod suggestion;
pub mod vertical;

pub use adapter::{AdapterError, LanguageAdapter};
pub use diagnostic::{Diagnostic, Severity};
pub use fragment::{CallKind, FileFragment, RawCall};
pub use graph::{
    CodeGraph, Edge, EdgeExport, EdgeKind, GraphExport, GraphSnapshot, Node, NodeFlags, NodeKind,
    NodeStats, SourceSpan,
};
pub use ids::{FileId, NodeId};
pub use metric::{Metric, MetricKind};
pub use report::{AnalysisReport, ReportExport, ReportSnapshot, Summary};
pub use source::SourceFile;
pub use suggestion::{Suggestion, Target};
pub use vertical::Vertical;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_links_calls_to_defs() {
        let mut g = CodeGraph::new();
        let file = g.intern_file("src/lib.rs");

        // A call site references `foo` before we've seen its definition:
        // it should create an external placeholder...
        let caller = g.add_node(Node {
            id: NodeId(0),
            name: "main".into(),
            qualified_name: "crate::main".into(),
            module_path: "crate".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(file, 1, 0, 3, 1),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        });
        let foo_placeholder = g.intern_node("crate::foo", || Node::external("crate::foo"));
        g.add_edge(
            caller,
            foo_placeholder,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(file, 2, 4, 2, 9)),
        );

        // ...and when the real definition is added, the placeholder is upgraded
        // in place (same NodeId), so the edge stays valid.
        let foo_def = g.add_node(Node {
            id: NodeId(0),
            name: "foo".into(),
            qualified_name: "crate::foo".into(),
            module_path: "crate".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(file, 5, 0, 7, 1),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        });

        assert_eq!(foo_placeholder, foo_def);
        assert_eq!(g.node(foo_def).kind, NodeKind::Function);
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.in_degree(foo_def), 1);
        assert_eq!(g.out_degree(caller), 1);
    }

    #[test]
    fn duplicate_edges_merge_and_count() {
        let mut g = CodeGraph::new();
        let f = g.intern_file("a.rs");
        let a = g.add_node(Node::external("a"));
        let b = g.add_node(Node::external("b"));
        g.add_edge(
            a,
            b,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
        );
        g.add_edge(
            a,
            b,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 2, 0, 2, 1)),
        );
        assert_eq!(g.edge_count(), 1);
        let (_, _, e) = g.edges().next().unwrap();
        assert_eq!(e.count, 2);
    }

    #[test]
    fn snapshot_round_trips_through_serde() {
        // Build a small graph with a real def, an external target and an edge.
        let mut g = CodeGraph::new();
        let file = g.intern_file("src/lib.rs");
        let main = g.add_node(Node {
            id: NodeId(0),
            name: "main".into(),
            qualified_name: "crate::main".into(),
            module_path: "crate".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(file, 1, 0, 3, 1),
            flags: NodeFlags {
                is_pub: true,
                ..Default::default()
            },
            stats: NodeStats {
                decision_points: 2,
                ..Default::default()
            },
        });
        let dep = g.intern_node("std::println", || Node::external("std::println"));
        g.add_edge(
            main,
            dep,
            Edge::new(EdgeKind::MacroCall, SourceSpan::new(file, 2, 4, 2, 9)),
        );

        // snapshot -> JSON -> snapshot -> graph must preserve structure exactly.
        let snap = g.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: crate::GraphSnapshot = serde_json::from_str(&json).unwrap();
        let g2 = CodeGraph::from_snapshot(back);

        assert_eq!(g2.node_count(), g.node_count());
        assert_eq!(g2.edge_count(), g.edge_count());
        assert_eq!(g2.file_path(file), Some(std::path::Path::new("src/lib.rs")));
        let main2 = g2.node_by_qualified("crate::main").unwrap();
        assert_eq!(main2, main);
        assert_eq!(g2.node(main2).cyclomatic_complexity(), 3);
        assert!(g2.node(main2).flags.is_pub);
        assert_eq!(g2.out_degree(main2), 1);
        assert_eq!(
            g2.node(g2.node_by_qualified("std::println").unwrap()).kind,
            NodeKind::External
        );
        let (_, _, e) = g2.edges().next().unwrap();
        assert_eq!(e.kind, EdgeKind::MacroCall);
    }
}
