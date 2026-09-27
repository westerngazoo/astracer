//! Roots of execution, and the tree that grows from each: "where does control
//! begin, and what does it do from there".
//!
//! A program has more than one place control begins. `main` is one. On a
//! kernel it is the export the bootloader jumps to, and every trap handler the
//! hardware vectors into. In a threaded program every spawned thread or task
//! is another, running concurrently with the code that started it. Each is the
//! root of its own call tree, and reading unfamiliar code is walking those
//! trees: start at a root, open what it calls, follow the branch that matters.
//!
//! The outline (crate ▸ module ▸ function) answers "how is this organised";
//! these trees answer "what runs", which is a different question with
//! different roots.

use std::collections::HashMap;

use lcw_core::{CodeGraph, EdgeKind, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

use crate::entries::{entry_points, primary_entry, EntryKind};
use crate::reach::{reach_count, Direction};

/// How a thread of control comes to start at a root. Ordered as the explorer
/// lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// A program entry: `main`, `_start`, Go's `init`. The process starts here.
    Program,
    /// Exported across a foreign ABI and called by nothing in the analyzed
    /// code: a bootloader's kernel entry, a trap handler, a host's callback.
    Exported,
    /// Started on another thread or task by a spawn site.
    Spawned,
}

impl RootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RootKind::Program => "program",
            RootKind::Exported => "exported",
            RootKind::Spawned => "spawned",
        }
    }
}

/// A place where a thread of control begins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRoot {
    pub node: NodeId,
    pub kind: RootKind,
    /// For a spawned root: every function that spawns it, in id order. A
    /// worker started from two places is one root with two spawners.
    pub spawned_by: Vec<NodeId>,
    /// Functions reachable from the root, through calls and further spawns.
    pub reach: usize,
    /// Every spawner is a test, so this thread exists only under the test
    /// harness. Listed last: it is not a thread of the program itself.
    pub test_only: bool,
}

/// Every root of execution, in reading order: the primary entry first (see
/// [`primary_entry`]), then other program entries, foreign-ABI exports, and
/// spawned threads and tasks — each group by reach, largest first, with
/// test-only threads after the rest.
pub fn run_roots(graph: &CodeGraph) -> Vec<RunRoot> {
    let mut roots: Vec<RunRoot> = entry_points(graph)
        .into_iter()
        .filter_map(|e| {
            let kind = match e.kind {
                EntryKind::Main => RootKind::Program,
                EntryKind::Exported => RootKind::Exported,
                _ => return None,
            };
            Some(RunRoot {
                node: e.id,
                kind,
                spawned_by: Vec::new(),
                reach: reach_count(graph, e.id, Direction::Callees),
                test_only: false,
            })
        })
        .collect();

    let mut spawned: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for id in graph.node_ids() {
        for (to, edge) in graph.edges_out(id) {
            if edge.kind == EdgeKind::Spawn {
                spawned.entry(to).or_default().push(id);
            }
        }
    }
    for (node, mut by) in spawned {
        by.sort_unstable_by_key(|id| id.0);
        by.dedup();
        let test_only = by.iter().all(|&s| graph.node(s).flags.is_test);
        roots.push(RunRoot {
            node,
            kind: RootKind::Spawned,
            spawned_by: by,
            reach: reach_count(graph, node, Direction::Callees),
            test_only,
        });
    }

    let primary = primary_entry(graph);
    roots.sort_by(|a, b| {
        (Some(a.node) != primary)
            .cmp(&(Some(b.node) != primary))
            .then(a.kind.cmp(&b.kind))
            .then(a.test_only.cmp(&b.test_only))
            .then(b.reach.cmp(&a.reach))
            .then(a.node.0.cmp(&b.node.0))
    });
    roots
}

/// One child in the tree: something the parent calls or starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub node: NodeId,
    /// How the parent reaches it. [`EdgeKind::Spawn`] when the parent starts
    /// it on another thread rather than calling it.
    pub via: EdgeKind,
    /// Line of the parent's first call site for it, for source order.
    pub line: u32,
    /// Call sites in the parent that reach it.
    pub count: u32,
    /// Already on the path from the root: a recursive call. Shown so the
    /// reader sees the loop, never opened, so the tree stays finite.
    pub recursive: bool,
    /// Whether opening it would show anything.
    pub has_branches: bool,
}

/// One level of the tree below `node`, in source order — the order the code
/// does things in. `path` is the chain from the root down to `node`, used to
/// mark recursion. External targets (`std`, other crates, unresolved calls)
/// are left out unless asked for: in a tree meant for following the program's
/// own logic, `unwrap` and `into` are noise.
pub fn branches(
    graph: &CodeGraph,
    node: NodeId,
    path: &[NodeId],
    include_external: bool,
) -> Vec<Branch> {
    let keep = |id: NodeId| include_external || graph.node(id).kind != NodeKind::External;
    let mut by_target: HashMap<NodeId, Branch> = HashMap::new();
    for (to, edge) in graph.edges_out(node) {
        if !keep(to) {
            continue;
        }
        let line = edge.call_site.start_line;
        let entry = by_target.entry(to).or_insert_with(|| Branch {
            node: to,
            via: edge.kind,
            line,
            count: 0,
            recursive: to == node || path.contains(&to),
            has_branches: graph.edges_out(to).any(|(t, _)| keep(t)),
        });
        entry.count += edge.count;
        // Starting a thread is the more important thing to show when the
        // parent both calls and spawns the same function.
        if edge.kind == EdgeKind::Spawn {
            entry.via = EdgeKind::Spawn;
        }
        if line < entry.line {
            entry.line = line;
        }
    }
    let mut out: Vec<Branch> = by_target.into_values().collect();
    out.sort_by_key(|b| (b.line, b.node.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{call, def, id, plain};
    use lcw_core::NodeFlags;

    /// ```text
    /// app::main ──▶ app::run ──▶ app::parse ──▶ app::parse (recursion)
    ///     │            └──────▶ std::fs::read (external)
    ///     ├─spawn─▶ app::main::<spawned@L3> ──▶ app::work
    ///     └─spawn─▶ app::listener
    /// app::kmain (exported)       app::tests::t ─spawn─▶ app::probe
    /// ```
    fn threaded() -> CodeGraph {
        let mut g = CodeGraph::new();
        let main = plain(&mut g, "app::main", "src/main.rs", 1);
        let run = plain(&mut g, "app::run", "src/main.rs", 10);
        let parse = plain(&mut g, "app::parse", "src/main.rs", 20);
        let work = plain(&mut g, "app::work", "src/main.rs", 30);
        let listener = plain(&mut g, "app::listener", "src/main.rs", 40);
        let closure = def(
            &mut g,
            "app::main::<spawned@L3>",
            "src/main.rs",
            3,
            NodeFlags::default(),
        );
        let exported = NodeFlags {
            is_exported: true,
            ..Default::default()
        };
        def(&mut g, "app::kmain", "src/boot.rs", 1, exported);
        let test = def(
            &mut g,
            "app::tests::t",
            "src/main.rs",
            90,
            NodeFlags {
                is_test: true,
                ..Default::default()
            },
        );
        let probe = plain(&mut g, "app::probe", "src/main.rs", 95);
        let read = g.add_node(lcw_core::Node::external("std::fs::read"));

        call(&mut g, main, run, EdgeKind::DirectCall, 2);
        call(&mut g, main, closure, EdgeKind::Spawn, 3);
        call(&mut g, main, listener, EdgeKind::Spawn, 4);
        call(&mut g, closure, work, EdgeKind::DirectCall, 3);
        call(&mut g, run, parse, EdgeKind::DirectCall, 11);
        call(&mut g, run, read, EdgeKind::Unresolved, 12);
        call(&mut g, parse, parse, EdgeKind::DirectCall, 21);
        call(&mut g, test, probe, EdgeKind::Spawn, 91);
        g
    }

    fn names(g: &CodeGraph, roots: &[RunRoot]) -> Vec<String> {
        roots
            .iter()
            .map(|r| g.node(r.node).qualified_name.clone())
            .collect()
    }

    #[test]
    fn roots_run_from_the_entry_through_exports_to_threads() {
        let g = threaded();
        let roots = run_roots(&g);
        assert_eq!(
            names(&g, &roots),
            vec![
                "app::main",
                "app::kmain",
                "app::main::<spawned@L3>",
                "app::listener",
                "app::probe",
            ]
        );
        assert_eq!(roots[0].kind, RootKind::Program);
        assert_eq!(roots[1].kind, RootKind::Exported);
        assert!(roots[2..].iter().all(|r| r.kind == RootKind::Spawned));
        // The closure thread reaches `work`; the listener reaches nothing.
        assert!(roots[2].reach > roots[3].reach);
        assert_eq!(roots[2].spawned_by, vec![id(&g, "app::main")]);
        // A thread only a test starts is the harness's, not the program's.
        assert!(!roots[3].test_only);
        assert!(roots[4].test_only);
    }

    #[test]
    fn spawns_count_toward_reach() {
        let g = threaded();
        let main = run_roots(&g).into_iter().next().unwrap();
        // run, parse, the closure, work and listener — through two spawns.
        assert_eq!(main.reach, 5);
    }

    #[test]
    fn branches_follow_source_order_and_mark_spawns() {
        let g = threaded();
        let main = id(&g, "app::main");
        let kids = branches(&g, main, &[main], false);
        let got: Vec<(String, EdgeKind)> = kids
            .iter()
            .map(|b| (g.node(b.node).name.clone(), b.via))
            .collect();
        assert_eq!(
            got,
            vec![
                ("run".into(), EdgeKind::DirectCall),
                ("<spawned@L3>".into(), EdgeKind::Spawn),
                ("listener".into(), EdgeKind::Spawn),
            ]
        );
        assert!(kids[0].has_branches && kids[1].has_branches);
        assert!(!kids[2].has_branches, "listener calls nothing");
    }

    #[test]
    fn externals_are_left_out_unless_asked_for() {
        let g = threaded();
        let run = id(&g, "app::run");
        assert_eq!(branches(&g, run, &[run], false).len(), 1);
        assert_eq!(branches(&g, run, &[run], true).len(), 2);
    }

    #[test]
    fn recursion_is_marked_so_the_tree_stays_finite() {
        let g = threaded();
        let (main, run, parse) = (
            id(&g, "app::main"),
            id(&g, "app::run"),
            id(&g, "app::parse"),
        );
        let kids = branches(&g, parse, &[main, run, parse], false);
        assert_eq!(kids.len(), 1);
        assert!(kids[0].recursive, "parse calls itself");
    }
}
