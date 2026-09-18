//! # lcw-analysis (Layer 2)
//!
//! Engineering lenses and quality metrics over the [`CodeGraph`]. This is the
//! Manifesto made executable: complexity/parameter/length budgets, memory &
//! thread-safety hazards, purity hotspots, architecture layering (Clean / Onion
//! / MVC) and an OOP-vs-FP paradigm read.
//!
//! The closed interface (plan "interfaces cerradas") is:
//!
//! ```text
//! Lens::evaluate(&CodeGraph, &Config) -> Vec<Diagnostic>
//! ```
//!
//! [`analyze`] is the orchestrator the engine calls: it computes [`metrics`],
//! runs the enabled lenses, and fills an [`AnalysisReport`]. Lenses are toggled
//! by `[lenses]` in the config so the UI can surface them as overlays/filters.

pub mod lenses;
pub mod metrics;

use lcw_config::Config;
use lcw_core::{AnalysisReport, CodeGraph, Diagnostic};

/// An engineering lens: a self-contained rule producing diagnostics.
pub trait Lens {
    /// Stable machine name (matches `Diagnostic::lens`), e.g. `complexity`.
    fn name(&self) -> &'static str;

    /// Evaluate the lens against the graph, honoring config thresholds/toggles.
    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic>;
}

/// The set of lenses enabled by `config.lenses` (deterministic order).
pub fn enabled_lenses(config: &Config) -> Vec<Box<dyn Lens>> {
    let mut lenses: Vec<Box<dyn Lens>> = Vec::new();
    if config.lenses.complexity {
        lenses.push(Box::new(lenses::ComplexityLens));
    }
    if config.lenses.hazards {
        lenses.push(Box::new(lenses::HazardsLens));
    }
    if config.lenses.purity {
        lenses.push(Box::new(lenses::PurityLens));
    }
    if config.lenses.layering {
        lenses.push(Box::new(lenses::LayeringLens));
    }
    if config.lenses.paradigm {
        lenses.push(Box::new(lenses::ParadigmLens));
    }
    lenses
}

/// Run Layer 2 over `report`: compute metrics and append lens diagnostics.
///
/// Diagnostics are sorted highest-severity first (then by node, then code) so
/// output is deterministic and the worst issues surface at the top.
pub fn analyze(config: &Config, report: &mut AnalysisReport) {
    let metrics = metrics::compute(&report.graph, config);
    report.metrics.extend(metrics);

    for lens in enabled_lenses(config) {
        let diagnostics = lens.evaluate(&report.graph, config);
        report.diagnostics.extend(diagnostics);
    }

    report.diagnostics.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.node.cmp(&b.node))
            .then_with(|| a.code.cmp(&b.code))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeFlags, NodeKind, NodeStats, Severity, SourceSpan};

    fn def_node(qualified: &str, module: &str, kind: NodeKind, stats: NodeStats) -> Node {
        Node {
            id: lcw_core::NodeId(0),
            name: qualified
                .rsplit("::")
                .next()
                .unwrap_or(qualified)
                .to_string(),
            qualified_name: qualified.to_string(),
            module_path: module.to_string(),
            kind,
            span: SourceSpan::default(),
            flags: NodeFlags::default(),
            stats,
        }
    }

    #[test]
    fn complexity_lens_flags_hot_functions() {
        let mut g = CodeGraph::new();
        let stats = NodeStats {
            decision_points: 15, // cc = 16 > default max 10
            ..Default::default()
        };
        g.add_node(def_node("crate::hot", "crate", NodeKind::Function, stats));

        let diags = lenses::ComplexityLens.evaluate(&g, &Config::default());
        assert!(diags
            .iter()
            .any(|d| d.code == "high_complexity" && d.severity >= Severity::Medium));
    }

    #[test]
    fn layering_lens_flags_inner_to_outer() {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let domain = g.add_node(def_node(
            "crate::domain::User::validate",
            "crate::domain",
            NodeKind::Method,
            NodeStats::default(),
        ));
        let infra = g.add_node(def_node(
            "crate::infrastructure::db::save",
            "crate::infrastructure::db",
            NodeKind::Function,
            NodeStats::default(),
        ));
        // domain -> infrastructure is an inner->outer violation.
        g.add_edge(
            domain,
            infra,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
        );

        let mut config = Config::default();
        config.lenses.architecture = vec!["clean".to_string()];
        let diags = lenses::LayeringLens.evaluate(&g, &config);
        assert!(diags.iter().any(|d| d.code == "layering_violation"));

        // The reverse dependency (outer -> inner) is fine.
        let mut g2 = CodeGraph::new();
        let d2 = g2.add_node(def_node(
            "crate::domain::User::validate",
            "crate::domain",
            NodeKind::Method,
            NodeStats::default(),
        ));
        let i2 = g2.add_node(def_node(
            "crate::infrastructure::db::save",
            "crate::infrastructure::db",
            NodeKind::Function,
            NodeStats::default(),
        ));
        g2.add_edge(
            i2,
            d2,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
        );
        assert!(lenses::LayeringLens.evaluate(&g2, &config).is_empty());
    }

    #[test]
    fn analyze_fills_metrics_and_sorts_diagnostics() {
        let mut g = CodeGraph::new();
        g.add_node(def_node(
            "crate::hot",
            "crate",
            NodeKind::Function,
            NodeStats {
                decision_points: 30,
                unsafe_blocks: 1,
                ..Default::default()
            },
        ));
        let mut report = AnalysisReport::new(g);

        analyze(&Config::default(), &mut report);

        assert!(!report.metrics.is_empty());
        assert!(!report.diagnostics.is_empty());
        // Sorted highest severity first.
        for pair in report.diagnostics.windows(2) {
            assert!(pair[0].severity >= pair[1].severity);
        }
    }
}
