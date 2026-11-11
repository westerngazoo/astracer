//! **Impact** (blast radius): "if I change this function, what else can break?"
//!
//! A level-by-level breadth-first walk over the *callers* of a symbol, bounded
//! by a depth. Every caller is reported once, at its shortest distance, with
//! the hop it takes toward the root (`via`) and the call site of that hop, so a
//! reader (or an agent) can jump straight to the line that would be affected.
//! Entry points among the callers tell you which programs / APIs / tests the
//! change can reach.

use std::collections::HashSet;

use lcw_core::{CodeGraph, NodeId, NodeKind, SourceSpan};
use serde::{Deserialize, Serialize};

use crate::entries::{classify_entry, EntryKind};

/// One transitive caller of the impact root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactCaller {
    pub id: NodeId,
    /// Hops from the root (direct callers are depth 1).
    pub depth: u32,
    /// The node this caller calls on its shortest way to the root.
    pub via: NodeId,
    /// Call site of `id -> via`, in the caller's file.
    pub call_site: SourceSpan,
    /// Set when this caller is itself an entry point.
    pub entry: Option<EntryKind>,
}

/// The blast radius of one symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impact {
    pub root: NodeId,
    pub max_depth: u32,
    /// Callers ordered by depth, then qualified name.
    pub callers: Vec<ImpactCaller>,
    /// `by_depth[i]` = callers at depth `i + 1`.
    pub by_depth: Vec<usize>,
    /// Distinct source files touched by the root and its callers.
    pub files: usize,
    /// More callers exist past `max_depth` (the walk was cut short).
    pub beyond_depth: bool,
}

impl Impact {
    /// Callers that are entry points (`main`, exports, public roots, tests).
    pub fn entry_points(&self) -> impl Iterator<Item = &ImpactCaller> {
        self.callers.iter().filter(|c| c.entry.is_some())
    }
}

/// Transitive callers of `root` up to `max_depth` hops, or `None` when `root`
/// is out of range. `O(n + e)` over the visited subgraph.
pub fn impact(graph: &CodeGraph, root: NodeId, max_depth: u32) -> Option<Impact> {
    let root_node = graph.try_node(root)?;
    let mut seen: HashSet<NodeId> = HashSet::from([root]);
    let mut files: HashSet<lcw_core::FileId> = HashSet::new();
    if root_node.kind != NodeKind::External {
        files.insert(root_node.span.file());
    }
    let mut callers: Vec<ImpactCaller> = Vec::new();
    let mut by_depth = Vec::new();
    let mut level: Vec<NodeId> = vec![root];

    for depth in 1..=max_depth {
        let mut next: Vec<ImpactCaller> = Vec::new();
        for &via in &level {
            for (from, e) in graph.edges_in(via) {
                if graph.node(from).kind == NodeKind::External || !seen.insert(from) {
                    continue;
                }
                next.push(ImpactCaller {
                    id: from,
                    depth,
                    via,
                    call_site: e.call_site,
                    entry: None,
                });
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| {
            graph
                .node(a.id)
                .qualified_name
                .cmp(&graph.node(b.id).qualified_name)
        });
        for c in &mut next {
            c.entry = classify_entry(graph, c.id);
            files.insert(graph.node(c.id).span.file());
        }
        by_depth.push(next.len());
        level = next.iter().map(|c| c.id).collect();
        callers.extend(next);
    }

    let beyond_depth = by_depth.len() as u32 == max_depth
        && level.iter().any(|&id| {
            graph
                .neighbors_in(id)
                .any(|f| !seen.contains(&f) && graph.node(f).kind != NodeKind::External)
        });

    Some(Impact {
        root,
        max_depth,
        callers,
        by_depth,
        files: files.len(),
        beyond_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, call, id, plain};
    use lcw_core::EdgeKind;

    fn names(g: &CodeGraph, imp: &Impact) -> Vec<(String, u32)> {
        imp.callers
            .iter()
            .map(|c| (g.node(c.id).qualified_name.clone(), c.depth))
            .collect()
    }

    #[test]
    fn impact_walks_callers_by_depth_with_call_sites() {
        let g = app_graph();
        let next = id(&g, "app::core::Lexer::next");
        let imp = impact(&g, next, 3).unwrap();
        assert_eq!(
            names(&g, &imp),
            vec![
                ("app::core::parse".to_string(), 1),
                ("app::core::tests::test_parse".to_string(), 2),
                ("app::run".to_string(), 2),
                ("app::main".to_string(), 3),
            ]
        );
        assert_eq!(imp.by_depth, vec![1, 2, 1]);
        // run reaches the root through parse, at its first call site (line 22).
        let run = imp
            .callers
            .iter()
            .find(|c| c.id == id(&g, "app::run"))
            .unwrap();
        assert_eq!(run.via, id(&g, "app::core::parse"));
        assert_eq!(run.call_site.start_line, 22);
        // core.rs (next, parse, test) + main.rs (run, main).
        assert_eq!(imp.files, 2);
        assert!(!imp.beyond_depth);

        let entries: Vec<(&str, EntryKind)> = imp
            .entry_points()
            .map(|c| (g.node(c.id).name.as_str(), c.entry.unwrap()))
            .collect();
        assert_eq!(
            entries,
            vec![("test_parse", EntryKind::Test), ("main", EntryKind::Main)]
        );
    }

    #[test]
    fn impact_depth_limit_flags_more_callers() {
        let g = app_graph();
        let next = id(&g, "app::core::Lexer::next");
        let imp = impact(&g, next, 1).unwrap();
        assert_eq!(names(&g, &imp), vec![("app::core::parse".to_string(), 1)]);
        assert!(imp.beyond_depth);

        let main = impact(&g, id(&g, "app::main"), 3).unwrap();
        assert!(main.callers.is_empty());
        assert!(main.by_depth.is_empty());
        assert!(!main.beyond_depth);
        assert!(impact(&g, NodeId(999), 3).is_none());
    }

    #[test]
    fn impact_terminates_on_cycles_and_reports_each_caller_once() {
        let mut g = CodeGraph::new();
        let a = plain(&mut g, "m::a", "src/lib.rs", 1);
        let b = plain(&mut g, "m::b", "src/lib.rs", 5);
        let c = plain(&mut g, "m::c", "src/lib.rs", 9);
        call(&mut g, a, b, EdgeKind::DirectCall, 2);
        call(&mut g, b, a, EdgeKind::DirectCall, 6);
        call(&mut g, c, a, EdgeKind::DirectCall, 10);
        call(&mut g, c, b, EdgeKind::DirectCall, 11);
        let imp = impact(&g, a, 10).unwrap();
        assert_eq!(
            names(&g, &imp),
            vec![("m::b".to_string(), 1), ("m::c".to_string(), 1)]
        );
        assert!(!imp.beyond_depth);
    }
}
