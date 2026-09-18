//! Quality metrics (Manifesto section 4). Metrics are computed by Layer 2 and
//! attached either to a specific node or to the project as a whole.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// A named, quantitative measurement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MetricKind {
    /// Decision points + 1, per function.
    CyclomaticComplexity,
    /// Number of callers.
    FanIn,
    /// Number of distinct callees.
    FanOut,
    LinesOfCode,
    MaxNesting,
    Parameters,
    /// 0.0..=1.0 estimate that a function is pure (no side effects).
    PurityScore,
    /// Heuristic heap-allocation count (Manifesto: memory footprint).
    HeapAllocations,
    UnsafeBlocks,
    /// Project-level: fraction of functions in a dependency cycle.
    Cyclicity,
}

impl MetricKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MetricKind::CyclomaticComplexity => "cyclomatic_complexity",
            MetricKind::FanIn => "fan_in",
            MetricKind::FanOut => "fan_out",
            MetricKind::LinesOfCode => "lines_of_code",
            MetricKind::MaxNesting => "max_nesting",
            MetricKind::Parameters => "parameters",
            MetricKind::PurityScore => "purity_score",
            MetricKind::HeapAllocations => "heap_allocations",
            MetricKind::UnsafeBlocks => "unsafe_blocks",
            MetricKind::Cyclicity => "cyclicity",
        }
    }
}

impl std::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A measured value, optionally scoped to a single node.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub kind: MetricKind,
    pub value: f64,
    /// `None` means the metric is project-wide.
    pub node: Option<NodeId>,
}

impl Metric {
    pub fn node(kind: MetricKind, node: NodeId, value: f64) -> Self {
        Metric {
            kind,
            value,
            node: Some(node),
        }
    }

    pub fn project(kind: MetricKind, value: f64) -> Self {
        Metric {
            kind,
            value,
            node: None,
        }
    }
}
