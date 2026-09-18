//! Extended engineering lenses (Layer 2, opt-in).
//!
//! These are *graph-shape* lenses — cycle/recursion, dead code, god functions,
//! call hotspots and unstable dependencies — that read the call graph's
//! topology rather than a single function's body.
//!
//! ## Why they are gated behind their own config (and default to *disabled*)
//!
//! The built-in lenses read `lcw_config::Config`, which lives in a crate we do
//! not own here and is `#[serde(deny_unknown_fields)]`, so it cannot be
//! extended with new toggles from this crate. To stay strictly additive and
//! keep the *default* pipeline output byte-for-byte identical (golden snapshots
//! in other crates depend on it), the extended lenses read a dedicated
//! [`ExtLensConfig`] whose `Default` turns **everything off**. Nothing runs
//! unless a caller explicitly opts in (e.g. via the engine's `with_lens_stage`
//! hook or [`crate::analyze_extended`]). This mirrors the "config decides which
//! lenses run" pattern the built-ins use, just with a config this crate can own.

use std::collections::HashMap;

use lcw_config::Config;
use lcw_core::{CodeGraph, Diagnostic, Node, NodeId, NodeKind, Severity};
use petgraph::algo::tarjan_scc;

use crate::Lens;

/// Which extended lenses to run, and with what thresholds.
///
/// Every field is an `Option` that is `None` (disabled) by default; `Some(cfg)`
/// enables that lens with the given thresholds. This keeps the default
/// (`Default::default()`) fully inert so it never perturbs baseline output.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtLensConfig {
    /// Strongly-connected-component / self-recursion detection.
    pub recursion_cycles: Option<RecursionCycleLens>,
    /// Uncalled, non-entrypoint definitions (possible dead code).
    pub dead_code: Option<DeadCodeLens>,
    /// Very high fan-in *and* fan-out hubs (god functions).
    pub god_function: Option<GodFunctionLens>,
    /// Heavily-called nodes sitting on many call paths (hotspots).
    pub hotspot: Option<HotspotLens>,
    /// High fan-out into complex/volatile callees (unstable dependencies).
    pub unstable_dependency: Option<UnstableDependencyLens>,
}

impl ExtLensConfig {
    /// Enable every extended lens with its documented default thresholds.
    /// Handy for "give me everything" callers and tests.
    pub fn all() -> Self {
        ExtLensConfig {
            recursion_cycles: Some(RecursionCycleLens::default()),
            dead_code: Some(DeadCodeLens::default()),
            god_function: Some(GodFunctionLens::default()),
            hotspot: Some(HotspotLens::default()),
            unstable_dependency: Some(UnstableDependencyLens::default()),
        }
    }

    /// True when no extended lens is enabled (the default state).
    pub fn is_disabled(&self) -> bool {
        self.recursion_cycles.is_none()
            && self.dead_code.is_none()
            && self.god_function.is_none()
            && self.hotspot.is_none()
            && self.unstable_dependency.is_none()
    }
}

/// The set of extended lenses enabled by `cfg` (deterministic order). Returns
/// an empty vec for the default (disabled) config.
pub fn extended_lenses(cfg: &ExtLensConfig) -> Vec<Box<dyn Lens>> {
    let mut lenses: Vec<Box<dyn Lens>> = Vec::new();
    if let Some(l) = cfg.recursion_cycles {
        lenses.push(Box::new(l));
    }
    if let Some(l) = cfg.dead_code {
        lenses.push(Box::new(l));
    }
    if let Some(l) = cfg.god_function {
        lenses.push(Box::new(l));
    }
    if let Some(l) = cfg.hotspot {
        lenses.push(Box::new(l));
    }
    if let Some(l) = cfg.unstable_dependency {
        lenses.push(Box::new(l));
    }
    lenses
}

// --- recursion / cycles ----------------------------------------------------

/// Detects call cycles via strongly-connected components (mutual recursion or
/// larger dependency loops) plus direct self-recursion. Cyclic call structures
/// block modular reasoning and incremental compilation (Manifesto Principle I).
#[derive(Debug, Clone, Copy)]
pub struct RecursionCycleLens {
    /// Smallest SCC size treated as a cycle. `2` = mutual recursion and up.
    pub min_cycle_size: usize,
    /// SCCs at least this large escalate from `Medium` to `High`.
    pub large_cycle_size: usize,
    /// Also flag directly self-recursive functions (a 1-node cycle).
    pub report_self_recursion: bool,
}

impl Default for RecursionCycleLens {
    fn default() -> Self {
        RecursionCycleLens {
            min_cycle_size: 2,
            large_cycle_size: 4,
            report_self_recursion: true,
        }
    }
}

impl Lens for RecursionCycleLens {
    fn name(&self) -> &'static str {
        "recursion"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();

        // Multi-node cycles: strongly-connected components with >1 member.
        for component in tarjan_scc(graph.raw()) {
            let size = component.len();
            if size < self.min_cycle_size {
                continue;
            }
            let severity = if size >= self.large_cycle_size {
                Severity::High
            } else {
                Severity::Medium
            };
            for idx in component {
                let id = NodeId(idx.index() as u32);
                let node = graph.node(id);
                // External placeholders are call sinks; they can't own a cycle.
                if node.kind == NodeKind::External {
                    continue;
                }
                out.push(
                    Diagnostic::new(
                        "recursion",
                        "recursion_cycle",
                        severity,
                        format!(
                            "`{}` sits in a call cycle of {size} functions; cyclic dependencies block modular reasoning",
                            node.qualified_name
                        ),
                    )
                    .at(id),
                );
            }
        }

        // Direct self-recursion is a 1-node cycle `tarjan_scc` does not group.
        if self.report_self_recursion {
            for (from, to, _edge) in graph.edges() {
                if from != to {
                    continue;
                }
                let node = graph.node(from);
                if node.kind == NodeKind::External {
                    continue;
                }
                out.push(
                    Diagnostic::new(
                        "recursion",
                        "self_recursion",
                        Severity::Low,
                        format!(
                            "`{}` is directly self-recursive; ensure a base case and bounded depth",
                            node.qualified_name
                        ),
                    )
                    .at(from),
                );
            }
        }
        out
    }
}

// --- dead / unreachable code ----------------------------------------------

/// Flags defined functions/methods with no callers that are not entrypoints
/// (`main`, tests, or — unless opted in — the public API surface). Heuristic:
/// dynamic dispatch and reflection can call code the static graph misses, so
/// this is opt-in and reported at `Low` severity.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeadCodeLens {
    /// Treat `pub` items as reachable (they may be called from outside the
    /// crate). Off by default (`false`) to avoid flagging a library's public
    /// API surface as dead.
    pub include_public: bool,
}

impl Lens for DeadCodeLens {
    fn name(&self) -> &'static str {
        "dead_code"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for node in graph.nodes() {
            if !is_dead_code_candidate(node, self.include_public) {
                continue;
            }
            if graph.in_degree(node.id) == 0 {
                out.push(
                    Diagnostic::new(
                        "dead_code",
                        "dead_code",
                        Severity::Low,
                        format!(
                            "`{}` has no callers and is not an entrypoint (possible dead/unreachable code)",
                            node.qualified_name
                        ),
                    )
                    .at(node.id),
                );
            }
        }
        out
    }
}

/// Whether a node is eligible to be flagged as dead code.
fn is_dead_code_candidate(node: &Node, include_public: bool) -> bool {
    // Only real definitions of callable code; skip external targets/closures.
    if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }
    // `main` and test functions are legitimate roots with no in-graph callers.
    if node.flags.is_test || node.name == "main" {
        return false;
    }
    // Public items are reachable across the crate boundary unless asked.
    if node.flags.is_pub && !include_public {
        return false;
    }
    true
}

// --- god functions ---------------------------------------------------------

/// Flags "god functions": nodes with both very high fan-in *and* fan-out. They
/// are simultaneously depended upon by many callers and reach into many
/// callees, which concentrates responsibility and change risk in one place.
#[derive(Debug, Clone, Copy)]
pub struct GodFunctionLens {
    /// Minimum number of callers (fan-in) to qualify.
    pub fan_in: usize,
    /// Minimum number of distinct callees (fan-out) to qualify.
    pub fan_out: usize,
}

impl Default for GodFunctionLens {
    fn default() -> Self {
        GodFunctionLens {
            fan_in: 8,
            fan_out: 8,
        }
    }
}

impl Lens for GodFunctionLens {
    fn name(&self) -> &'static str {
        "god_function"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            let fan_in = graph.in_degree(node.id);
            let fan_out = graph.out_degree(node.id);
            if fan_in >= self.fan_in && fan_out >= self.fan_out {
                // Both dimensions well past the bar → the smell is acute.
                let severity = if fan_in >= self.fan_in.saturating_mul(2)
                    && fan_out >= self.fan_out.saturating_mul(2)
                {
                    Severity::High
                } else {
                    Severity::Medium
                };
                out.push(
                    Diagnostic::new(
                        "god_function",
                        "god_function",
                        severity,
                        format!(
                            "`{}` has fan-in {fan_in} and fan-out {fan_out}; it likely does too much (god function)",
                            node.qualified_name
                        ),
                    )
                    .at(node.id),
                );
            }
        }
        out
    }
}

// --- hotspots / centrality -------------------------------------------------

/// Flags call *hotspots*: nodes reached from many callers and via many call
/// sites (high incoming call volume). These sit on many execution paths, so
/// optimizing them pays off broadly (Manifesto section 4: clock-cycle latency).
#[derive(Debug, Clone, Copy)]
pub struct HotspotLens {
    /// Minimum distinct callers (fan-in) that marks a chokepoint.
    pub min_callers: usize,
    /// Minimum total incoming call sites (summed edge multiplicity).
    pub min_call_volume: u32,
}

impl Default for HotspotLens {
    fn default() -> Self {
        HotspotLens {
            min_callers: 6,
            min_call_volume: 16,
        }
    }
}

impl Lens for HotspotLens {
    fn name(&self) -> &'static str {
        "hotspot"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        // Incoming call *volume* (edges carry a merged multiplicity `count`).
        let mut volume: HashMap<NodeId, u32> = HashMap::new();
        for (_from, to, edge) in graph.edges() {
            let entry = volume.entry(to).or_insert(0);
            *entry = entry.saturating_add(edge.count);
        }

        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            let callers = graph.in_degree(node.id);
            let calls = volume.get(&node.id).copied().unwrap_or(0);
            // "Many paths" means at least a couple of callers; then either the
            // caller count or the raw call volume must clear the bar.
            if callers < 2 || (callers < self.min_callers && calls < self.min_call_volume) {
                continue;
            }
            let severity = if callers >= self.min_callers.saturating_mul(2)
                || calls >= self.min_call_volume.saturating_mul(2)
            {
                Severity::High
            } else {
                Severity::Medium
            };
            out.push(
                Diagnostic::new(
                    "hotspot",
                    "hotspot",
                    severity,
                    format!(
                        "`{}` is a call hotspot: {callers} callers across {calls} call sites, on many paths; optimize the hot path",
                        node.qualified_name
                    ),
                )
                .at(node.id),
            );
        }
        out
    }
}

// --- unstable dependencies -------------------------------------------------

/// Flags nodes that fan out heavily into *complex or volatile* callees. Lacking
/// VCS churn on the graph, volatility is approximated by callee cyclomatic
/// complexity and by callees that are themselves high fan-in hubs — both tend
/// to change often, so depending on many of them makes a function fragile
/// (Instability, adapted from Martin's metrics).
#[derive(Debug, Clone, Copy)]
pub struct UnstableDependencyLens {
    /// Minimum number of *internal* callees before the ratio is considered.
    pub min_fan_out: usize,
    /// Callee cyclomatic complexity at/above which it counts as volatile.
    pub complex_cc: u32,
    /// Fraction of volatile callees (0.0..=1.0) needed to flag the node.
    pub fraction: f64,
}

impl Default for UnstableDependencyLens {
    fn default() -> Self {
        UnstableDependencyLens {
            min_fan_out: 5,
            complex_cc: 10,
            fraction: 0.5,
        }
    }
}

impl Lens for UnstableDependencyLens {
    fn name(&self) -> &'static str {
        "unstable_dependency"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            // Only weigh resolved callees; external targets carry no metrics.
            let internal: Vec<NodeId> = graph
                .neighbors_out(node.id)
                .filter(|&c| graph.node(c).kind != NodeKind::External)
                .collect();
            if internal.len() < self.min_fan_out {
                continue;
            }
            let volatile = internal
                .iter()
                .filter(|&&c| {
                    let callee = graph.node(c);
                    callee.cyclomatic_complexity() > self.complex_cc
                        || graph.in_degree(c) >= self.min_fan_out.saturating_mul(2)
                })
                .count();
            let ratio = volatile as f64 / internal.len() as f64;
            if ratio >= self.fraction {
                let severity = if ratio >= 0.75 {
                    Severity::Medium
                } else {
                    Severity::Low
                };
                out.push(
                    Diagnostic::new(
                        "unstable_dependency",
                        "unstable_dependency",
                        severity,
                        format!(
                            "`{}` depends on {volatile}/{} complex or volatile callees; changes there will ripple (unstable dependency)",
                            node.qualified_name,
                            internal.len()
                        ),
                    )
                    .at(node.id),
                );
            }
        }
        out
    }
}

// --- helpers ---------------------------------------------------------------

/// A real definition (anything but an unresolved/external call target).
fn is_def(node: &Node) -> bool {
    node.kind != NodeKind::External
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeFlags, NodeKind, NodeStats, SourceSpan};

    fn add_def(g: &mut CodeGraph, qn: &str, kind: NodeKind) -> NodeId {
        add_def_full(g, qn, kind, NodeFlags::default(), NodeStats::default())
    }

    fn add_def_full(
        g: &mut CodeGraph,
        qn: &str,
        kind: NodeKind,
        flags: NodeFlags,
        stats: NodeStats,
    ) -> NodeId {
        let name = qn.rsplit("::").next().unwrap_or(qn).to_string();
        let module_path = qn
            .rsplit_once("::")
            .map(|(m, _)| m.to_string())
            .unwrap_or_default();
        g.add_node(Node {
            id: NodeId(0),
            name,
            qualified_name: qn.to_string(),
            module_path,
            kind,
            span: SourceSpan::default(),
            flags,
            stats,
        })
    }

    fn call(g: &mut CodeGraph, from: NodeId, to: NodeId) {
        g.add_edge(
            from,
            to,
            Edge::new(EdgeKind::DirectCall, SourceSpan::default()),
        );
    }

    fn codes<'a>(diags: &'a [Diagnostic], code: &str) -> Vec<&'a Diagnostic> {
        diags.iter().filter(|d| d.code == code).collect()
    }

    // --- recursion / cycles ---

    #[test]
    fn recursion_lens_flags_mutual_recursion_and_self_recursion() {
        let mut g = CodeGraph::new();
        let a = add_def(&mut g, "crate::a", NodeKind::Function);
        let b = add_def(&mut g, "crate::b", NodeKind::Function);
        call(&mut g, a, b);
        call(&mut g, b, a); // a <-> b mutual recursion (SCC of size 2)
        let s = add_def(&mut g, "crate::selfrec", NodeKind::Function);
        call(&mut g, s, s); // direct self-recursion (1-node cycle)

        let diags = RecursionCycleLens::default().evaluate(&g, &Config::default());

        let cycle = codes(&diags, "recursion_cycle");
        assert_eq!(cycle.len(), 2, "both members of the a<->b cycle flagged");
        assert!(cycle.iter().all(|d| d.severity == Severity::Medium));
        let selfrec = codes(&diags, "self_recursion");
        assert_eq!(selfrec.len(), 1);
        assert_eq!(selfrec[0].severity, Severity::Low);
        assert_eq!(selfrec[0].node, Some(s));
    }

    #[test]
    fn recursion_lens_escalates_large_cycles_and_ignores_acyclic() {
        // 4-node cycle a->b->c->d->a hits the `large_cycle_size` bar → High.
        let mut g = CodeGraph::new();
        let ids: Vec<NodeId> = (0..4)
            .map(|i| add_def(&mut g, &format!("crate::n{i}"), NodeKind::Function))
            .collect();
        for i in 0..4 {
            call(&mut g, ids[i], ids[(i + 1) % 4]);
        }
        let diags = RecursionCycleLens::default().evaluate(&g, &Config::default());
        let cycle = codes(&diags, "recursion_cycle");
        assert_eq!(cycle.len(), 4);
        assert!(cycle.iter().all(|d| d.severity == Severity::High));

        // A straight chain a->b->c is acyclic → nothing.
        let mut g2 = CodeGraph::new();
        let a = add_def(&mut g2, "crate::a", NodeKind::Function);
        let b = add_def(&mut g2, "crate::b", NodeKind::Function);
        let c = add_def(&mut g2, "crate::c", NodeKind::Function);
        call(&mut g2, a, b);
        call(&mut g2, b, c);
        assert!(RecursionCycleLens::default()
            .evaluate(&g2, &Config::default())
            .is_empty());
    }

    // --- dead code ---

    #[test]
    fn dead_code_lens_flags_only_uncalled_non_entrypoints() {
        let mut g = CodeGraph::new();
        // Uncalled private fn → dead.
        let dead = add_def(&mut g, "crate::dead", NodeKind::Function);
        // `main` is an entrypoint even with no callers.
        add_def(&mut g, "crate::main", NodeKind::Function);
        // A test function is a root too.
        add_def_full(
            &mut g,
            "crate::it_works",
            NodeKind::Function,
            NodeFlags {
                is_test: true,
                ..Default::default()
            },
            NodeStats::default(),
        );
        // A public fn is reachable across the crate boundary by default.
        add_def_full(
            &mut g,
            "crate::api",
            NodeKind::Function,
            NodeFlags {
                is_pub: true,
                ..Default::default()
            },
            NodeStats::default(),
        );
        // A called private fn is alive.
        let caller = add_def(&mut g, "crate::caller", NodeKind::Function);
        let callee = add_def(&mut g, "crate::callee", NodeKind::Function);
        call(&mut g, caller, callee);
        // ...but `caller` itself has no callers, so it is also dead here.

        let diags = DeadCodeLens::default().evaluate(&g, &Config::default());
        let flagged: Vec<_> = codes(&diags, "dead_code")
            .iter()
            .map(|d| d.node.unwrap())
            .collect();
        assert!(flagged.contains(&dead));
        assert!(flagged.contains(&caller));
        assert!(!flagged.contains(&callee));
        assert!(diags.iter().all(|d| d.severity == Severity::Low));
        // main, test and pub are excluded.
        assert_eq!(codes(&diags, "dead_code").len(), 2);

        // Opting in to public items flags the unused `pub` API too.
        let with_pub = DeadCodeLens {
            include_public: true,
        }
        .evaluate(&g, &Config::default());
        assert_eq!(codes(&with_pub, "dead_code").len(), 3);
    }

    // --- god functions ---

    #[test]
    fn god_function_lens_needs_both_high_fan_in_and_fan_out() {
        let mut g = CodeGraph::new();
        let hub = add_def(&mut g, "crate::hub", NodeKind::Function);
        for i in 0..3 {
            let caller = add_def(&mut g, &format!("crate::in{i}"), NodeKind::Function);
            call(&mut g, caller, hub);
            let callee = add_def(&mut g, &format!("crate::out{i}"), NodeKind::Function);
            call(&mut g, hub, callee);
        }
        // fan_in == fan_out == 3.
        let lens = GodFunctionLens {
            fan_in: 3,
            fan_out: 3,
        };
        let diags = lens.evaluate(&g, &Config::default());
        let god = codes(&diags, "god_function");
        assert_eq!(god.len(), 1);
        assert_eq!(god[0].node, Some(hub));
        assert_eq!(god[0].severity, Severity::Medium);

        // High fan-in but no fan-out is not a god function.
        let strict = GodFunctionLens {
            fan_in: 3,
            fan_out: 8,
        };
        assert!(codes(&strict.evaluate(&g, &Config::default()), "god_function").is_empty());
    }

    // --- hotspots ---

    #[test]
    fn hotspot_lens_uses_callers_or_call_volume() {
        // Caller-count path: 3 distinct callers, low volume.
        let mut g = CodeGraph::new();
        let target = add_def(&mut g, "crate::target", NodeKind::Function);
        for i in 0..3 {
            let c = add_def(&mut g, &format!("crate::c{i}"), NodeKind::Function);
            call(&mut g, c, target);
        }
        let by_callers = HotspotLens {
            min_callers: 3,
            min_call_volume: 1000,
        }
        .evaluate(&g, &Config::default());
        assert_eq!(codes(&by_callers, "hotspot").len(), 1);

        // Volume path: only 2 callers, but one calls it many times.
        let mut g2 = CodeGraph::new();
        let t = add_def(&mut g2, "crate::t", NodeKind::Function);
        let hot = add_def(&mut g2, "crate::hot", NodeKind::Function);
        let cold = add_def(&mut g2, "crate::cold", NodeKind::Function);
        for _ in 0..60 {
            call(&mut g2, hot, t); // merges into one edge with count == 60
        }
        call(&mut g2, cold, t);
        let by_volume = HotspotLens {
            min_callers: 5,
            min_call_volume: 50,
        }
        .evaluate(&g2, &Config::default());
        let hs = codes(&by_volume, "hotspot");
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].node, Some(t));

        // A single-caller target never counts as "on many paths".
        let mut g3 = CodeGraph::new();
        let only = add_def(&mut g3, "crate::only", NodeKind::Function);
        let one = add_def(&mut g3, "crate::one", NodeKind::Function);
        for _ in 0..99 {
            call(&mut g3, one, only);
        }
        assert!(codes(
            &HotspotLens {
                min_callers: 2,
                min_call_volume: 1,
            }
            .evaluate(&g3, &Config::default()),
            "hotspot"
        )
        .is_empty());
    }

    // --- unstable dependencies ---

    #[test]
    fn unstable_dependency_lens_flags_high_volatile_fan_out() {
        let mut g = CodeGraph::new();
        let src = add_def(&mut g, "crate::src", NodeKind::Function);
        // Three complex callees (cc = 13) and one trivial one → 3/4 volatile.
        for i in 0..3 {
            let cx = add_def_full(
                &mut g,
                &format!("crate::cx{i}"),
                NodeKind::Function,
                NodeFlags::default(),
                NodeStats {
                    decision_points: 12,
                    ..Default::default()
                },
            );
            call(&mut g, src, cx);
        }
        let simple = add_def(&mut g, "crate::simple", NodeKind::Function);
        call(&mut g, src, simple);

        let lens = UnstableDependencyLens {
            min_fan_out: 3,
            complex_cc: 10,
            fraction: 0.5,
        };
        let diags = lens.evaluate(&g, &Config::default());
        let unstable = codes(&diags, "unstable_dependency");
        assert_eq!(unstable.len(), 1);
        assert_eq!(unstable[0].node, Some(src));
        assert_eq!(unstable[0].severity, Severity::Medium); // 0.75 >= 0.75

        // A node whose callees are all simple is stable.
        let mut g2 = CodeGraph::new();
        let s2 = add_def(&mut g2, "crate::s2", NodeKind::Function);
        for i in 0..4 {
            let leaf = add_def(&mut g2, &format!("crate::leaf{i}"), NodeKind::Function);
            call(&mut g2, s2, leaf);
        }
        assert!(codes(
            &lens.evaluate(&g2, &Config::default()),
            "unstable_dependency"
        )
        .is_empty());
    }

    // --- gating / orchestration ---

    #[test]
    fn default_config_is_disabled_and_yields_no_lenses() {
        let cfg = ExtLensConfig::default();
        assert!(cfg.is_disabled());
        assert!(extended_lenses(&cfg).is_empty());
    }

    #[test]
    fn all_config_enables_every_extended_lens() {
        let cfg = ExtLensConfig::all();
        assert!(!cfg.is_disabled());
        assert_eq!(extended_lenses(&cfg).len(), 5);
    }

    #[test]
    fn extended_lenses_respects_individual_toggles() {
        let cfg = ExtLensConfig {
            god_function: Some(GodFunctionLens::default()),
            ..Default::default()
        };
        let lenses = extended_lenses(&cfg);
        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].name(), "god_function");
    }

    #[test]
    fn analyze_extended_is_noop_when_disabled_and_appends_when_enabled() {
        use lcw_core::AnalysisReport;

        // Build a graph with a cycle so at least one extended lens fires.
        let mut g = CodeGraph::new();
        let a = add_def(&mut g, "crate::a", NodeKind::Function);
        let b = add_def(&mut g, "crate::b", NodeKind::Function);
        call(&mut g, a, b);
        call(&mut g, b, a);

        let mut report = AnalysisReport::new(g);
        crate::analyze_extended(&Config::default(), &ExtLensConfig::default(), &mut report);
        assert!(
            report.diagnostics.is_empty(),
            "disabled config must not change output"
        );

        crate::analyze_extended(&Config::default(), &ExtLensConfig::all(), &mut report);
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.code == "recursion_cycle"));
        // Diagnostics stay sorted highest-severity first.
        for pair in report.diagnostics.windows(2) {
            assert!(pair[0].severity >= pair[1].severity);
        }
    }
}
