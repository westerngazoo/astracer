//! Extended advisors (Layer 3, opt-in).
//!
//! These turn the *extended* Layer 2 findings (cycles, dead code, god
//! functions, hotspots, unstable dependencies — see `lcw_analysis::ext`) into
//! target-tagged [`Suggestion`]s, tuned to the detected [`Vertical`].
//!
//! Like their lens counterparts they are **disabled by default**: the built-in
//! [`crate::advise`] pass is left untouched so baseline suggestion output (and
//! any golden snapshot depending on it) stays identical. A caller opts in by
//! running [`crate::advise_extended`] (e.g. from the engine's
//! `with_advisor_stage` hook) with an enabled [`ExtAdvisorConfig`]. Even when
//! enabled, each advisor only fires if the matching extended lens produced
//! diagnostics, so the two layers stay in lock-step.

use lcw_core::{AnalysisReport, Suggestion, Target, Vertical};

use crate::Advisor;

/// Which extended advisors to run. Every flag defaults to `false`, so the
/// default value is fully inert.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtAdvisorConfig {
    /// Advice for `recursion_cycle` / `self_recursion` diagnostics.
    pub recursion_cycles: bool,
    /// Advice for `dead_code` diagnostics.
    pub dead_code: bool,
    /// Advice for `god_function` diagnostics.
    pub god_function: bool,
    /// Advice for `hotspot` diagnostics.
    pub hotspot: bool,
    /// Advice for `unstable_dependency` diagnostics.
    pub unstable_dependency: bool,
}

impl ExtAdvisorConfig {
    /// Enable every extended advisor.
    pub fn all() -> Self {
        ExtAdvisorConfig {
            recursion_cycles: true,
            dead_code: true,
            god_function: true,
            hotspot: true,
            unstable_dependency: true,
        }
    }

    /// True when no extended advisor is enabled (the default state).
    pub fn is_disabled(&self) -> bool {
        !(self.recursion_cycles
            || self.dead_code
            || self.god_function
            || self.hotspot
            || self.unstable_dependency)
    }
}

/// The extended advisors enabled by `cfg` (deterministic order). Empty for the
/// default (disabled) config.
pub fn extended_advisors(cfg: &ExtAdvisorConfig) -> Vec<Box<dyn Advisor>> {
    let mut advisors: Vec<Box<dyn Advisor>> = Vec::new();
    if cfg.recursion_cycles {
        advisors.push(Box::new(RecursionCycleAdvisor));
    }
    if cfg.dead_code {
        advisors.push(Box::new(DeadCodeAdvisor));
    }
    if cfg.god_function {
        advisors.push(Box::new(GodFunctionAdvisor));
    }
    if cfg.hotspot {
        advisors.push(Box::new(HotspotAdvisor));
    }
    if cfg.unstable_dependency {
        advisors.push(Box::new(UnstableDependencyAdvisor));
    }
    advisors
}

/// Break call cycles / bound recursion (maintainability, with robustness on
/// stack-constrained verticals).
pub struct RecursionCycleAdvisor;
impl Advisor for RecursionCycleAdvisor {
    fn name(&self) -> &'static str {
        "recursion_cycles"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let cycles = count(report, "recursion_cycle");
        let self_rec = count(report, "self_recursion");
        if cycles == 0 && self_rec == 0 {
            return Vec::new();
        }

        let mut targets = vec![Target::Maintainability, Target::Scalability];
        let mut rationale = String::from(
            "Cyclic call structures block modular reasoning and incremental compilation; \
             introduce trait boundaries / dependency inversion to cut the cycles.",
        );
        // On stack-constrained or real-time verticals, unbounded recursion is
        // also a crash risk, so robustness earns a seat at the table.
        if matches!(report.vertical, Vertical::Embedded | Vertical::GameEngine) {
            targets.push(Target::Robustness);
            rationale.push_str(
                " Bound recursion depth (or make it iterative) to protect a limited stack.",
            );
        }

        let title = if cycles > 0 {
            format!("Break call cycles entangling {cycles} function(s)")
        } else {
            format!("Bound {self_rec} self-recursive function(s)")
        };
        vec![Suggestion::new(title, rationale)
            .with_targets(targets)
            .with_priority(cap(60 + cycles * 2 + self_rec, 90))]
    }
}

/// Remove or cover unreachable code (robustness + maintainability).
pub struct DeadCodeAdvisor;
impl Advisor for DeadCodeAdvisor {
    fn name(&self) -> &'static str {
        "dead_code"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "dead_code");
        if n == 0 {
            return Vec::new();
        }
        let mut rationale = String::from(
            "Unreachable functions are untested liabilities: delete them, or — if they are real \
             entrypoints/handlers — wire them in and cover them with tests.",
        );
        if report.vertical == Vertical::Embedded {
            rationale.push_str(" On embedded targets dead code also wastes limited flash/ROM.");
        }
        vec![Suggestion::new(
            format!("Remove or cover {n} unreachable function(s)"),
            rationale,
        )
        .with_targets([Target::Robustness, Target::Maintainability])
        .with_priority(cap(55 + n * 2, 85))]
    }
}

/// Split god functions (maintainability + scalability).
pub struct GodFunctionAdvisor;
impl Advisor for GodFunctionAdvisor {
    fn name(&self) -> &'static str {
        "god_function"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "god_function");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Split {n} god function(s) with high fan-in and fan-out"),
            "A function many callers depend on that itself reaches into many callees concentrates \
             change risk; extract cohesive helpers and depend on narrower interfaces.",
        )
        .with_targets([Target::Maintainability, Target::Scalability])
        .with_priority(cap(60 + n * 3, 90))]
    }
}

/// Optimize call hotspots (performance + latency, vertical-tuned).
pub struct HotspotAdvisor;
impl Advisor for HotspotAdvisor {
    fn name(&self) -> &'static str {
        "hotspot"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "hotspot");
        if n == 0 {
            return Vec::new();
        }
        // Perf-sensitive verticals care more, and want different tactics.
        let (extra, bump) = match report.vertical {
            Vertical::GameEngine => (
                " Keep them off the per-frame hot path: cache results and avoid per-call allocation.",
                8usize,
            ),
            Vertical::Backend => (
                " Add caching/memoization and watch them under load; they gate throughput.",
                4,
            ),
            Vertical::Embedded => (" Budget cycles carefully on constrained hardware.", 6),
            _ => ("", 0),
        };
        let mut rationale = String::from(
            "Hotspots sit on many execution paths, so improving them pays off broadly: reduce work, \
             memoize pure results and cut allocations on the path.",
        );
        rationale.push_str(extra);
        vec![Suggestion::new(
            format!("Optimize {n} call hotspot(s) on critical paths"),
            rationale,
        )
        .with_targets([Target::Performance, Target::Latency])
        .with_priority(cap(60 + n * 2 + bump, 90))]
    }
}

/// Stabilize unstable dependency chains (maintainability + robustness).
pub struct UnstableDependencyAdvisor;
impl Advisor for UnstableDependencyAdvisor {
    fn name(&self) -> &'static str {
        "unstable_dependency"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "unstable_dependency");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Stabilize {n} unstable dependency chain(s)"),
            "Depending heavily on complex or frequently-changing callees makes a function fragile; \
             invert the dependency behind a stable interface so churn stops rippling upward.",
        )
        .with_targets([Target::Maintainability, Target::Robustness])
        .with_priority(cap(55 + n * 2, 82))]
    }
}

// --- helpers ---------------------------------------------------------------

/// Number of diagnostics carrying a given code.
fn count(report: &AnalysisReport, code: &str) -> usize {
    report.diagnostics.iter().filter(|d| d.code == code).count()
}

/// Clamp `base` to `ceiling` and narrow to the `u8` priority space.
fn cap(base: usize, ceiling: u8) -> u8 {
    base.min(ceiling as usize) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{AnalysisReport, Diagnostic, Severity};

    fn report_with(codes: &[(&str, Severity)], vertical: Vertical) -> AnalysisReport {
        let mut report = AnalysisReport {
            vertical,
            ..Default::default()
        };
        for (code, sev) in codes {
            report
                .diagnostics
                .push(Diagnostic::new("ext", *code, *sev, "x"));
        }
        report
    }

    #[test]
    fn recursion_advisor_is_vertical_aware() {
        let base = report_with(&[("recursion_cycle", Severity::Medium)], Vertical::Backend);
        let s = RecursionCycleAdvisor.advise(&base, &lcw_config::Config::default());
        assert_eq!(s.len(), 1);
        assert!(s[0].title.contains("Break call cycles"));
        assert!(s[0].targets.contains(&Target::Maintainability));
        // Backend does not add the stack-safety robustness angle.
        assert!(!s[0].targets.contains(&Target::Robustness));

        let embedded = report_with(&[("recursion_cycle", Severity::Medium)], Vertical::Embedded);
        let se = RecursionCycleAdvisor.advise(&embedded, &lcw_config::Config::default());
        assert!(se[0].targets.contains(&Target::Robustness));
        assert!(se[0].rationale.contains("stack"));
    }

    #[test]
    fn recursion_advisor_titles_self_recursion_when_no_cycles() {
        let r = report_with(&[("self_recursion", Severity::Low)], Vertical::Unknown);
        let s = RecursionCycleAdvisor.advise(&r, &lcw_config::Config::default());
        assert!(s[0].title.contains("self-recursive"));
    }

    #[test]
    fn dead_code_advisor_targets_robustness() {
        let r = report_with(
            &[("dead_code", Severity::Low), ("dead_code", Severity::Low)],
            Vertical::Unknown,
        );
        let s = DeadCodeAdvisor.advise(&r, &lcw_config::Config::default());
        assert_eq!(s.len(), 1);
        assert!(s[0].title.contains('2'));
        assert!(s[0].targets.contains(&Target::Robustness));

        // Embedded gets the flash/ROM note.
        let emb = report_with(&[("dead_code", Severity::Low)], Vertical::Embedded);
        let se = DeadCodeAdvisor.advise(&emb, &lcw_config::Config::default());
        assert!(se[0].rationale.contains("flash"));
    }

    #[test]
    fn god_function_and_unstable_advisors_fire_on_their_codes() {
        let g = report_with(&[("god_function", Severity::High)], Vertical::Unknown);
        let sg = GodFunctionAdvisor.advise(&g, &lcw_config::Config::default());
        assert!(sg[0].title.contains("god function"));
        assert!(sg[0].targets.contains(&Target::Maintainability));

        let u = report_with(
            &[("unstable_dependency", Severity::Medium)],
            Vertical::Unknown,
        );
        let su = UnstableDependencyAdvisor.advise(&u, &lcw_config::Config::default());
        assert!(su[0].title.contains("unstable dependency"));
        assert!(su[0].targets.contains(&Target::Robustness));
    }

    #[test]
    fn hotspot_advisor_bumps_priority_for_realtime_verticals() {
        let generic = report_with(&[("hotspot", Severity::Medium)], Vertical::Unknown);
        let sg = HotspotAdvisor.advise(&generic, &lcw_config::Config::default());
        let game = report_with(&[("hotspot", Severity::Medium)], Vertical::GameEngine);
        let sgame = HotspotAdvisor.advise(&game, &lcw_config::Config::default());
        assert!(sg[0].targets.contains(&Target::Performance));
        // The game-engine variant is prioritized higher and mentions frames.
        assert!(sgame[0].priority > sg[0].priority);
        assert!(sgame[0].rationale.contains("per-frame"));
    }

    #[test]
    fn advisors_are_silent_without_their_diagnostics() {
        let empty = AnalysisReport::default();
        let cfg = lcw_config::Config::default();
        assert!(RecursionCycleAdvisor.advise(&empty, &cfg).is_empty());
        assert!(DeadCodeAdvisor.advise(&empty, &cfg).is_empty());
        assert!(GodFunctionAdvisor.advise(&empty, &cfg).is_empty());
        assert!(HotspotAdvisor.advise(&empty, &cfg).is_empty());
        assert!(UnstableDependencyAdvisor.advise(&empty, &cfg).is_empty());
    }

    #[test]
    fn config_gating_selects_advisors() {
        assert!(ExtAdvisorConfig::default().is_disabled());
        assert!(extended_advisors(&ExtAdvisorConfig::default()).is_empty());
        assert_eq!(extended_advisors(&ExtAdvisorConfig::all()).len(), 5);

        let only_hotspot = ExtAdvisorConfig {
            hotspot: true,
            ..Default::default()
        };
        let selected = extended_advisors(&only_hotspot);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "hotspot");
    }
}
