//! Diagnostics emitted by engineering lenses (Layer 2).

use serde::{Deserialize, Serialize};

use crate::graph::SourceSpan;
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
    /// The exact place the finding is about when it is narrower than the
    /// node's definition: the offending call site, the loop body call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SourceSpan>,
    /// Other nodes involved: the call chain to a blocking call, the members
    /// of a cycle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<NodeId>,
    /// Named measurements backing the finding (`depth`, `members`, ...), in
    /// the order they were added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<(String, u64)>,
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
            site: None,
            related: Vec::new(),
            evidence: Vec::new(),
        }
    }

    pub fn at(mut self, node: NodeId) -> Self {
        self.node = Some(node);
        self
    }

    pub fn with_site(mut self, site: SourceSpan) -> Self {
        self.site = Some(site);
        self
    }

    pub fn with_related(mut self, related: impl IntoIterator<Item = NodeId>) -> Self {
        self.related = related.into_iter().collect();
        self
    }

    pub fn with_evidence(mut self, key: impl Into<String>, value: u64) -> Self {
        self.evidence.push((key.into(), value));
        self
    }

    /// The value of one evidence entry, if recorded.
    pub fn evidence(&self, key: &str) -> Option<u64> {
        self.evidence
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| *v)
    }
}
