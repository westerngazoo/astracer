//! **Groups**: the parts a repository is made of — one per crate — and the
//! order they sit in, callers above callees.
//!
//! This is what the crate-boxed layout draws: each group becomes a box around
//! its own functions, and the boxes stack in rows by [`Group::layer`], so a
//! call between parts reads top to bottom, the way the dependency diagram in
//! a README does.
//!
//! A group is the first segment of a node's module path — the same top level
//! the [`outline`](crate::outline) shows. A repository with a single crate is
//! split one level deeper, by its top-level modules, because one box around
//! everything says nothing. Everything outside the repository shares one
//! [`Group::external`] group, placed last.

use std::collections::HashMap;

use lcw_core::{CodeGraph, EdgeKind, NodeKind};

/// Name of the group holding external / unresolved targets.
pub const EXTERNAL_GROUP: &str = "external";

/// Name of the group for owned code with an empty module path (rare: an
/// adapter could not place the definition).
const TOP_LEVEL_GROUP: &str = "(top level)";

/// One part of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The crate, or `crate::module` for a single-crate repository.
    pub name: String,
    /// Row in the call order: 0 for parts nothing else in the repository calls
    /// (binaries, apps, tests), and every other part one row below the lowest
    /// part that calls it. Only calls that name their target count, and when
    /// two parts call each other the heavier direction wins; a remaining cycle
    /// shares a row. The external group sits below all of them.
    pub layer: u32,
    /// Number of graph nodes in the group.
    pub members: u32,
    /// Whether this is the group of code outside the repository.
    pub external: bool,
}

/// Every node assigned to a group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Groups {
    /// Sorted by layer, then size (largest first), then name; the external
    /// group, if any, is last.
    pub groups: Vec<Group>,
    /// The group of each node, indexed by `NodeId.0`.
    pub of_node: Vec<u32>,
}

impl Groups {
    /// Indices of the nodes in group `g`, in node order.
    pub fn members(&self, g: u32) -> impl Iterator<Item = usize> + '_ {
        self.of_node
            .iter()
            .enumerate()
            .filter(move |(_, &of)| of == g)
            .map(|(i, _)| i)
    }
}

/// Split `graph` into its parts and order them by who calls whom.
pub fn crate_groups(graph: &CodeGraph) -> Groups {
    let split_modules = single_crate(graph);

    // Assign raw group ids in first-seen order.
    let mut index: HashMap<String, u32> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut of_node = Vec::with_capacity(graph.node_count());
    for node in graph.nodes() {
        let key = group_key(node.kind, &node.module_path, split_modules);
        let id = *index.entry(key.clone()).or_insert_with(|| {
            names.push(key);
            (names.len() - 1) as u32
        });
        of_node.push(id);
    }

    let external = index.get(EXTERNAL_GROUP).copied();
    let layers = layers(graph, &of_node, names.len(), external);

    let mut members = vec![0u32; names.len()];
    for &g in &of_node {
        members[g as usize] += 1;
    }

    // Sort, then renumber so the group index is its position in `groups`.
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (a as usize, b as usize);
        layers[a]
            .cmp(&layers[b])
            .then(members[b].cmp(&members[a]))
            .then(names[a].cmp(&names[b]))
    });
    let mut renumber = vec![0u32; names.len()];
    for (new, &old) in order.iter().enumerate() {
        renumber[old as usize] = new as u32;
    }

    let groups = order
        .iter()
        .map(|&old| {
            let old = old as usize;
            Group {
                name: names[old].clone(),
                layer: layers[old],
                members: members[old],
                external: Some(old as u32) == external,
            }
        })
        .collect();
    let of_node = of_node.iter().map(|&g| renumber[g as usize]).collect();
    Groups { groups, of_node }
}

/// Whether every owned node lives in the same crate (so groups should go one
/// module level deeper).
fn single_crate(graph: &CodeGraph) -> bool {
    let mut first: Option<&str> = None;
    for node in graph.nodes() {
        if node.kind == NodeKind::External {
            continue;
        }
        let Some(krate) = segments(&node.module_path).next() else {
            continue;
        };
        match first {
            None => first = Some(krate),
            Some(f) if f != krate => return false,
            Some(_) => {}
        }
    }
    true
}

fn group_key(kind: NodeKind, module_path: &str, split_modules: bool) -> String {
    if kind == NodeKind::External {
        return EXTERNAL_GROUP.to_string();
    }
    let mut segs = segments(home_module(kind, module_path));
    let Some(krate) = segs.next() else {
        return TOP_LEVEL_GROUP.to_string();
    };
    match (split_modules, segs.next()) {
        (true, Some(module)) => format!("{krate}::{module}"),
        _ => krate.to_string(),
    }
}

/// The module a node lives in. A closure's module path is the function it sits
/// in (`app::main` for a thread spawned in `main`), so drop that function, and
/// any closures around it, to get back to the module; otherwise a crate-root
/// function that spawns a thread would look like a module of its own.
fn home_module(kind: NodeKind, module_path: &str) -> &str {
    if kind != NodeKind::Closure {
        return module_path;
    }
    let mut path = module_path;
    while let Some((rest, last)) = path.rsplit_once("::") {
        path = rest;
        if !last.starts_with('<') {
            break;
        }
    }
    path
}

fn segments(module_path: &str) -> impl Iterator<Item = &str> {
    module_path.split("::").filter(|s| !s.is_empty())
}

/// Whether an edge is evidence of one part depending on another. Only calls
/// that name their target count (`f()`, `module::f()`, `Type::f()`, a spawn):
/// a method call or a macro is matched by name alone, and across parts a
/// common name (`get`, `insert`, `matches!`) would tie together parts that
/// never call each other, and one such edge can push a part down a row.
fn orders_parts(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::DirectCall | EdgeKind::AssociatedCall | EdgeKind::Spawn
    )
}

/// Longest-path layering of the group call graph. The external group is left
/// out of the ordering and placed below everything.
///
/// When two parts call each other, only the direction with more calls orders
/// them: crates (Rust) and packages (Go) cannot depend on each other both
/// ways, so the lighter direction is a resolution artifact, and for languages
/// that allow it the heavier direction is still the better reading order.
/// Parts left calling each other in a cycle (equal weights, or longer loops)
/// are condensed and share a layer.
fn layers(graph: &CodeGraph, of_node: &[u32], n: usize, external: Option<u32>) -> Vec<u32> {
    let mut weight: HashMap<(u32, u32), u32> = HashMap::new();
    for (from, to, edge) in graph.edges() {
        let (Some(&a), Some(&b)) = (of_node.get(from.0 as usize), of_node.get(to.0 as usize))
        else {
            continue;
        };
        if a != b && Some(a) != external && Some(b) != external && orders_parts(edge.kind) {
            *weight.entry((a, b)).or_insert(0) += edge.count.max(1);
        }
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (&(a, b), &w) in &weight {
        if weight.get(&(b, a)).is_some_and(|&back| back > w) {
            continue;
        }
        adj[a as usize].push(b);
    }
    for list in &mut adj {
        list.sort_unstable();
    }

    let (comp, count) = tarjan(&adj);
    // Tarjan finishes a component only after everything it reaches, so
    // components come out sinks first: walking the ids downward is a
    // topological order, and pushing layers forward along it is exact.
    let mut comp_layer = vec![0u32; count];
    let mut by_comp: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (v, &c) in comp.iter().enumerate() {
        by_comp[c].push(v);
    }
    for c in (0..count).rev() {
        let here = comp_layer[c];
        for &v in &by_comp[c] {
            for &w in &adj[v] {
                let d = comp[w as usize];
                if d != c {
                    comp_layer[d] = comp_layer[d].max(here + 1);
                }
            }
        }
    }

    let mut out: Vec<u32> = comp.iter().map(|&c| comp_layer[c]).collect();
    if let Some(e) = external {
        let deepest = out
            .iter()
            .enumerate()
            .filter(|&(g, _)| g as u32 != e)
            .map(|(_, &l)| l)
            .max();
        out[e as usize] = deepest.map_or(0, |l| l + 1);
    }
    out
}

/// Strongly connected components (Tarjan), iterative so a long chain of parts
/// cannot overflow the stack. Returns the component of each vertex and the
/// component count; ids are assigned sinks first.
fn tarjan(adj: &[Vec<u32>]) -> (Vec<usize>, usize) {
    const UNSEEN: usize = usize::MAX;
    let n = adj.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp = vec![UNSEEN; n];
    let mut next_index = 0usize;
    let mut count = 0usize;

    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        // Each frame is (vertex, next edge to look at).
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;

        while let Some(frame) = frames.last_mut() {
            let (v, i) = *frame;
            if let Some(&w) = adj[v].get(i) {
                frame.1 += 1;
                let w = w as usize;
                if index[w] == UNSEEN {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    frames.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp[w] = count;
                    if w == v {
                        break;
                    }
                }
                count += 1;
            }
        }
    }
    (comp, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, call, plain};
    use lcw_core::{EdgeKind, Node};

    fn names(groups: &Groups) -> Vec<(&str, u32, u32)> {
        groups
            .groups
            .iter()
            .map(|g| (g.name.as_str(), g.layer, g.members))
            .collect()
    }

    /// `bin` calls `engine` and `core`, `engine` calls `core`: the longest path
    /// puts `core` two rows down even though `bin` calls it directly.
    fn workspace() -> CodeGraph {
        let mut g = CodeGraph::new();
        let main = plain(&mut g, "bin::main", "bins/main.rs", 1);
        let run = plain(&mut g, "engine::run", "engine/lib.rs", 1);
        let step = plain(&mut g, "engine::step", "engine/lib.rs", 9);
        let node = plain(&mut g, "core::graph::node", "core/graph.rs", 1);
        let tool = plain(&mut g, "tools::gen", "tools/gen.rs", 1);
        let print = g.add_node(Node::external("std::io::print"));
        call(&mut g, main, run, EdgeKind::DirectCall, 2);
        call(&mut g, main, node, EdgeKind::DirectCall, 3);
        call(&mut g, run, step, EdgeKind::DirectCall, 2);
        call(&mut g, step, node, EdgeKind::DirectCall, 10);
        call(&mut g, tool, print, EdgeKind::Unresolved, 2);
        g
    }

    #[test]
    fn crates_stack_callers_above_callees() {
        let groups = crate_groups(&workspace());
        assert_eq!(
            names(&groups),
            vec![
                ("bin", 0, 1),
                ("tools", 0, 1),
                ("engine", 1, 2),
                ("core", 2, 1),
                (EXTERNAL_GROUP, 3, 1),
            ]
        );
        assert!(groups.groups[4].external);
        assert!(groups.groups[..4].iter().all(|g| !g.external));
    }

    #[test]
    fn every_node_maps_to_its_crate() {
        let g = workspace();
        let groups = crate_groups(&g);
        assert_eq!(groups.of_node.len(), g.node_count());
        for node in g.nodes() {
            let group = &groups.groups[groups.of_node[node.id.0 as usize] as usize];
            let want = if node.kind == NodeKind::External {
                EXTERNAL_GROUP
            } else {
                node.module_path.split("::").next().unwrap()
            };
            assert_eq!(group.name, want, "{}", node.qualified_name);
        }
        let engine: Vec<usize> = groups.members(2).collect();
        assert_eq!(engine, vec![1, 2]);
    }

    #[test]
    fn crates_that_call_each_other_share_a_row() {
        let mut g = CodeGraph::new();
        let main = plain(&mut g, "bin::main", "bins/main.rs", 1);
        let a = plain(&mut g, "alpha::f", "alpha/lib.rs", 1);
        let b = plain(&mut g, "beta::g", "beta/lib.rs", 1);
        let c = plain(&mut g, "gamma::h", "gamma/lib.rs", 1);
        call(&mut g, main, a, EdgeKind::DirectCall, 2);
        call(&mut g, a, b, EdgeKind::DirectCall, 2);
        call(&mut g, b, a, EdgeKind::DirectCall, 2);
        call(&mut g, b, c, EdgeKind::DirectCall, 3);
        let groups = crate_groups(&g);
        assert_eq!(
            names(&groups),
            vec![
                ("bin", 0, 1),
                ("alpha", 1, 1),
                ("beta", 1, 1),
                ("gamma", 2, 1)
            ]
        );
    }

    #[test]
    fn method_and_macro_calls_do_not_order_crates() {
        // `core` "calls" `engine` only through a method and a macro matched by
        // name; `engine` really calls `core`. The ordering must not see a cycle.
        let mut g = CodeGraph::new();
        let run = plain(&mut g, "engine::run", "engine/lib.rs", 1);
        let get = plain(&mut g, "engine::Cache::get", "engine/cache.rs", 1);
        let new = plain(&mut g, "core::Graph::new", "core/graph.rs", 1);
        let nodes = plain(&mut g, "core::Graph::nodes", "core/graph.rs", 9);
        call(&mut g, run, new, EdgeKind::AssociatedCall, 2);
        call(&mut g, nodes, get, EdgeKind::MethodCall, 10);
        call(&mut g, nodes, run, EdgeKind::MacroCall, 11);
        let groups = crate_groups(&g);
        assert_eq!(names(&groups), vec![("engine", 0, 2), ("core", 1, 2)]);
    }

    #[test]
    fn a_stray_call_back_up_does_not_flip_the_order() {
        let mut g = CodeGraph::new();
        let a = plain(&mut g, "engine::run", "engine/lib.rs", 1);
        let b = plain(&mut g, "engine::step", "engine/lib.rs", 9);
        let c = plain(&mut g, "core::node", "core/lib.rs", 1);
        let d = plain(&mut g, "core::edge", "core/lib.rs", 9);
        call(&mut g, a, c, EdgeKind::DirectCall, 2);
        call(&mut g, a, d, EdgeKind::DirectCall, 3);
        call(&mut g, b, c, EdgeKind::DirectCall, 10);
        // One bare call resolved the wrong way ("first declared").
        call(&mut g, c, b, EdgeKind::DirectCall, 2);
        let groups = crate_groups(&g);
        assert_eq!(names(&groups), vec![("engine", 0, 2), ("core", 1, 2)]);
    }

    #[test]
    fn a_single_crate_splits_by_top_level_module() {
        let groups = crate_groups(&app_graph());
        let got = names(&groups);
        // `app::core::Lexer::next` and `app::core::tests::*` stay in `app::core`.
        assert_eq!(
            got,
            vec![
                ("app", 0, 3),
                ("app::core", 1, 3),
                ("app::io", 1, 2),
                (EXTERNAL_GROUP, 2, 1),
            ]
        );
    }

    #[test]
    fn a_spawned_closure_stays_with_the_module_around_its_function() {
        let mut g = CodeGraph::new();
        let main = plain(&mut g, "app::main", "src/main.rs", 1);
        let serve = plain(&mut g, "app::net::serve", "src/net.rs", 1);
        let closures = [
            "app::main::<spawned@L3>",
            "app::main::<spawned@L3>::<spawned@L5>",
            "app::net::serve::<spawned@L2>",
        ]
        .map(|qn| {
            let id = plain(&mut g, qn, "src/main.rs", 3);
            g.node_mut(id).kind = NodeKind::Closure;
            id
        });
        call(&mut g, main, closures[0], EdgeKind::Spawn, 3);
        call(&mut g, closures[0], closures[1], EdgeKind::Spawn, 5);
        call(&mut g, serve, closures[2], EdgeKind::Spawn, 2);
        let groups = crate_groups(&g);
        assert_eq!(names(&groups), vec![("app", 0, 3), ("app::net", 0, 2)]);
    }

    #[test]
    fn a_long_chain_layers_without_recursion() {
        let mut g = CodeGraph::new();
        let ids: Vec<_> = (0..5000)
            .map(|i| plain(&mut g, &format!("c{i}::f"), "x.rs", i + 1))
            .collect();
        for w in ids.windows(2) {
            call(&mut g, w[0], w[1], EdgeKind::DirectCall, 1);
        }
        let groups = crate_groups(&g);
        assert_eq!(groups.groups.len(), 5000);
        assert_eq!(groups.groups[0].name, "c0");
        assert_eq!(groups.groups[4999].name, "c4999");
        assert_eq!(groups.groups[4999].layer, 4999);
    }

    #[test]
    fn an_empty_graph_has_no_groups() {
        let groups = crate_groups(&CodeGraph::new());
        assert!(groups.groups.is_empty() && groups.of_node.is_empty());
    }
}
