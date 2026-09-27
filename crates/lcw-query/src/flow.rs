//! Flow tracing: the shortest call path(s) from an entry point to a target.
//!
//! A breadth-first search over the call graph is exactly "how does control get
//! from here to there in the fewest hops". Because every edge costs one hop,
//! BFS discovers nodes in non-decreasing distance, so the first time a target
//! is dequeued its distance is minimal; we finish that layer to collect *every*
//! shortest-path predecessor and can then enumerate a few alternate routes of
//! the same length.

use std::collections::{HashMap, HashSet, VecDeque};

use lcw_core::{CodeGraph, NodeId};

/// A traced set of shortest paths from `source` to `target`.
#[derive(Debug, Clone)]
pub struct FlowPaths {
    pub source: NodeId,
    pub target: NodeId,
    /// One or more equally-short paths, each `source .. target` inclusive.
    pub paths: Vec<Vec<NodeId>>,
    /// Distance (in hops) from the source for every node the search touched.
    /// The layered flow diagram uses it as the column index.
    pub depth: HashMap<NodeId, u32>,
}

impl FlowPaths {
    /// Number of hops on the (shortest) traced path.
    pub fn hops(&self) -> usize {
        self.paths
            .first()
            .map(|p| p.len().saturating_sub(1))
            .unwrap_or(0)
    }
}

/// Breadth-first search for the shortest path(s) from any `sources` node to the
/// nearest `targets` node, recording *all* shortest-path predecessors so up to
/// `max_paths` alternate routes of the same length can be enumerated. Stops
/// expanding past `max_depth` hops.
pub fn shortest_paths(
    graph: &CodeGraph,
    sources: &[NodeId],
    targets: &[NodeId],
    max_paths: usize,
    max_depth: u32,
) -> Option<FlowPaths> {
    if sources.is_empty() || targets.is_empty() {
        return None;
    }
    let target_set: HashSet<NodeId> = targets.iter().copied().collect();
    let source_set: HashSet<NodeId> = sources.iter().copied().collect();

    let mut dist: HashMap<NodeId, u32> = HashMap::new();
    let mut parents: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    for &s in sources {
        if dist.insert(s, 0).is_none() {
            queue.push_back(s);
        }
    }

    let mut found_at: Option<u32> = None;
    let mut reached: Option<NodeId> = None;
    while let Some(u) = queue.pop_front() {
        let du = dist[&u];
        if let Some(fd) = found_at {
            if du > fd {
                break; // fully explored the layer that first hit a target
            }
        }
        if target_set.contains(&u) {
            found_at = Some(du);
            reached.get_or_insert(u);
            continue; // don't expand past a target
        }
        if du >= max_depth {
            continue;
        }
        for v in graph.neighbors_out(u) {
            let dv = du + 1;
            match dist.get(&v) {
                None => {
                    dist.insert(v, dv);
                    parents.entry(v).or_default().push(u);
                    queue.push_back(v);
                }
                Some(&existing) if existing == dv && !source_set.contains(&v) => {
                    parents.entry(v).or_default().push(u);
                }
                _ => {}
            }
        }
    }

    let target = reached?;
    let source = sources.iter().copied().find(|s| dist.get(s) == Some(&0))?;

    let mut paths: Vec<Vec<NodeId>> = Vec::new();
    let mut stack: Vec<NodeId> = Vec::new();
    enumerate(
        target,
        &parents,
        &source_set,
        &mut stack,
        &mut paths,
        max_paths.max(1),
    );
    if paths.is_empty() {
        return None;
    }
    Some(FlowPaths {
        source,
        target,
        paths,
        depth: dist,
    })
}

/// Convenience: a single shortest path `from .. to` (inclusive), or `None` when
/// `to` is unreachable within `max_depth` hops. `from == to` yields `[from]`.
pub fn shortest_path(
    graph: &CodeGraph,
    from: NodeId,
    to: NodeId,
    max_depth: u32,
) -> Option<Vec<NodeId>> {
    if from == to {
        return Some(vec![from]);
    }
    shortest_paths(graph, &[from], &[to], 1, max_depth).and_then(|fp| fp.paths.into_iter().next())
}

/// How two nodes are connected by calls, looked for in both directions.
///
/// "Flow from here" has an obvious reading — where does control go from the
/// source — and an equally common intent the one-way search silently fails:
/// the reader anchors a deep function and clicks `main`, asking *how does
/// control get here*. Answering only the first leaves the second looking like
/// a broken feature, so both directions are tried and the answer says which
/// one held. An empty path is never the answer: "no connection" is its own
/// variant, so a front end can say so rather than showing nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// Source and target are the same node.
    Same,
    /// The source calls its way to the target: the path runs
    /// `source ..= target`, in call order.
    Downstream(Vec<NodeId>),
    /// The source does not reach the target, but the target reaches the
    /// source: the path runs `target ..= source`, in call order.
    Upstream(Vec<NodeId>),
    /// No call path either way within the depth bound. In fast mode this is
    /// often a dynamic call (trait object, function pointer, closure) or an
    /// unresolved one, not a true absence.
    Unconnected,
}

impl Connection {
    /// The traced path in call order, or empty.
    pub fn path(&self) -> &[NodeId] {
        match self {
            Connection::Downstream(p) | Connection::Upstream(p) => p,
            Connection::Same | Connection::Unconnected => &[],
        }
    }
}

/// Find how `source` and `target` connect: downstream first (the direct
/// reading of "flow from here"), then upstream. Each search is a shortest
/// path, so whichever direction holds is reported in the fewest hops.
pub fn connection(graph: &CodeGraph, source: NodeId, target: NodeId, max_depth: u32) -> Connection {
    if source == target {
        return Connection::Same;
    }
    if let Some(p) = shortest_path(graph, source, target, max_depth) {
        return Connection::Downstream(p);
    }
    if let Some(p) = shortest_path(graph, target, source, max_depth) {
        return Connection::Upstream(p);
    }
    Connection::Unconnected
}

/// Walk predecessors backward from `node` to every source, emitting each
/// distinct path (in forward order) until `max` have been collected.
fn enumerate(
    node: NodeId,
    parents: &HashMap<NodeId, Vec<NodeId>>,
    sources: &HashSet<NodeId>,
    stack: &mut Vec<NodeId>,
    out: &mut Vec<Vec<NodeId>>,
    max: usize,
) {
    if out.len() >= max {
        return;
    }
    stack.push(node);
    match parents.get(&node) {
        None => {
            // A node with no shortest-path predecessor is a source.
            if sources.contains(&node) {
                out.push(stack.iter().rev().copied().collect());
            }
        }
        Some(preds) => {
            for &p in preds {
                enumerate(p, parents, sources, stack, out, max);
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    stack.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, call, id, plain};
    use lcw_core::EdgeKind;
    #[test]
    fn connection_prefers_downstream_and_falls_back_to_upstream() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let parse = id(&g, "app::core::parse");

        // Anchor at main, pick parse: the ordinary reading.
        match connection(&g, main, parse, 16) {
            Connection::Downstream(p) => {
                assert_eq!(p.first(), Some(&main));
                assert_eq!(p.last(), Some(&parse));
            }
            other => panic!("expected downstream, got {other:?}"),
        }

        // Anchor at parse, pick main: the one-way search found nothing here
        // and the viewer showed nothing. Now it is the same path, flagged as
        // upstream and still in call order.
        match connection(&g, parse, main, 16) {
            Connection::Upstream(p) => {
                assert_eq!(p.first(), Some(&main), "call order: caller first");
                assert_eq!(p.last(), Some(&parse));
            }
            other => panic!("expected upstream, got {other:?}"),
        }
    }

    #[test]
    fn connection_names_the_empty_cases_instead_of_returning_nothing() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let orphan = id(&g, "app::orphan");
        assert_eq!(connection(&g, main, main, 16), Connection::Same);
        assert_eq!(connection(&g, main, orphan, 16), Connection::Unconnected);
        assert_eq!(connection(&g, orphan, main, 16), Connection::Unconnected);
        assert!(Connection::Unconnected.path().is_empty());
    }

    #[test]
    fn connection_respects_the_depth_bound_in_both_directions() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let parse = id(&g, "app::core::parse");
        let hops = shortest_path(&g, main, parse, 16).unwrap().len() - 1;
        assert!(hops >= 2, "fixture needs a multi-hop path");
        assert_eq!(
            connection(&g, main, parse, (hops - 1) as u32),
            Connection::Unconnected
        );
        assert_eq!(
            connection(&g, parse, main, (hops - 1) as u32),
            Connection::Unconnected
        );
    }

    #[test]
    fn finds_the_shortest_route_and_records_depths() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let next = id(&g, "app::core::Lexer::next");
        let fp = shortest_paths(&g, &[main], &[next], 4, 16).expect("reachable");
        assert_eq!(fp.hops(), 3);
        assert_eq!(fp.paths.len(), 1);
        assert_eq!(
            fp.paths[0],
            vec![main, id(&g, "app::run"), id(&g, "app::core::parse"), next]
        );
        assert_eq!(fp.depth[&main], 0);
        assert_eq!(fp.depth[&next], 3);
    }

    #[test]
    fn enumerates_alternate_equal_length_paths() {
        // a -> b -> d and a -> c -> d are both 2 hops.
        let mut g = CodeGraph::new();
        let a = plain(&mut g, "m::a", "src/lib.rs", 1);
        let b = plain(&mut g, "m::b", "src/lib.rs", 5);
        let c = plain(&mut g, "m::c", "src/lib.rs", 9);
        let d = plain(&mut g, "m::d", "src/lib.rs", 13);
        call(&mut g, a, b, EdgeKind::DirectCall, 2);
        call(&mut g, a, c, EdgeKind::DirectCall, 3);
        call(&mut g, b, d, EdgeKind::DirectCall, 6);
        call(&mut g, c, d, EdgeKind::DirectCall, 10);
        let fp = shortest_paths(&g, &[a], &[d], 8, 8).unwrap();
        assert_eq!(fp.paths.len(), 2);
        assert!(fp.paths.iter().all(|p| p.len() == 3));
        // Capping keeps only the first.
        assert_eq!(shortest_paths(&g, &[a], &[d], 1, 8).unwrap().paths.len(), 1);
    }

    #[test]
    fn respects_direction_and_depth_limit() {
        let g = app_graph();
        let main = id(&g, "app::main");
        let next = id(&g, "app::core::Lexer::next");
        // Calls only flow forward: nothing leads *back* to main.
        assert!(shortest_path(&g, next, main, 16).is_none());
        // Too shallow a search misses a 3-hop path.
        assert!(shortest_path(&g, main, next, 2).is_none());
        assert_eq!(shortest_path(&g, main, main, 16), Some(vec![main]));
    }

    #[test]
    fn empty_inputs_yield_none() {
        let g = app_graph();
        assert!(shortest_paths(&g, &[], &[NodeId(0)], 1, 8).is_none());
        assert!(shortest_paths(&g, &[NodeId(0)], &[], 1, 8).is_none());
    }
}
