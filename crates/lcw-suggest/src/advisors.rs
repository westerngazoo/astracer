//! Concrete advisors (Layer 3 rules). Each turns Layer 2 findings + the
//! detected vertical into actionable [`Suggestion`]s tagged with the
//! optimization [`Target`]s they serve. Priorities here are *base* weights; the
//! orchestrator rescales them by the config's per-target priorities.

use lcw_core::{AnalysisReport, MetricKind, Suggestion, Target, Vertical};

use crate::Advisor;

/// Number of diagnostics with a given code.
fn count(report: &AnalysisReport, code: &str) -> usize {
    report.diagnostics.iter().filter(|d| d.code == code).count()
}

/// Value of a project-level metric (or 0.0 if absent).
fn project_metric(report: &AnalysisReport, kind: MetricKind) -> f64 {
    report
        .metrics
        .iter()
        .find(|m| m.node.is_none() && m.kind == kind)
        .map(|m| m.value)
        .unwrap_or(0.0)
}

fn cap(base: usize, ceiling: u8) -> u8 {
    base.min(ceiling as usize) as u8
}

/// Refactor high-complexity functions.
pub struct ComplexityAdvisor;
impl Advisor for ComplexityAdvisor {
    fn name(&self) -> &'static str {
        "complexity"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "high_complexity");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Reduce cyclomatic complexity in {n} function(s)"),
            "High-complexity functions are hard to test and reason about; extract helpers, \
             flatten branching and replace nested conditionals with early returns.",
        )
        .with_targets([Target::Maintainability, Target::Robustness])
        .with_priority(cap(60 + n * 3, 90))]
    }
}

/// Break dependency cycles.
pub struct CyclesAdvisor;
impl Advisor for CyclesAdvisor {
    fn name(&self) -> &'static str {
        "cycles"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let cyclicity = project_metric(report, MetricKind::Cyclicity);
        if cyclicity <= 0.0 {
            return Vec::new();
        }
        let pct = (cyclicity * 100.0).round();
        vec![Suggestion::new(
            format!("Break dependency cycles ({pct:.0}% of functions are entangled)"),
            "Cyclic call structures block modular reasoning and incremental compilation; \
             introduce trait boundaries / dependency inversion to cut the cycles.",
        )
        .with_targets([Target::Maintainability, Target::Scalability])
        .with_priority(cap(60 + pct as usize, 90))]
    }
}

/// Encapsulate `unsafe`.
pub struct HazardsAdvisor;
impl Advisor for HazardsAdvisor {
    fn name(&self) -> &'static str {
        "hazards"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "unsafe_usage");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Audit and encapsulate `unsafe` in {n} function(s)"),
            "Confine `unsafe` behind safe abstractions with documented invariants, and cover \
             them with tests/miri to protect memory and thread safety.",
        )
        .with_targets([Target::Robustness])
        .with_priority(cap(65 + n * 2, 88))]
    }
}

/// Reduce heap-allocation pressure.
pub struct AllocationAdvisor;
impl Advisor for AllocationAdvisor {
    fn name(&self) -> &'static str {
        "allocations"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "heap_pressure");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Reduce heap allocations in {n} hot function(s)"),
            "Prefer borrowing, pre-sized buffers and arena/stack reuse over per-call allocation \
             to cut allocator pressure and tail latency.",
        )
        .with_targets([Target::Performance, Target::Latency])
        .with_priority(cap(60 + n * 2, 85))]
    }
}

/// Restore architecture boundaries.
pub struct LayeringAdvisor;
impl Advisor for LayeringAdvisor {
    fn name(&self) -> &'static str {
        "layering"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "layering_violation") + count(report, "mvc_violation");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Restore architecture boundaries ({n} layering violation(s))"),
            "Inner layers should not depend on outer ones; invert the offending dependencies \
             (ports/adapters) so the domain stays framework-agnostic.",
        )
        .with_targets([Target::Maintainability, Target::Scalability])
        .with_priority(cap(62 + n * 2, 88))]
    }
}

/// Isolate side effects in widely-used impure functions.
pub struct PurityAdvisor;
impl Advisor for PurityAdvisor {
    fn name(&self) -> &'static str {
        "purity"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let n = count(report, "impure_hotspot");
        if n == 0 {
            return Vec::new();
        }
        vec![Suggestion::new(
            format!("Isolate side effects in {n} widely-used function(s)"),
            "Push I/O and mutation to the edges (functional core / imperative shell) so the \
             high-fan-in core becomes pure and easy to test.",
        )
        .with_targets([Target::Robustness, Target::Maintainability])
        .with_priority(cap(55 + n * 2, 80))]
    }
}

/// A suggestion tailored to the detected industry vertical (Manifesto §5).
pub struct VerticalAdvisor;
impl Advisor for VerticalAdvisor {
    fn name(&self) -> &'static str {
        "vertical"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let suggestion = match report.vertical {
            Vertical::Embedded => Suggestion::new(
                "Hold to no_std / stack discipline for embedded targets",
                "Absolute control of resources: avoid heap in interrupt paths, bound stack usage \
                 and keep peripherals behind typed HAL wrappers.",
            )
            .with_targets([Target::Performance, Target::Robustness])
            .with_priority(70),
            Vertical::GameEngine => Suggestion::new(
                "Keep per-frame hot paths data-oriented and allocation-free",
                "Real-time budgets (60+ FPS): favor SoA layouts, reuse buffers across frames and \
                 keep the update/render loop free of syscalls and allocations.",
            )
            .with_targets([Target::Performance, Target::Latency])
            .with_priority(70),
            Vertical::Backend => Suggestion::new(
                "Add bulkheads and backpressure to scale safely",
                "Modular-monolith backends scale best with bounded resource pools, timeouts and \
                 backpressure between services to contain failures.",
            )
            .with_targets([Target::Scalability, Target::Robustness])
            .with_priority(68),
            Vertical::FullStack => Suggestion::new(
                "Split components and memoize expensive view work",
                "Reactive UIs stay smooth when heavy computations are memoized and large \
                 components are split so re-renders touch less of the tree.",
            )
            .with_targets([Target::Performance, Target::Maintainability])
            .with_priority(60),
            Vertical::Unknown => return Vec::new(),
        };
        vec![suggestion]
    }
}

/// All advisors, in a deterministic order.
pub fn all_advisors() -> Vec<Box<dyn Advisor>> {
    vec![
        Box::new(ComplexityAdvisor),
        Box::new(CyclesAdvisor),
        Box::new(HazardsAdvisor),
        Box::new(AllocationAdvisor),
        Box::new(LayeringAdvisor),
        Box::new(PurityAdvisor),
        Box::new(VerticalAdvisor),
    ]
}
