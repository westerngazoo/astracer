//! Diagnostics emitted by engineering lenses (Layer 2).

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Severity of a finding. Ordered so `>=` comparisons work as expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
        };
        f.write_str(s)
    }
}

/// A single finding produced by a lens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable machine code, e.g. `high_complexity`.
    pub code: String,
    /// Which lens produced it, e.g. `complexity` or `layering`.
    pub lens: String,
    pub severity: Severity,
    pub message: String,
    /// Node the finding is attached to, if any.
    pub node: Option<NodeId>,
}

impl Diagnostic {
    pub fn new(
        lens: impl Into<String>,
        code: impl Into<String>,
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Diagnostic {
            code: code.into(),
            lens: lens.into(),
            severity,
            message: message.into(),
            node: None,
        }
    }

    pub fn at(mut self, node: NodeId) -> Self {
        self.node = Some(node);
        self
    }
}
