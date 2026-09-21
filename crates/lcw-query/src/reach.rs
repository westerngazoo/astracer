//! Reachability and bounded call trees: "what does this trigger?" (downstream,
//! following calls) and "who can trigger this?" (upstream, following callers).
//!
//! Both are breadth-first searches over the adjacency lists. A [`call_tree`]
//! is the same walk shaped for display: each node is *expanded once* (the first
//! time it is met, at its shallowest depth) and later encounters are leaves
//! marked `repeat`, so the tree stays `O(n)` even on cyclic graphs — the same
//! trick an IDE "Call Hierarchy" panel uses.

use std::collections::{HashSet, VecDeque};

use lcw_core::{CodeGraph, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

/// Which way to walk the call edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow calls outward: callees, then their callees...
    Callees,
    /// Follow calls inward: callers, then their callers...
    Callers,
}

fn neighbors(graph: &CodeGraph, id: NodeId, dir: Direction) -> Vec<NodeId> {
    match dir {
        Direction::Callees => graph.neighbors_out(id).collect(),
        Direction::Callers => graph.neighbors_in(id).collect(),
    }
}

/// A node reached by [`reachable`], with its hop distance from the roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    pub id: NodeId,
    pub depth: u32,
}

/// Every node reachable from `roots` within `max_depth` hops in direction
/// `dir`, in breadth-first order (roots first, at depth 0). Each node appears
/// once, at its shortest distance.
pub fn reachable(
    graph: &CodeGraph,
    roots: &[NodeId],
    dir: Direction,
    max_depth: u32,
) -> Vec<Reached> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<Reached> = VecDeque::new();
    let mut out = Vec::new();
    for &r in roots {
        if graph.try_node(r).is_some() && seen.insert(r) {
            queue.push_back(Reached { id: r, depth: 0 });
        }
    }
    while let Some(cur) = queue.pop_front() {
        out.push(cur);
        if cur.depth >= max_depth {
            continue;
        }
        for v in neighbors(graph, cur.id, dir) {
            if seen.insert(v) {
                queue.push_back(Reached {
                    id: v,
                    depth: cur.depth + 1,
                });
            }
        }
    }
    out
}

/// How many *internal* (non-external) nodes other than `root` itself are
/// reachable from it. For an entry point this is "how much of the code this
/// path drives"; for a leaf it is 0.
pub fn reach_count(graph: &CodeGraph, root: NodeId, dir: Direction) -> usize {
    reachable(graph, &[root], dir, u32::MAX)
        .iter()
        .filter(|r| r.id != root && graph.node(r.id).kind != NodeKind::External)
        .count()
}

/// Knobs for [`call_tree`]. Defaults give a compact, readable tree.
#[derive(Debug, Clone, Copy)]
pub struct CallTreeOptions {
    pub direction: Direction,
    /// Levels below the root to expand (root is depth 0).
    pub max_depth: u32,
    /// Children shown per node; the rest are counted in `truncated`.
    pub max_children: usize,
    /// Include external / unresolved targets as leaves.
    pub include_external: bool,
}

impl Default for CallTreeOptions {
    fn default() -> Self {
        CallTreeOptions {
            direction: Direction::Callees,
            max_depth: 3,
            max_children: 12,
            include_external: false,
        }
    }
}

/// One node of a bounded call tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTreeNode {
    pub id: NodeId,
    pub depth: u32,
    pub children: Vec<CallTreeNode>,
    /// Children omitted by `max_children`.
    pub truncated: usize,
    /// This node was already expanded elsewhere in the tree (or is an ancestor
    /// on the current path, i.e. a cycle); it is shown as a leaf.
    pub repeat: bool,
}

impl CallTreeNode {
    /// Total nodes in this subtree, itself included.
    pub fn size(&self) -> usize {
        1 + self.children.iter().map(CallTreeNode::size).sum::<usize>()
    }
}

/// Build a bounded call tree rooted at `root`. Callee children are ordered by
/// their first call site (source order — the order the code runs them), caller
/// children by qualified name; internal nodes come before externals.
pub fn call_tree(graph: &CodeGraph, root: NodeId, opts: &CallTreeOptions) -> CallTreeNode {
    let mut expanded: HashSet<NodeId> = HashSet::new();
    expand(graph, root, 0, opts, &mut expanded)
}

fn expand(
    graph: &CodeGraph,
    id: NodeId,
    depth: u32,
    opts: &CallTreeOptions,
    expanded: &mut HashSet<NodeId>,
) -> CallTreeNode {
    let repeat = !expanded.insert(id);
    let mut node = CallTreeNode {
        id,
        depth,
        children: Vec::new(),
        truncated: 0,
        repeat,
    };
    if repeat || depth >= opts.max_depth {
        return node;
    }

    let mut kids: Vec<(NodeId, u32)> = match opts.direction {
        Direction::Callees => graph
            .edges_out(id)
            .map(|(to, e)| (to, e.call_site.start_line))
            .collect(),
        Direction::Callers => graph.edges_in(id).map(|(from, _)| (from, 0)).collect(),
    };
    kids.retain(|(k, _)| opts.include_external || graph.node(*k).kind != NodeKind::External);
    // Merged edges of different kinds list the same neighbor twice; keep one.
    kids.sort_by(|a, b| {
        let (na, nb) = (graph.node(a.0), graph.node(b.0));
        let ext = |k: NodeKind| k == NodeKind::External;
        ext(na.kind)
            .cmp(&ext(nb.kind))
            .then(a.1.cmp(&b.1))
            .then_with(|| na.qualified_name.cmp(&nb.qualified_name))
    });
    kids.dedup_by_key(|k| k.0);

    let total = kids.len();
    for (k, _) in kids.into_iter().take(opts.max_children) {
        node.children
            .push(expand(graph, k, depth + 1, opts, expanded));
    }
    node.truncated = total.saturating_sub(node.children.len());
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, call, id, plain};
    use lcw_core::EdgeKind;

    #[test]
    fn reachable_walks_forward_and_backward_with_depths() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let down = reachable(&g, &[main], Direction::Callees, u32::MAX);
        let names: Vec<(String, u32)> = down
            .iter()
            .map(|r| (g.node(r.id).qualified_name.clone(), r.depth))
            .collect();
        assert_eq!(names[0], ("app::main".into(), 0));
        assert!(names.contains(&("app::core::Lexer::next".into(), 3)));
        assert!(names.contains(&("std::fs::read".into(), 3)));
        // Externals don't count toward "how much code does main drive".
        assert_eq!(reach_count(&g, main, Direction::Callees), 5);

        let up = reachable(&g, &[id(&g, "app::core::parse")], Direction::Callers, 8);
        let callers: Vec<String> = up
            .iter()
            .map(|r| g.node(r.id).qualified_name.clone())
            .collect();
        assert!(callers.contains(&"app::run".to_string()));
        assert!(callers.contains(&"app::main".to_string()));
        assert!(callers.contains(&"app::core::tests::test_parse".to_string()));
        // A depth limit stops the walk.
        assert_eq!(reachable(&g, &[main], Direction::Callees, 1).len(), 3);
    }

    #[test]
    fn call_tree_orders_by_call_site_and_expands_once() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let tree = call_tree(&g, main, &CallTreeOptions::default());
        // main calls run (line 11) before save (line 12).
        let kids: Vec<&str> = tree
            .children
            .iter()
            .map(|c| g.node(c.id).name.as_str())
            .collect();
        assert_eq!(kids, vec!["run", "save"]);
        // run -> load -> (std::fs::read excluded), run -> parse -> next.
        let run = &tree.children[0];
        assert_eq!(run.children.len(), 2);
        assert!(run.children[0].children.is_empty()); // load's only callee is external
        assert_eq!(tree.size(), 6);
        assert!(!tree.children.iter().any(|c| c.repeat));
    }

    #[test]
    fn call_tree_marks_cycles_and_truncates() {
        let mut g = CodeGraph::new();
        let a = plain(&mut g, "m::a", "src/lib.rs", 1);
        let b = plain(&mut g, "m::b", "src/lib.rs", 5);
        call(&mut g, a, b, EdgeKind::DirectCall, 2);
        call(&mut g, b, a, EdgeKind::DirectCall, 6); // a <-> b
        for i in 0..5 {
            let leaf = plain(&mut g, &format!("m::leaf{i}"), "src/lib.rs", 20 + i);
            call(&mut g, b, leaf, EdgeKind::DirectCall, 7 + i);
        }
        let opts = CallTreeOptions {
            max_children: 2,
            ..Default::default()
        };
        let tree = call_tree(&g, a, &opts);
        let b_node = &tree.children[0];
        assert_eq!(b_node.id, b);
        // b's children: `a` (line 6, a repeat/cycle) then leaf0; 4 truncated.
        assert_eq!(b_node.children.len(), 2);
        assert!(b_node.children[0].repeat);
        assert_eq!(b_node.children[0].id, a);
        assert_eq!(b_node.truncated, 4);
    }

    #[test]
    fn callers_tree_walks_upstream() {
        let g = app_graph();
        let parse = id(&g, "app::core::parse");
        let opts = CallTreeOptions {
            direction: Direction::Callers,
            ..Default::default()
        };
        let tree = call_tree(&g, parse, &opts);
        let callers: Vec<&str> = tree
            .children
            .iter()
            .map(|c| g.node(c.id).qualified_name.as_str())
            .collect();
        assert_eq!(callers, vec!["app::core::tests::test_parse", "app::run"]);
        assert_eq!(tree.children[1].children[0].id, id(&g, "app::main"));
    }
}
