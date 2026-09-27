//! The **outline**: the call graph re-shaped as the hierarchy a reader already
//! has in their head — crate ▸ module ▸ type ▸ function — so they can browse
//! by *where code lives* rather than by *who calls whom*.
//!
//! Every node's `module_path` (`lcw_core::graph::CodeGraph`) is a path of
//! scopes; splitting it on `::` and inserting the function as a leaf builds a
//! trie. Scopes sort alphabetically; functions sort in source order (file,
//! then line), which is how you'd read them in the editor. External /
//! unresolved targets are left out: the outline is about the code you own.

use std::collections::BTreeMap;

use lcw_core::{CodeGraph, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

use crate::entries::{classify_entry, EntryKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineKind {
    /// A crate, module or type segment of a path.
    Scope,
    /// A function / method (leaf).
    Function,
}

/// One row of the outline tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNode {
    /// Pre-order index: a stable key for UI state such as "expanded".
    pub id: usize,
    /// Last path segment (scope name or function name).
    pub label: String,
    /// Full path: the scope's module path, or the function's qualified name.
    pub path: String,
    pub kind: OutlineKind,
    /// The graph node, for functions.
    pub node: Option<NodeId>,
    pub depth: u32,
    /// Functions in this subtree (1 for a function).
    pub functions: u32,
    /// Entry points in this subtree.
    pub entries: u32,
    /// The function's own entry classification, if any.
    pub entry: Option<EntryKind>,
    /// Cyclomatic complexity of the function (0 for scopes).
    pub cyclomatic: u32,
    pub children: Vec<OutlineNode>,
}

impl OutlineNode {
    pub fn is_scope(&self) -> bool {
        self.kind == OutlineKind::Scope
    }
}

/// The whole tree plus a couple of totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    pub roots: Vec<OutlineNode>,
    pub functions: usize,
    pub scopes: usize,
}

impl Outline {
    /// Pre-order traversal (parents before children), which is also `id` order.
    pub fn flatten(&self) -> Vec<&OutlineNode> {
        fn walk<'a>(n: &'a OutlineNode, out: &mut Vec<&'a OutlineNode>) {
            out.push(n);
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut out = Vec::with_capacity(self.functions + self.scopes);
        for r in &self.roots {
            walk(r, &mut out);
        }
        out
    }

    /// The row for a graph node, if it is in the outline.
    pub fn find(&self, node: NodeId) -> Option<&OutlineNode> {
        self.flatten().into_iter().find(|n| n.node == Some(node))
    }

    /// Keep only the functions whose `path` contains `query`
    /// (case-insensitive), plus their ancestor scopes; an empty query keeps
    /// everything. Totals are recomputed.
    pub fn prune(&self, query: &str) -> Outline {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.clone();
        }
        fn keep(n: &OutlineNode, q: &str) -> Option<OutlineNode> {
            match n.kind {
                OutlineKind::Function => n.path.to_lowercase().contains(q).then(|| n.clone()),
                OutlineKind::Scope => {
                    let children: Vec<OutlineNode> =
                        n.children.iter().filter_map(|c| keep(c, q)).collect();
                    if children.is_empty() {
                        return None;
                    }
                    let mut s = n.clone();
                    s.functions = children.iter().map(|c| c.functions).sum();
                    s.entries = children.iter().map(|c| c.entries).sum();
                    s.children = children;
                    Some(s)
                }
            }
        }
        let roots: Vec<OutlineNode> = self.roots.iter().filter_map(|r| keep(r, &q)).collect();
        let mut out = Outline {
            roots,
            functions: 0,
            scopes: 0,
        };
        let (f, s) = count(&out.roots);
        out.functions = f;
        out.scopes = s;
        out
    }
}

fn count(roots: &[OutlineNode]) -> (usize, usize) {
    let mut f = 0;
    let mut s = 0;
    for n in roots.iter().flat_map(|r| {
        let mut v = Vec::new();
        fn walk<'a>(n: &'a OutlineNode, v: &mut Vec<&'a OutlineNode>) {
            v.push(n);
            for c in &n.children {
                walk(c, v);
            }
        }
        walk(r, &mut v);
        v
    }) {
        match n.kind {
            OutlineKind::Function => f += 1,
            OutlineKind::Scope => s += 1,
        }
    }
    (f, s)
}

/// Intermediate trie node while building.
#[derive(Default)]
struct Trie {
    scopes: BTreeMap<String, Trie>,
    /// `(file, line, name, node)` — sorted into source order at emit time.
    functions: Vec<(u32, u32, String, NodeId)>,
}

/// Build the outline for `graph`.
pub fn outline(graph: &CodeGraph) -> Outline {
    let mut root = Trie::default();
    for n in graph.nodes() {
        if n.kind == NodeKind::External {
            continue;
        }
        let mut cur = &mut root;
        for seg in n.module_path.split("::").filter(|s| !s.is_empty()) {
            cur = cur.scopes.entry(seg.to_string()).or_default();
        }
        cur.functions
            .push((n.span.file, n.span.start_line, n.name.clone(), n.id));
    }

    let mut next_id = 0usize;
    let mut roots = Vec::new();
    for (label, trie) in root.scopes {
        roots.push(emit(graph, &label, &label, trie, 0, &mut next_id));
    }
    // Functions with an empty module path (rare: top-level in a file the
    // adapter could not place) hang directly off the root.
    let mut top = root.functions;
    top.sort();
    for (_, _, _, id) in top {
        roots.push(leaf(graph, id, 0, &mut next_id));
    }

    let (functions, scopes) = count(&roots);
    Outline {
        roots,
        functions,
        scopes,
    }
}

fn emit(
    graph: &CodeGraph,
    label: &str,
    path: &str,
    trie: Trie,
    depth: u32,
    next_id: &mut usize,
) -> OutlineNode {
    let id = *next_id;
    *next_id += 1;
    let mut children = Vec::new();
    for (l, t) in trie.scopes {
        let p = format!("{path}::{l}");
        children.push(emit(graph, &l, &p, t, depth + 1, next_id));
    }
    let mut fns = trie.functions;
    fns.sort();
    for (_, _, _, nid) in fns {
        children.push(leaf(graph, nid, depth + 1, next_id));
    }
    let functions = children.iter().map(|c| c.functions).sum();
    let entries = children.iter().map(|c| c.entries).sum();
    OutlineNode {
        id,
        label: label.to_string(),
        path: path.to_string(),
        kind: OutlineKind::Scope,
        node: None,
        depth,
        functions,
        entries,
        entry: None,
        cyclomatic: 0,
        children,
    }
}

fn leaf(graph: &CodeGraph, nid: NodeId, depth: u32, next_id: &mut usize) -> OutlineNode {
    let id = *next_id;
    *next_id += 1;
    let n = graph.node(nid);
    let entry = classify_entry(graph, nid);
    OutlineNode {
        id,
        label: n.name.clone(),
        path: n.qualified_name.clone(),
        kind: OutlineKind::Function,
        node: Some(nid),
        depth,
        functions: 1,
        entries: entry.is_some() as u32,
        entry,
        cyclomatic: n.cyclomatic_complexity(),
        children: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id};

    fn labels(nodes: &[OutlineNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.label.as_str()).collect()
    }

    #[test]
    fn builds_crate_module_type_function_hierarchy() {
        let g = app_graph();
        let o = outline(&g);
        assert_eq!(o.roots.len(), 1);
        let app = &o.roots[0];
        assert_eq!(app.label, "app");
        assert_eq!(app.functions, 8);
        assert_eq!(app.entries, 3);
        // Scopes first (alphabetical), then functions in source order.
        assert_eq!(
            labels(&app.children),
            vec!["core", "io", "main", "run", "orphan"]
        );
        let core = &app.children[0];
        assert_eq!(labels(&core.children), vec!["Lexer", "tests", "parse"]);
        assert_eq!(core.children[0].path, "app::core::Lexer");
        assert_eq!(core.children[0].children[0].path, "app::core::Lexer::next");
        assert_eq!(o.functions, 8);
        assert_eq!(o.scopes, 5); // app, core, io, Lexer, tests
                                 // Externals are excluded.
        assert!(o.flatten().iter().all(|n| n.path != "std::fs::read"));
    }

    #[test]
    fn ids_are_pre_order_and_lookup_works() {
        let g = app_graph();
        let o = outline(&g);
        let flat = o.flatten();
        for (i, n) in flat.iter().enumerate() {
            assert_eq!(n.id, i);
        }
        let main = o.find(id(&g, "app::main")).unwrap();
        assert_eq!(main.entry, Some(EntryKind::Main));
        assert_eq!(main.depth, 1);
        assert_eq!(main.cyclomatic, 3);
    }

    #[test]
    fn prune_keeps_matches_and_their_ancestors() {
        let g = app_graph();
        let o = outline(&g);
        let p = o.prune("PARSE");
        assert_eq!(p.functions, 2); // parse + test_parse
        let app = &p.roots[0];
        assert_eq!(labels(&app.children), vec!["core"]);
        assert_eq!(app.functions, 2);
        assert_eq!(labels(&app.children[0].children), vec!["tests", "parse"]);
        assert_eq!(o.prune("  ").functions, 8);
        assert!(o.prune("zzz").roots.is_empty());
    }
}
