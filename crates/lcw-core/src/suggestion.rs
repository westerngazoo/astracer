//! Architectural suggestions (Layer 3) and the optimization targets they serve.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Optimization targets from Manifesto section 2.3 / section 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Scalability,
    Maintainability,
    Robustness,
    Latency,
    Performance,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Scalability => "scalability",
            Target::Maintainability => "maintainability",
            Target::Robustness => "robustness",
            Target::Latency => "latency",
            Target::Performance => "performance",
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recommendation produced by an [`crate::Advisor`]-style rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub title: String,
    pub rationale: String,
    pub targets: Vec<Target>,
    /// 0 (lowest) ..= 100 (highest). Weighted by config target priorities.
    pub priority: u8,
    pub node: Option<NodeId>,
}

impl Suggestion {
    pub fn new(title: impl Into<String>, rationale: impl Into<String>) -> Self {
        Suggestion {
            title: title.into(),
            rationale: rationale.into(),
            targets: Vec::new(),
            priority: 50,
            node: None,
        }
    }

    pub fn with_targets(mut self, targets: impl IntoIterator<Item = Target>) -> Self {
        self.targets = targets.into_iter().collect();
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn at(mut self, node: NodeId) -> Self {
        self.node = Some(node);
        self
    }
}
