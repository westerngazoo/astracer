//! # lcw-suggest (Layer 3)
//!
//! Turns analysis into advice. It detects the industry [`Vertical`] (embedded /
//! game engine / backend / full-stack) and runs a set of [`Advisor`]s that emit
//! [`Suggestion`]s tuned to optimization targets (scalability, maintainability,
//! robustness, latency, performance).
//!
//! The closed interface (plan "interfaces cerradas") is:
//!
//! ```text
//! Advisor::advise(&AnalysisReport, &Config) -> Vec<Suggestion>
//! ```
//!
//! [`advise`] is the orchestrator the engine calls: it sets `report.vertical`,
//! runs the advisors, rescales priorities by the config's per-target weights and
//! keeps the top `max_suggestions`.

pub mod advisors;
pub mod vertical;

use lcw_config::Config;
use lcw_core::{AnalysisReport, Suggestion};

pub use vertical::{detect as detect_vertical, VerticalScores};

/// An advisor: a rule producing target-tagged suggestions from a report.
pub trait Advisor {
    /// Stable machine name, e.g. `complexity`.
    fn name(&self) -> &'static str;

    /// Produce suggestions for `report` under `config`.
    fn advise(&self, report: &AnalysisReport, config: &Config) -> Vec<Suggestion>;
}

/// Run Layer 3 over `report`: detect the vertical, gather suggestions, rescale
/// priorities by configured target weights, sort and cap.
pub fn advise(config: &Config, report: &mut AnalysisReport) {
    // 1. Resolve the vertical: an explicit config choice wins over detection.
    let vertical = config
        .project
        .vertical
        .fixed()
        .unwrap_or_else(|| vertical::detect(report));
    report.vertical = vertical;

    // 2. Gather raw suggestions.
    let mut suggestions = Vec::new();
    for advisor in advisors::all_advisors() {
        suggestions.extend(advisor.advise(report, config));
    }

    // 3. Rescale priority by the strongest configured weight among its targets.
    for suggestion in &mut suggestions {
        let weight = if suggestion.targets.is_empty() {
            1.0
        } else {
            suggestion
                .targets
                .iter()
                .map(|t| config.suggestions.weight(*t))
                .fold(f64::MIN, f64::max)
        };
        let scaled = (suggestion.priority as f64 * weight)
            .round()
            .clamp(0.0, 100.0);
        suggestion.priority = scaled as u8;
    }

    // 4. Highest priority first (ties broken by title for determinism), capped.
    suggestions.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.title.cmp(&b.title))
    });
    suggestions.truncate(config.suggestions.max_suggestions);

    report.suggestions = suggestions;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{
        CodeGraph, Diagnostic, Metric, MetricKind, Node, NodeFlags, NodeKind, NodeStats, Severity,
        SourceSpan, Target, Vertical,
    };

    fn backend_graph() -> CodeGraph {
        let mut g = CodeGraph::new();
        for qn in [
            "crate::server::router::route",
            "crate::service::handler::handle",
            "crate::repository::database::query",
            "axum::serve",
        ] {
            g.add_node(Node {
                id: lcw_core::NodeId(0),
                name: qn.rsplit("::").next().unwrap().to_string(),
                qualified_name: qn.to_string(),
                module_path: qn
                    .rsplit_once("::")
                    .map(|(m, _)| m.to_string())
                    .unwrap_or_default(),
                kind: NodeKind::Function,
                span: SourceSpan::default(),
                flags: NodeFlags::default(),
                stats: NodeStats::default(),
            });
        }
        g
    }

    #[test]
    fn detects_backend_vertical() {
        let report = AnalysisReport::new(backend_graph());
        assert_eq!(vertical::detect(&report), Vertical::Backend);
    }

    #[test]
    fn advise_sets_vertical_and_builds_suggestions() {
        let mut report = AnalysisReport::new(backend_graph());
        // Pretend Layer 2 flagged some complexity + a cycle.
        report.diagnostics.push(Diagnostic::new(
            "complexity",
            "high_complexity",
            Severity::Medium,
            "x",
        ));
        report
            .metrics
            .push(Metric::project(MetricKind::Cyclicity, 0.25));

        advise(&Config::default(), &mut report);

        assert_eq!(report.vertical, Vertical::Backend);
        assert!(report
            .suggestions
            .iter()
            .any(|s| s.title.contains("cyclomatic")));
        assert!(report
            .suggestions
            .iter()
            .any(|s| s.title.contains("cycles")));
        // The vertical advisor always contributes for a known vertical.
        assert!(report
            .suggestions
            .iter()
            .any(|s| s.targets.contains(&Target::Scalability)));
        // Sorted by priority (desc).
        for pair in report.suggestions.windows(2) {
            assert!(pair[0].priority >= pair[1].priority);
        }
    }

    #[test]
    fn config_weights_scale_priority() {
        let mut report = AnalysisReport::new(backend_graph());
        report.diagnostics.push(Diagnostic::new(
            "hazards",
            "unsafe_usage",
            Severity::Medium,
            "x",
        ));

        let mut config = Config::default();
        config.suggestions.robustness = 0.0; // zero out robustness weight
        advise(&config, &mut report);

        // The unsafe advisor targets Robustness only, so a 0 weight zeroes it.
        let unsafe_sugg = report
            .suggestions
            .iter()
            .find(|s| s.title.contains("unsafe"));
        assert!(matches!(unsafe_sugg, Some(s) if s.priority == 0));
    }
}
