//! Entry points: where a reader should *start* walking a codebase.
//!
//! The call graph has no arrows pointing at a process's first function, so
//! "where does it begin" has to be inferred. We combine two signals: the
//! conventional name of a program entry (`main`, plus Go's `init`) and graph
//! shape (a definition nobody in the analyzed code calls is a *root*: either a
//! true entry, a public API surface consumed from outside, or dead code).

use lcw_core::{CodeGraph, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

/// Why a node counts as an entry point. Ordered from "most likely where the
/// program starts" to "least": sorting by kind puts `main` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// The process entry: `main` in Rust/Go/Python/TypeScript, `init` in Go.
    Main,
    /// A public item with no in-graph callers: an API surface used from outside
    /// the analyzed code (a library's exports, a handler registered by a
    /// framework, a Tauri command, an FFI export).
    PublicRoot,
    /// A private definition with no callers: reached dynamically (trait
    /// objects, function pointers, closures) or genuinely dead.
    Root,
    /// A test with no callers: a root for the test harness.
    Test,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Main => "main",
            EntryKind::PublicRoot => "public",
            EntryKind::Root => "root",
            EntryKind::Test => "test",
        }
    }
}

impl std::fmt::Display for EntryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A node classified as an entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPoint {
    pub id: NodeId,
    pub kind: EntryKind,
}

/// Classify one node, or `None` when it is not an entry point (external, or
/// called from within the analyzed code).
pub fn classify_entry(graph: &CodeGraph, id: NodeId) -> Option<EntryKind> {
    let n = graph.try_node(id)?;
    if n.kind == NodeKind::External {
        return None;
    }
    if is_main_name(graph, &n.name) {
        return Some(EntryKind::Main);
    }
    if graph.in_degree(id) > 0 {
        return None;
    }
    if n.flags.is_test {
        Some(EntryKind::Test)
    } else if n.flags.is_pub {
        Some(EntryKind::PublicRoot)
    } else {
        Some(EntryKind::Root)
    }
}

/// Every entry point in the graph, `main` first, then public roots, private
/// roots and tests; alphabetical by qualified name within a kind.
pub fn entry_points(graph: &CodeGraph) -> Vec<EntryPoint> {
    let mut out: Vec<EntryPoint> = graph
        .node_ids()
        .filter_map(|id| classify_entry(graph, id).map(|kind| EntryPoint { id, kind }))
        .collect();
    out.sort_by(|a, b| {
        a.kind.cmp(&b.kind).then_with(|| {
            graph
                .node(a.id)
                .qualified_name
                .cmp(&graph.node(b.id).qualified_name)
        })
    });
    out
}

/// Just the process entries (`main`s), the usual place to start a walk.
pub fn mains(graph: &CodeGraph) -> Vec<NodeId> {
    entry_points(graph)
        .into_iter()
        .filter(|e| e.kind == EntryKind::Main)
        .map(|e| e.id)
        .collect()
}

/// `main` everywhere; `init` too when the analyzed sources are Go (its runtime
/// calls every package `init` before `main`).
fn is_main_name(graph: &CodeGraph, name: &str) -> bool {
    name == "main" || (name == "init" && is_go(graph))
}

fn is_go(graph: &CodeGraph) -> bool {
    graph
        .files()
        .next()
        .and_then(|(_, p)| p.extension())
        .is_some_and(|e| e == "go")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id, plain};

    #[test]
    fn classifies_main_roots_public_api_and_tests() {
        let g = app_graph();
        let entries = entry_points(&g);
        let kinds: Vec<(String, EntryKind)> = entries
            .iter()
            .map(|e| (g.node(e.id).qualified_name.clone(), e.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("app::main".into(), EntryKind::Main),
                ("app::orphan".into(), EntryKind::Root),
                ("app::core::tests::test_parse".into(), EntryKind::Test),
            ]
        );
        // Called items are not entries, even when public.
        assert_eq!(classify_entry(&g, id(&g, "app::run")), None);
        assert_eq!(mains(&g), vec![id(&g, "app::main")]);
    }

    #[test]
    fn public_uncalled_items_are_public_roots() {
        let mut g = CodeGraph::new();
        let api = crate::fixtures::def(
            &mut g,
            "lib::api::handle",
            "src/api.rs",
            1,
            lcw_core::NodeFlags {
                is_pub: true,
                ..Default::default()
            },
        );
        assert_eq!(classify_entry(&g, api), Some(EntryKind::PublicRoot));
    }

    #[test]
    fn go_init_counts_as_main() {
        let mut g = CodeGraph::new();
        let init = plain(&mut g, "pkg::init", "cmd/x.go", 1);
        assert_eq!(classify_entry(&g, init), Some(EntryKind::Main));
        let mut r = CodeGraph::new();
        let init_rs = plain(&mut r, "krate::init", "src/lib.rs", 1);
        assert_eq!(classify_entry(&r, init_rs), Some(EntryKind::Root));
    }
}
