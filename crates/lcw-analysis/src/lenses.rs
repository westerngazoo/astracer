//! Engineering lenses (Layer 2). Each [`Lens`] reads the graph + config and
//! emits diagnostics; they are intentionally independent so the UI can toggle
//! them as overlays/filters and the blast radius of any one stays contained
//! (Principle I).

use lcw_config::Config;
use lcw_core::{CodeGraph, Diagnostic, Node, NodeKind, Severity};

use crate::metrics::purity_score;
use crate::Lens;

/// Cyclomatic complexity, parameter count, length and nesting thresholds.
pub struct ComplexityLens;

impl Lens for ComplexityLens {
    fn name(&self) -> &'static str {
        "complexity"
    }

    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic> {
        let m = &config.metrics;
        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            let cc = node.cyclomatic_complexity();
            if cc > m.cyclomatic_max {
                let severity = if cc >= m.cyclomatic_max.saturating_mul(2) {
                    Severity::High
                } else {
                    Severity::Medium
                };
                out.push(
                    Diagnostic::new(
                        "complexity",
                        "high_complexity",
                        severity,
                        format!(
                            "`{}` has cyclomatic complexity {cc} (max {})",
                            node.qualified_name, m.cyclomatic_max
                        ),
                    )
                    .at(node.id),
                );
            }
            if node.stats.parameters > m.max_parameters {
                out.push(
                    Diagnostic::new(
                        "complexity",
                        "too_many_parameters",
                        Severity::Low,
                        format!(
                            "`{}` takes {} parameters (max {})",
                            node.qualified_name, node.stats.parameters, m.max_parameters
                        ),
                    )
                    .at(node.id),
                );
            }
            if node.stats.lines_of_code > m.max_function_loc {
                out.push(
                    Diagnostic::new(
                        "complexity",
                        "long_function",
                        Severity::Low,
                        format!(
                            "`{}` is {} lines long (max {})",
                            node.qualified_name, node.stats.lines_of_code, m.max_function_loc
                        ),
                    )
                    .at(node.id),
                );
            }
            if node.stats.max_nesting > m.max_nesting {
                out.push(
                    Diagnostic::new(
                        "complexity",
                        "deep_nesting",
                        Severity::Low,
                        format!(
                            "`{}` nests {} levels deep (max {})",
                            node.qualified_name, node.stats.max_nesting, m.max_nesting
                        ),
                    )
                    .at(node.id),
                );
            }
        }
        out
    }
}

/// Memory & thread-safety hazards: `unsafe` usage and heap-allocation pressure.
pub struct HazardsLens;

impl Lens for HazardsLens {
    fn name(&self) -> &'static str {
        "hazards"
    }

    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            if node.stats.unsafe_blocks > 0 || node.flags.is_unsafe {
                let blocks = node.stats.unsafe_blocks.max(node.flags.is_unsafe as u32);
                out.push(
                    Diagnostic::new(
                        "hazards",
                        "unsafe_usage",
                        Severity::Medium,
                        format!(
                            "`{}` uses `unsafe` ({blocks} block(s)); review memory & thread safety",
                            node.qualified_name
                        ),
                    )
                    .at(node.id),
                );
            }

            let allocations = node.stats.allocations as f64 * config.metrics.heap_sensitivity;
            if allocations >= 8.0 {
                let severity = if allocations >= 20.0 {
                    Severity::Medium
                } else {
                    Severity::Low
                };
                out.push(
                    Diagnostic::new(
                        "hazards",
                        "heap_pressure",
                        severity,
                        format!(
                            "`{}` performs ~{allocations:.0} heap allocations (memory pressure)",
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

/// Flags side-effect-heavy functions that are nonetheless widely depended upon
/// (a maintainability smell: hard-to-test code in the fan-in core).
pub struct PurityLens;

impl Lens for PurityLens {
    fn name(&self) -> &'static str {
        "purity"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for node in graph.nodes().filter(|n| is_def(n)) {
            let purity = purity_score(node);
            let fan_in = graph.in_degree(node.id);
            if purity < 0.4 && fan_in >= 3 {
                out.push(
                    Diagnostic::new(
                        "purity",
                        "impure_hotspot",
                        Severity::Low,
                        format!(
                            "`{}` is side-effect heavy (purity {purity:.2}) yet has {fan_in} callers; consider isolating effects",
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

/// Architecture layering checks for Clean/Onion (concentric layers) and MVC
/// (model must not depend on view/controller). Layers are inferred from module
/// path segments — a heuristic that improves with the semantic backend.
pub struct LayeringLens;

impl Lens for LayeringLens {
    fn name(&self) -> &'static str {
        "layering"
    }

    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic> {
        let styles = &config.lenses.architecture;
        let check_layered = styles.iter().any(|s| s == "clean" || s == "onion");
        let check_mvc = styles.iter().any(|s| s == "mvc");
        let layered_style = if styles.iter().any(|s| s == "clean") {
            "clean"
        } else {
            "onion"
        };

        let mut out = Vec::new();
        for (from, to, _edge) in graph.edges() {
            if from == to {
                continue;
            }
            let src = graph.node(from);
            let dst = graph.node(to);
            if src.kind == NodeKind::External || dst.kind == NodeKind::External {
                continue;
            }

            if check_layered {
                if let (Some(sl), Some(dl)) = (
                    concentric_layer(&src.module_path),
                    concentric_layer(&dst.module_path),
                ) {
                    // Inner (lower index) must not depend on outer (higher index).
                    if sl.index < dl.index {
                        out.push(
                            Diagnostic::new(
                                "layering",
                                "layering_violation",
                                Severity::Medium,
                                format!(
                                    "{layered_style}: `{}` ({}) depends on `{}` ({}) — inner layer reaching outward",
                                    src.qualified_name, sl.name, dst.qualified_name, dl.name
                                ),
                            )
                            .at(from),
                        );
                    }
                }
            }

            if check_mvc {
                if let (Some(Mvc::Model), Some(outer)) =
                    (mvc_layer(&src.module_path), mvc_layer(&dst.module_path))
                {
                    if matches!(outer, Mvc::View | Mvc::Controller) {
                        out.push(
                            Diagnostic::new(
                                "layering",
                                "mvc_violation",
                                Severity::Medium,
                                format!(
                                    "mvc: model `{}` depends on {} `{}`",
                                    src.qualified_name,
                                    outer.as_str(),
                                    dst.qualified_name
                                ),
                            )
                            .at(from),
                        );
                    }
                }
            }
        }
        out
    }
}

/// Characterizes the codebase paradigm (OOP vs FP vs mixed) from the ratio of
/// methods to free functions and the mean purity. Emits one project-level note.
pub struct ParadigmLens;

impl Lens for ParadigmLens {
    fn name(&self) -> &'static str {
        "paradigm"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut methods = 0usize;
        let mut functions = 0usize;
        let mut purity_sum = 0.0f64;
        let mut defs = 0usize;

        for node in graph.nodes().filter(|n| is_def(n)) {
            defs += 1;
            purity_sum += purity_score(node);
            if node.kind == NodeKind::Method || node.flags.is_method {
                methods += 1;
            } else if node.kind == NodeKind::Function {
                functions += 1;
            }
        }

        if defs == 0 {
            return Vec::new();
        }
        let classified = (methods + functions).max(1);
        let method_ratio = methods as f64 / classified as f64;
        let mean_purity = purity_sum / defs as f64;

        let label = if method_ratio >= 0.6 {
            "object-oriented"
        } else if method_ratio <= 0.35 && mean_purity >= 0.6 {
            "functional"
        } else {
            "mixed / procedural"
        };

        vec![Diagnostic::new(
            "paradigm",
            "paradigm_profile",
            Severity::Info,
            format!(
                "codebase paradigm: {label} ({:.0}% methods, mean purity {mean_purity:.2})",
                method_ratio * 100.0
            ),
        )]
    }
}

// --- helpers ---------------------------------------------------------------

fn is_def(node: &Node) -> bool {
    node.kind != NodeKind::External
}

/// A concentric (Clean/Onion) architecture layer.
struct Layer {
    index: u8,
    name: &'static str,
}

/// Classify a module path into a Clean/Onion layer (0 = innermost domain).
/// The *last* matching segment wins (closest to the item's own module).
fn concentric_layer(module_path: &str) -> Option<Layer> {
    let mut found: Option<Layer> = None;
    for seg in module_path.split("::") {
        let idx = match seg {
            "domain" | "entities" | "entity" | "model" | "models" | "core" => Some(0u8),
            "application" | "app" | "usecase" | "use_case" | "usecases" | "service"
            | "services" | "logic" => Some(1),
            "interface" | "interfaces" | "adapter" | "adapters" | "api" | "presentation"
            | "handler" | "handlers" | "controller" | "controllers" => Some(2),
            "infrastructure" | "infra" | "framework" | "frameworks" | "db" | "database"
            | "persistence" | "repository" | "repositories" | "repo" | "io" | "net" | "http"
            | "web" | "fs" => Some(3),
            _ => None,
        };
        if let Some(index) = idx {
            found = Some(Layer {
                index,
                name: LAYER_NAMES[index as usize],
            });
        }
    }
    found
}

const LAYER_NAMES: [&str; 4] = ["domain", "application", "interface", "infrastructure"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mvc {
    Model,
    View,
    Controller,
}

impl Mvc {
    fn as_str(self) -> &'static str {
        match self {
            Mvc::Model => "model",
            Mvc::View => "view",
            Mvc::Controller => "controller",
        }
    }
}

fn mvc_layer(module_path: &str) -> Option<Mvc> {
    let mut found = None;
    for seg in module_path.split("::") {
        let layer = match seg {
            "model" | "models" | "entity" | "entities" | "domain" => Some(Mvc::Model),
            "view" | "views" | "template" | "templates" | "component" | "components" => {
                Some(Mvc::View)
            }
            "controller" | "controllers" | "handler" | "handlers" | "route" | "routes" => {
                Some(Mvc::Controller)
            }
            _ => None,
        };
        if layer.is_some() {
            found = layer;
        }
    }
    found
}
