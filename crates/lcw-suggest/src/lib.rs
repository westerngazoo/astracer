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
pub mod ext;
pub mod vertical;

use lcw_config::Config;
use lcw_core::{AnalysisReport, Suggestion};

pub use ext::{extended_advisors, ExtAdvisorConfig};
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
    rescale_priorities(&mut suggestions, config);

    // 4. Highest priority first (ties broken by title for determinism), capped.
    sort_and_cap(&mut suggestions, config);

    report.suggestions = suggestions;
}

/// Run the **extended** (opt-in) advisors selected by `ext` and merge their
/// suggestions into `report.suggestions`, then re-rank and re-cap the combined
/// set.
///
/// This is the additive counterpart to [`advise`]: the built-in pass is left
/// untouched (so baseline suggestions / golden snapshots stay valid), and a
/// caller opts into the graph-shape advice by threading an enabled
/// [`ExtAdvisorConfig`] here — typically from the engine's `with_advisor_stage`
/// hook, after [`advise`] has set `report.vertical`. Only the newly-produced
/// suggestions are weight-scaled (the existing ones were already scaled by
/// [`advise`]), so priorities are never double-counted. With the default
/// (disabled) config it is a no-op.
pub fn advise_extended(config: &Config, ext: &ExtAdvisorConfig, report: &mut AnalysisReport) {
    let mut extra = Vec::new();
    for advisor in extended_advisors(ext) {
        extra.extend(advisor.advise(report, config));
    }
    if extra.is_empty() {
        return;
    }
    rescale_priorities(&mut extra, config);
    report.suggestions.append(&mut extra);
    sort_and_cap(&mut report.suggestions, config);
}

/// Rescale each suggestion's base priority by the strongest configured weight
/// among the optimization targets it serves (empty targets keep weight `1.0`).
fn rescale_priorities(suggestions: &mut [Suggestion], config: &Config) {
    for suggestion in suggestions.iter_mut() {
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
}

/// Sort suggestions highest-priority first (ties broken by title for
/// determinism) and truncate to the configured cap.
fn sort_and_cap(suggestions: &mut Vec<Suggestion>, config: &Config) {
    suggestions.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.title.cmp(&b.title))
    });
    suggestions.truncate(config.suggestions.max_suggestions);
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

    #[test]
    fn default_advisor_set_is_unchanged() {
        // Locks the invariant that the extended (opt-in) advisors never enter
        // the default pass, so baseline suggestion output stays stable.
        assert_eq!(advisors::all_advisors().len(), 7);
    }

    #[test]
    fn advise_extended_is_opt_in_and_preserves_base_priorities() {
        let mut report = AnalysisReport::new(backend_graph());
        // A finding only an extended lens would produce.
        report
            .diagnostics
            .push(Diagnostic::new("hotspot", "hotspot", Severity::High, "x"));
        advise(&Config::default(), &mut report);

        // Snapshot the base suggestions' priorities.
        let before: Vec<(String, u8)> = report
            .suggestions
            .iter()
            .map(|s| (s.title.clone(), s.priority))
            .collect();

        // Disabled config: a strict no-op.
        advise_extended(
            &Config::default(),
            &ExtAdvisorConfig::default(),
            &mut report,
        );
        let after_disabled: Vec<(String, u8)> = report
            .suggestions
            .iter()
            .map(|s| (s.title.clone(), s.priority))
            .collect();
        assert_eq!(before, after_disabled);

        // Enabling the advisors adds hotspot advice without re-scaling the
        // already-scaled base suggestions (no double counting).
        advise_extended(&Config::default(), &ExtAdvisorConfig::all(), &mut report);
        assert!(report
            .suggestions
            .iter()
            .any(|s| s.title.contains("hotspot")));
        for (title, prio) in before {
            let now = report
                .suggestions
                .iter()
                .find(|s| s.title == title)
                .expect("base suggestion should survive the merge");
            assert_eq!(now.priority, prio, "base priority must be untouched");
        }
        // Combined set stays ranked highest-priority first.
        for pair in report.suggestions.windows(2) {
            assert!(pair[0].priority >= pair[1].priority);
        }
    }
}
