//! Quantitative metrics (Manifesto section 4). These are *measurements* — the
//! lenses turn them (and the raw graph) into [`lcw_core::Diagnostic`]s.
//!
//! Per-node metrics are emitted for defined functions/methods (external /
//! unresolved targets are skipped). One project-level metric, `Cyclicity`,
//! measures how much of the graph sits inside a dependency cycle.

use std::collections::HashSet;

use lcw_config::Config;
use lcw_core::{CodeGraph, Metric, MetricKind, Node, NodeKind};
use petgraph::algo::tarjan_scc;

/// Compute the full metric set for `graph` under `config`.
pub fn compute(graph: &CodeGraph, config: &Config) -> Vec<Metric> {
    let mut metrics = Vec::new();

    for node in graph.nodes() {
        if node.kind == NodeKind::External {
            continue;
        }
        let id = node.id;
        metrics.push(Metric::node(
            MetricKind::CyclomaticComplexity,
            id,
            node.cyclomatic_complexity() as f64,
        ));
        metrics.push(Metric::node(
            MetricKind::FanIn,
            id,
            graph.in_degree(id) as f64,
        ));
        metrics.push(Metric::node(
            MetricKind::FanOut,
            id,
            graph.out_degree(id) as f64,
        ));
        let allocations = node.stats.allocations as f64 * config.metrics.heap_sensitivity;
        metrics.push(Metric::node(MetricKind::HeapAllocations, id, allocations));
        metrics.push(Metric::node(
            MetricKind::PurityScore,
            id,
            purity_score(node),
        ));
    }

    metrics.push(Metric::project(MetricKind::Cyclicity, cyclicity(graph)));
    metrics
}

/// Heuristic purity in `0.0..=1.0`: 1.0 is "looks pure", lower means more
/// observable side effects. Purely structural (no type info), so it is a hint,
/// not a proof.
pub(crate) fn purity_score(node: &Node) -> f64 {
    let mut score = 1.0f64;
    if node.stats.allocations > 0 {
        score -= 0.35;
    }
    if node.stats.unsafe_blocks > 0 || node.flags.is_unsafe {
        score -= 0.30;
    }
    if node.flags.is_async || node.stats.awaits > 0 {
        score -= 0.20;
    }
    score.clamp(0.0, 1.0)
}

/// Fraction of nodes that participate in a dependency cycle: nodes inside a
/// strongly-connected component larger than one, plus direct self-recursion.
fn cyclicity(graph: &CodeGraph) -> f64 {
    let total = graph.node_count();
    if total == 0 {
        return 0.0;
    }

    let mut cyclic: HashSet<u32> = HashSet::new();
    for component in tarjan_scc(graph.raw()) {
        if component.len() > 1 {
            for idx in component {
                cyclic.insert(idx.index() as u32);
            }
        }
    }
    // Self-recursive functions are a 1-node cycle tarjan_scc doesn't group.
    for (from, to, _) in graph.edges() {
        if from == to {
            cyclic.insert(from.0);
        }
    }

    cyclic.len() as f64 / total as f64
}
