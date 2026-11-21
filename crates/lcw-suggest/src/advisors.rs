//! Concrete advisors (Layer 3 rules). Each turns Layer 2 findings + the
//! detected vertical into actionable [`Suggestion`]s tagged with the
//! optimization [`Target`]s they serve. Priorities here are *base* weights; the
//! orchestrator rescales them by the config's per-target priorities.

use lcw_core::{AnalysisReport, MetricKind, NodeId, Suggestion, Target, Vertical};

use crate::Advisor;

/// Per-node suggestions cap so the UI stays scannable.
const PER_NODE_CAP: usize = 8;

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

/// One suggestion per attached diagnostic (up to [`PER_NODE_CAP`]), using the
/// diagnostic message as the title and `fix` as the recommended action.
fn per_node_suggestions(
    report: &AnalysisReport,
    code: &str,
    fix: &str,
    targets: &[Target],
    priority: u8,
) -> Vec<Suggestion> {
    report
        .diagnostics
        .iter()
        .filter(|d| d.code == code && d.node.is_some())
        .take(PER_NODE_CAP)
        .map(|d| {
            Suggestion::new(d.message.clone(), fix)
                .at(d.node.unwrap_or(NodeId(0)))
                .with_targets(targets.iter().copied())
                .with_codes([code])
                .with_priority(priority)
        })
        .collect()
}

fn overflow_summary(
    total: usize,
    code: &str,
    label: &str,
    fix: &str,
    targets: &[Target],
    priority: u8,
) -> Option<Suggestion> {
    if total <= PER_NODE_CAP {
        return None;
    }
    Some(
        Suggestion::new(
            format!("… and {} more {label}", total.saturating_sub(PER_NODE_CAP)),
            fix,
        )
        .with_targets(targets.iter().copied())
        .with_codes([code])
        .with_priority(priority.saturating_sub(5)),
    )
}

/// Refactor high-complexity functions.
pub struct ComplexityAdvisor;
impl Advisor for ComplexityAdvisor {
    fn name(&self) -> &'static str {
        "complexity"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let code = "high_complexity";
        let n = count(report, code);
        if n == 0 {
            return Vec::new();
        }
        const FIX: &str = "Extract helpers, flatten branching, and replace nested conditionals \
                           with early returns.";
        let targets = [Target::Maintainability, Target::Robustness];
        let priority = cap(60 + n * 3, 90);
        let mut out = per_node_suggestions(report, code, FIX, &targets, priority);
        if let Some(more) = overflow_summary(
            n,
            code,
            "high-complexity function(s)",
            FIX,
            &targets,
            priority,
        ) {
            out.push(more);
        }
        out
    }
}

/// Break dependency cycles, starting with the worst one the cycles lens found.
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
        let worst = report
            .diagnostics
            .iter()
            .filter(|d| d.code == "cycle" && d.node.is_some())
            .max_by_key(|d| (d.severity, d.evidence("scopes"), d.evidence("size")));
        const WHY: &str = "Cyclic call structures block modular reasoning and incremental \
                           compilation; introduce trait boundaries / dependency inversion to cut \
                           the cycles.";
        let Some(worst) = worst else {
            return vec![Suggestion::new(
                format!("Break dependency cycles ({pct:.0}% of functions are entangled)"),
                format!("{WHY} Enable the cycles lens to list the participating functions."),
            )
            .with_targets([Target::Maintainability, Target::Scalability])
            .with_codes(["cycle"])
            .with_priority(cap(60 + pct as usize, 90))];
        };
        let cycles = count(report, "cycle");
        let plain_recursion = worst.severity <= lcw_core::Severity::Low;
        let (title, priority) = if plain_recursion {
            (
                format!(
                    "Only plain recursion found ({cycles} recursive group(s)); worst: {}",
                    worst.message
                ),
                cap(30 + cycles, 45),
            )
        } else {
            (
                format!(
                    "Break dependency cycles ({pct:.0}% entangled); worst: {}",
                    worst.message
                ),
                cap(60 + pct as usize, 90),
            )
        };
        vec![Suggestion::new(title, WHY)
            .at(worst.node.unwrap_or(NodeId(0)))
            .with_targets([Target::Maintainability, Target::Scalability])
            .with_codes(["cycle"])
            .with_priority(priority)]
    }
}

/// Move blocking work off the async executor.
pub struct AsyncBlockingAdvisor;
impl Advisor for AsyncBlockingAdvisor {
    fn name(&self) -> &'static str {
        "async_blocking"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let code = "async_blocking";
        let n = count(report, code);
        if n == 0 {
            return Vec::new();
        }
        const FIX: &str = "Synchronous I/O on an async executor thread stalls every task \
                           scheduled on it: wrap the blocking section in \
                           `tokio::task::spawn_blocking` (or `block_in_place`), or switch to an \
                           async API.";
        let targets = [Target::Latency, Target::Scalability];
        let priority = cap(70 + n, 95);
        let mut out = per_node_suggestions(report, code, FIX, &targets, priority);
        if let Some(more) = overflow_summary(
            n,
            code,
            "async fn(s) blocking the executor",
            FIX,
            &targets,
            priority,
        ) {
            out.push(more);
        }
        out
    }
}

/// Hoist I/O and heavy clones out of loops.
pub struct LoopAdvisor;
impl Advisor for LoopAdvisor {
    fn name(&self) -> &'static str {
        "loops"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let mut out = Vec::new();
        for (code, label, fix, base) in [
            (
                "n_plus_one",
                "per-iteration I/O call(s)",
                "One I/O round trip per element (N+1): fetch the batch in one call \
                 (a range scan or multi-get) before the loop.",
                65,
            ),
            (
                "clone_in_loop",
                "per-iteration clone(s)",
                "Cloning a collection or large value every iteration multiplies \
                 allocations: borrow it, clone once before the loop, or share an `Arc`.",
                50,
            ),
        ] {
            let n = count(report, code);
            if n == 0 {
                continue;
            }
            let targets = [Target::Performance, Target::Latency];
            let priority = cap(base + n * 2, 88);
            out.extend(per_node_suggestions(report, code, fix, &targets, priority));
            out.extend(overflow_summary(n, code, label, fix, &targets, priority));
        }
        out
    }
}

/// Encapsulate `unsafe`.
pub struct HazardsAdvisor;
impl Advisor for HazardsAdvisor {
    fn name(&self) -> &'static str {
        "hazards"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let code = "unsafe_usage";
        let n = count(report, code);
        if n == 0 {
            return Vec::new();
        }
        const FIX: &str = "Confine `unsafe` behind safe abstractions with documented invariants, \
                           and cover them with tests or Miri.";
        let targets = [Target::Robustness];
        let priority = cap(65 + n * 2, 88);
        let mut out = per_node_suggestions(report, code, FIX, &targets, priority);
        if let Some(more) =
            overflow_summary(n, code, "`unsafe` function(s)", FIX, &targets, priority)
        {
            out.push(more);
        }
        out
    }
}

/// Reduce heap-allocation pressure.
pub struct AllocationAdvisor;
impl Advisor for AllocationAdvisor {
    fn name(&self) -> &'static str {
        "allocations"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let code = "heap_pressure";
        let n = count(report, code);
        if n == 0 {
            return Vec::new();
        }
        const FIX: &str = "Prefer borrowing, pre-sized buffers, and arena/stack reuse over \
                           per-call allocation to cut allocator pressure and tail latency.";
        let targets = [Target::Performance, Target::Latency];
        let priority = cap(60 + n * 2, 85);
        let mut out = per_node_suggestions(report, code, FIX, &targets, priority);
        if let Some(more) = overflow_summary(
            n,
            code,
            "allocation-heavy function(s)",
            FIX,
            &targets,
            priority,
        ) {
            out.push(more);
        }
        out
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
        const FIX: &str = "Inner layers should not depend on outer ones; invert the offending \
                           dependencies (ports/adapters) so the domain stays framework-agnostic.";
        let targets = [Target::Maintainability, Target::Scalability];
        let priority = cap(62 + n * 2, 88);
        let mut out = Vec::new();
        for code in ["layering_violation", "mvc_violation"] {
            out.extend(per_node_suggestions(report, code, FIX, &targets, priority));
        }
        out.truncate(PER_NODE_CAP);
        if n > PER_NODE_CAP {
            out.push(
                Suggestion::new(
                    format!(
                        "… and {} more layering violation(s)",
                        n.saturating_sub(PER_NODE_CAP)
                    ),
                    FIX,
                )
                .with_targets(targets)
                .with_codes(["layering_violation", "mvc_violation"])
                .with_priority(priority.saturating_sub(5)),
            );
        }
        out
    }
}

/// Isolate side effects in widely-used impure functions.
pub struct PurityAdvisor;
impl Advisor for PurityAdvisor {
    fn name(&self) -> &'static str {
        "purity"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let code = "impure_hotspot";
        let n = count(report, code);
        if n == 0 {
            return Vec::new();
        }
        const FIX: &str =
            "Push I/O and mutation to the edges (functional core / imperative shell) \
                           so the high-fan-in core becomes pure and easy to test.";
        let targets = [Target::Robustness, Target::Maintainability];
        let priority = cap(55 + n * 2, 80);
        let mut out = per_node_suggestions(report, code, FIX, &targets, priority);
        if let Some(more) = overflow_summary(n, code, "impure hotspot(s)", FIX, &targets, priority)
        {
            out.push(more);
        }
        out
    }
}

/// A suggestion tailored to the detected industry vertical (Manifesto §5).
pub struct VerticalAdvisor;
impl Advisor for VerticalAdvisor {
    fn name(&self) -> &'static str {
        "vertical"
    }
    fn advise(&self, report: &AnalysisReport, _config: &lcw_config::Config) -> Vec<Suggestion> {
        let anchors: &[&str] = match report.vertical {
            Vertical::Embedded => &["heap_pressure", "unsafe_usage"],
            Vertical::GameEngine => &["clone_in_loop", "heap_pressure", "n_plus_one"],
            Vertical::Backend => &["async_blocking", "n_plus_one", "impure_hotspot"],
            Vertical::FullStack => &["high_complexity", "heap_pressure"],
            Vertical::Unknown => &[],
        };
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
        let anchor = anchors.iter().find_map(|code| {
            report
                .diagnostics
                .iter()
                .filter(|d| d.code == *code && d.node.is_some())
                .max_by_key(|d| d.severity)
        });
        let suggestion = match anchor {
            Some(d) => Suggestion {
                rationale: format!("{} Start here: {}", suggestion.rationale, d.message),
                ..suggestion
            }
            .at(d.node.unwrap_or(NodeId(0))),
            None => suggestion,
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
        Box::new(AsyncBlockingAdvisor),
        Box::new(LoopAdvisor),
        Box::new(VerticalAdvisor),
    ]
}
