//! # lcw-query
//!
//! **Pure, front-end-agnostic graph queries** over a [`CodeGraph`]. This crate
//! is the single home for "navigation" logic — the questions a person asks
//! while walking an unfamiliar codebase:
//!
//! * *Where do I start?* — [`entry_points`] (`main`, uncalled roots, tests).
//! * *What is the shape of the code?* — [`outline`] (crate ▸ module ▸ type ▸ fn).
//! * *What comes in, what goes out?* — [`node_card`] (callers/callees with the
//!   edge kind, multiplicity and call site).
//! * *How does control get from here to there?* — [`shortest_paths`].
//! * *What does this trigger / who can trigger this?* — [`call_tree`],
//!   [`reachable`].
//!
//! It depends only on `lcw-core` + `serde` and does no I/O, so it compiles for
//! the CLI, the native `winit` viewer **and** the `wasm32` webview alike
//! (Manifesto Principle I: one closed interface, many front ends). Front ends
//! must never re-implement traversal; they render what this crate returns.
//!
//! Everything here is `O(n + e)` or better per query (breadth-first searches
//! over the adjacency lists) and allocates only the result it hands back
//! (Principle II).

pub mod card;
pub mod entries;
pub mod flow;
pub mod outline;
pub mod reach;
pub mod resolve;

pub use card::{edge_kind_str, flags_of, kind_str, node_card, CallRef, CardMetrics, NodeCard};
pub use entries::{classify_entry, entry_points, mains, EntryKind, EntryPoint};
pub use flow::{shortest_path, shortest_paths, FlowPaths};
pub use outline::{outline, Outline, OutlineKind, OutlineNode};
pub use reach::{
    call_tree, reach_count, reachable, CallTreeNode, CallTreeOptions, Direction, Reached,
};
pub use resolve::{best_match, resolve};

/// Shared test fixtures: small hand-built graphs used across the module tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use lcw_core::{
        CodeGraph, Edge, EdgeKind, FileId, Node, NodeFlags, NodeId, NodeKind, NodeStats, SourceSpan,
    };

    /// Add a definition node. The module path is everything before the last
    /// `::`; the file is interned from `file`, the span starts at `line`.
    pub fn def(g: &mut CodeGraph, qn: &str, file: &str, line: u32, flags: NodeFlags) -> NodeId {
        let f = g.intern_file(file);
        let name = qn.rsplit("::").next().unwrap_or(qn).to_string();
        let module_path = qn
            .rsplit_once("::")
            .map(|(m, _)| m.to_string())
            .unwrap_or_default();
        g.add_node(Node {
            id: NodeId(0),
            name,
            qualified_name: qn.to_string(),
            module_path,
            kind: if flags.is_method {
                NodeKind::Method
            } else {
                NodeKind::Function
            },
            span: SourceSpan::new(f, line, 0, line + 3, 1),
            flags,
            stats: NodeStats {
                lines_of_code: 4,
                decision_points: 2,
                parameters: 1,
                ..Default::default()
            },
        })
    }

    pub fn plain(g: &mut CodeGraph, qn: &str, file: &str, line: u32) -> NodeId {
        def(g, qn, file, line, NodeFlags::default())
    }

    pub fn call(g: &mut CodeGraph, from: NodeId, to: NodeId, kind: EdgeKind, line: u32) {
        let file = g.node(from).span.file();
        g.add_edge(
            from,
            to,
            Edge::new(kind, SourceSpan::new(file, line, 4, line, 20)),
        );
    }

    /// A small "application":
    ///
    /// ```text
    /// app::main ──▶ app::run ──▶ app::io::load ──▶ std::fs::read (external)
    ///     │              │
    ///     │              └─────▶ app::core::parse ──▶ app::core::Lexer::next
    ///     └───────▶ app::io::save            ▲
    ///                                        │
    ///        app::core::tests::test_parse ───┘   app::orphan (uncalled)
    /// ```
    pub fn app_graph() -> CodeGraph {
        let mut g = CodeGraph::new();
        let pub_flag = NodeFlags {
            is_pub: true,
            ..Default::default()
        };
        let main = plain(&mut g, "app::main", "src/main.rs", 10);
        let run = def(&mut g, "app::run", "src/main.rs", 20, pub_flag);
        let load = def(&mut g, "app::io::load", "src/io.rs", 5, pub_flag);
        let save = def(&mut g, "app::io::save", "src/io.rs", 30, pub_flag);
        let parse = def(&mut g, "app::core::parse", "src/core.rs", 8, pub_flag);
        let next = def(
            &mut g,
            "app::core::Lexer::next",
            "src/core.rs",
            40,
            NodeFlags {
                is_method: true,
                ..Default::default()
            },
        );
        let test = def(
            &mut g,
            "app::core::tests::test_parse",
            "src/core.rs",
            80,
            NodeFlags {
                is_test: true,
                ..Default::default()
            },
        );
        let _orphan = plain(&mut g, "app::orphan", "src/main.rs", 60);
        let read = g.add_node(Node::external("std::fs::read"));

        call(&mut g, main, run, EdgeKind::DirectCall, 11);
        call(&mut g, main, save, EdgeKind::DirectCall, 12);
        call(&mut g, run, load, EdgeKind::DirectCall, 21);
        call(&mut g, run, parse, EdgeKind::DirectCall, 22);
        call(&mut g, run, parse, EdgeKind::DirectCall, 23); // merged: count 2
        call(&mut g, load, read, EdgeKind::Unresolved, 6);
        call(&mut g, parse, next, EdgeKind::MethodCall, 9);
        call(&mut g, test, parse, EdgeKind::DirectCall, 81);
        assert_eq!(g.file_count(), 3);
        assert_eq!(g.node(main).span.file(), FileId(0));
        g
    }

    pub fn id(g: &CodeGraph, qn: &str) -> NodeId {
        g.node_by_qualified(qn)
            .unwrap_or_else(|| panic!("missing {qn}"))
    }
}
