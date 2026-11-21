//! Drop test and example code from a graph (`--exclude-tests`), so the top
//! lists, callers, impact and search describe the shipped program only.
//!
//! Layer 1 flags the nodes ([`lcw_core::NodeFlags::is_test`] /
//! [`lcw_core::NodeFlags::is_example`]); this only rebuilds the graph without
//! them. Node ids are renumbered, so a filtered graph never mixes with ids
//! taken from the unfiltered one.

use lcw_core::{CodeGraph, GraphSnapshot, Node, NodeId, NodeKind};

/// Which kinds of non-shipping code to drop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Exclusions {
    /// `#[test]` fns, `#[cfg(test)]` modules, `tests/` dirs, `test_*.py`,
    /// `*_test.go`, `*.test.ts` / `*.spec.ts`.
    pub tests: bool,
    /// `examples/` and `benches/` dirs.
    pub examples: bool,
}

impl Exclusions {
    pub fn none() -> Self {
        Exclusions::default()
    }

    /// Tests, and examples/benches too unless `keep_examples`.
    pub fn tests(keep_examples: bool) -> Self {
        Exclusions {
            tests: true,
            examples: !keep_examples,
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.tests && !self.examples
    }

    /// Whether `node` is dropped.
    pub fn drops(&self, node: &Node) -> bool {
        node.kind != NodeKind::External
            && ((self.tests && node.flags.is_test) || (self.examples && node.flags.is_example))
    }
}

/// `graph` without the excluded nodes, their edges, and externals only they
/// called. Returns the graph unchanged when nothing is excluded or dropped.
pub fn exclude(graph: CodeGraph, ex: Exclusions) -> CodeGraph {
    if ex.is_empty() || !graph.nodes().any(|n| ex.drops(n)) {
        return graph;
    }
    let snap = graph.snapshot();
    let mut keep: Vec<bool> = snap.nodes.iter().map(|n| !ex.drops(n)).collect();
    let mut called = vec![false; snap.nodes.len()];
    for e in &snap.edges {
        if keep[e.from.0 as usize] {
            called[e.to.0 as usize] = true;
        }
    }
    for (i, n) in snap.nodes.iter().enumerate() {
        if n.kind == NodeKind::External && !called[i] {
            keep[i] = false;
        }
    }
    let mut remap = vec![None; snap.nodes.len()];
    let mut nodes = Vec::with_capacity(snap.nodes.len());
    for (i, n) in snap.nodes.into_iter().enumerate() {
        if keep[i] {
            remap[i] = Some(NodeId(nodes.len() as u32));
            nodes.push(n);
        }
    }
    let edges = snap
        .edges
        .into_iter()
        .filter_map(|mut e| {
            e.from = remap[e.from.0 as usize]?;
            e.to = remap[e.to.0 as usize]?;
            Some(e)
        })
        .collect();
    CodeGraph::from_snapshot(GraphSnapshot {
        files: snap.files,
        nodes,
        edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, NodeFlags, NodeStats, SourceSpan};

    fn add(g: &mut CodeGraph, name: &str, kind: NodeKind, flags: NodeFlags) -> NodeId {
        let f = g.intern_file("src/lib.rs");
        g.add_node(Node {
            id: NodeId(0),
            name: name.into(),
            qualified_name: format!("crate::{name}"),
            module_path: "crate".into(),
            kind,
            span: SourceSpan::new(f, 1, 0, 2, 0),
            flags,
            stats: NodeStats::default(),
        })
    }

    #[test]
    fn drops_tests_their_edges_and_externals_only_they_call() {
        let mut g = CodeGraph::new();
        let site = SourceSpan::new(lcw_core::FileId(0), 1, 0, 1, 1);
        let test = NodeFlags {
            is_test: true,
            ..Default::default()
        };
        let example = NodeFlags {
            is_example: true,
            ..Default::default()
        };
        let prod = add(&mut g, "prod", NodeKind::Function, NodeFlags::default());
        let t = add(&mut g, "check", NodeKind::Function, test);
        let ex = add(&mut g, "demo", NodeKind::Function, example);
        let assert_ext = add(
            &mut g,
            "assert_eq",
            NodeKind::External,
            NodeFlags::default(),
        );
        let fs_ext = add(&mut g, "read", NodeKind::External, NodeFlags::default());
        g.add_edge(t, prod, Edge::new(EdgeKind::DirectCall, site));
        g.add_edge(t, assert_ext, Edge::new(EdgeKind::Unresolved, site));
        g.add_edge(prod, fs_ext, Edge::new(EdgeKind::Unresolved, site));
        g.add_edge(ex, prod, Edge::new(EdgeKind::DirectCall, site));

        let only_tests = exclude(
            g.clone(),
            Exclusions {
                tests: true,
                examples: false,
            },
        );
        let names: Vec<_> = only_tests.nodes().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["prod", "demo", "read"]);
        assert_eq!(only_tests.edge_count(), 2);

        let both = exclude(g.clone(), Exclusions::tests(false));
        let names: Vec<_> = both.nodes().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["prod", "read"]);
        let p = both.node_by_qualified("crate::prod").unwrap();
        assert_eq!(both.in_degree(p), 0);

        assert_eq!(exclude(g.clone(), Exclusions::none()).node_count(), 5);
    }
}
