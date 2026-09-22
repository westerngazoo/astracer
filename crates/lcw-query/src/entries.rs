//! Entry points: where a reader should *start* walking a codebase.
//!
//! The call graph has no arrows pointing at a process's first function, so
//! "where does it begin" has to be inferred. Three signals combine:
//!
//! * the conventional **name** of a program entry — `main`, `_start` (what an
//!   ELF loader and a WASI host hand control to), Go's `init`;
//! * **linkage**: a symbol exported across a foreign ABI exists so that
//!   something outside this source can call it, and that caller can never
//!   appear in the graph;
//! * graph **shape**: a definition nobody in the analyzed code calls is a
//!   *root*, which is either a true entry, a public API surface consumed from
//!   outside, or dead code.
//!
//! The linkage signal is what makes the answer honest on bare-metal and WASM
//! codebases, where the kernel entry, the trap vector and every module export
//! are uncalled by construction and would otherwise look like dead code.

use lcw_core::{CodeGraph, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

/// Why a node counts as an entry point. Ordered from "most likely where the
/// program starts" to "least": sorting by kind puts `main` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// The process entry: `main` in Rust/Go/Python/TypeScript, `_start` (the
    /// ELF / WASI entry symbol), `init` in Go.
    Main,
    /// Exported across a foreign ABI and called by nobody in the analyzed code:
    /// a kernel entry the bootloader jumps to, a trap/interrupt handler the
    /// hardware vectors into, a WASM export the host calls, an FFI symbol.
    /// The caller is real but lives outside this source, so the graph cannot
    /// show it. On bare-metal and WASM codebases these, not `main`, are where
    /// execution actually begins.
    Exported,
    /// A public item with no in-graph callers: an API surface used from outside
    /// the analyzed code (a library's exports, a handler registered by a
    /// framework, a Tauri command).
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
            EntryKind::Exported => "exported",
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
    } else if n.flags.is_exported {
        Some(EntryKind::Exported)
    } else if n.flags.is_pub {
        Some(EntryKind::PublicRoot)
    } else {
        Some(EntryKind::Root)
    }
}

/// Every entry point in the graph, `main` first, then foreign-ABI exports,
/// public roots, private roots and tests; alphabetical by qualified name
/// within a kind.
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

/// The single best place to start reading: of the real entry points — program
/// entries and foreign-ABI exports — the one that drives the most code.
///
/// Reach is what separates the entry that matters from the ones that merely
/// exist. A repository typically has several `main`s (a build script, a
/// benchmark, a two-line bootstrap) and picking the alphabetically first, or
/// even the first `main`, routinely lands on a dev tool. On a microkernel the
/// answer is not a `main` at all: `kmain`, reached only by the bootloader,
/// drives an order of magnitude more code than any host-side binary in the
/// same workspace.
///
/// Ties break toward the lower node id, so the result is deterministic.
pub fn primary_entry(graph: &CodeGraph) -> Option<NodeId> {
    entry_points(graph)
        .into_iter()
        .filter(|e| matches!(e.kind, EntryKind::Main | EntryKind::Exported))
        .map(|e| {
            (
                crate::reach::reach_count(graph, e.id, crate::reach::Direction::Callees),
                std::cmp::Reverse(e.id),
            )
        })
        .max()
        .map(|(_, std::cmp::Reverse(id))| id)
}

/// Just the process entries (`main`s), the usual place to start a walk.
pub fn mains(graph: &CodeGraph) -> Vec<NodeId> {
    entry_points(graph)
        .into_iter()
        .filter(|e| e.kind == EntryKind::Main)
        .map(|e| e.id)
        .collect()
}

/// `main` everywhere; `_start`, the symbol an ELF loader and a WASI host both
/// hand control to, which is the entry in a `no_std` binary or a WASM command
/// module that has no `main` at all; `init` too when the analyzed sources are
/// Go (its runtime calls every package `init` before `main`).
fn is_main_name(graph: &CodeGraph, name: &str) -> bool {
    name == "main" || name == "_start" || (name == "init" && is_go(graph))
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
    fn foreign_abi_exports_are_entry_points_not_dead_code() {
        // A bare-metal kernel: `_start` (the ELF entry), `kmain` and a trap
        // handler the hardware vectors into, none of them called in-graph.
        let mut g = CodeGraph::new();
        let exported = lcw_core::NodeFlags {
            is_pub: true,
            is_exported: true,
            ..Default::default()
        };
        let start =
            crate::fixtures::def(&mut g, "kernel::_start", "kernel/src/main.rs", 1, exported);
        let kmain =
            crate::fixtures::def(&mut g, "kernel::kmain", "kernel/src/main.rs", 20, exported);
        let trap = crate::fixtures::def(
            &mut g,
            "kernel::trap_handler",
            "kernel/src/trap.rs",
            9,
            exported,
        );
        // A plain public helper is only an API surface, not an entry.
        let helper = crate::fixtures::def(
            &mut g,
            "kernel::helper",
            "kernel/src/main.rs",
            60,
            lcw_core::NodeFlags {
                is_pub: true,
                ..Default::default()
            },
        );

        assert_eq!(classify_entry(&g, start), Some(EntryKind::Main));
        assert_eq!(classify_entry(&g, kmain), Some(EntryKind::Exported));
        assert_eq!(classify_entry(&g, trap), Some(EntryKind::Exported));
        assert_eq!(classify_entry(&g, helper), Some(EntryKind::PublicRoot));

        // `_start` leads, then the exports, then the plain public root.
        let order: Vec<&str> = entry_points(&g)
            .iter()
            .map(|e| graph_name(&g, e.id))
            .collect();
        assert_eq!(order, vec!["_start", "kmain", "trap_handler", "helper"]);
    }

    #[test]
    fn an_export_with_callers_is_not_an_entry() {
        // Exported *and* called from inside: the in-graph caller already
        // explains it, so it does not belong in "where do I start".
        let mut g = CodeGraph::new();
        let caller = crate::fixtures::plain(&mut g, "krate::caller", "src/lib.rs", 1);
        let callback = crate::fixtures::def(
            &mut g,
            "krate::callback",
            "src/lib.rs",
            20,
            lcw_core::NodeFlags {
                is_exported: true,
                ..Default::default()
            },
        );
        crate::fixtures::call(&mut g, caller, callback, lcw_core::EdgeKind::DirectCall, 2);
        assert_eq!(classify_entry(&g, callback), None);
    }

    fn graph_name(g: &CodeGraph, id: NodeId) -> &str {
        g.node(id).name.as_str()
    }

    #[test]
    fn primary_entry_is_the_one_that_drives_the_most_code() {
        // A workspace shaped like a real one: a small host tool with a `main`,
        // and a kernel whose only way in is an export the bootloader jumps to.
        let mut g = CodeGraph::new();
        let tool_main = plain(&mut g, "tool::main", "tools/src/main.rs", 1);
        let helper = plain(&mut g, "tool::helper", "tools/src/main.rs", 20);
        crate::fixtures::call(&mut g, tool_main, helper, lcw_core::EdgeKind::DirectCall, 2);

        let kmain = crate::fixtures::def(
            &mut g,
            "kernel::kmain",
            "kernel/src/main.rs",
            1,
            lcw_core::NodeFlags {
                is_pub: true,
                is_exported: true,
                ..Default::default()
            },
        );
        for i in 0..5 {
            let sub = plain(
                &mut g,
                &format!("kernel::step{i}"),
                "kernel/src/boot.rs",
                10 + i,
            );
            crate::fixtures::call(&mut g, kmain, sub, lcw_core::EdgeKind::DirectCall, 2 + i);
        }

        // `tool::main` sorts first by kind, but drives 1 function against 5.
        assert_eq!(entry_points(&g)[0].id, tool_main);
        assert_eq!(primary_entry(&g), Some(kmain));
        assert_eq!(primary_entry(&CodeGraph::new()), None);
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
