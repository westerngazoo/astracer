//! The end-to-end analysis result the engine hands back to any front end
//! (CLI, Tauri UI, tests).

use serde::{Deserialize, Serialize};

use crate::diagnostic::{Diagnostic, Severity};
use crate::graph::{CodeGraph, GraphSnapshot};
use crate::metric::Metric;
use crate::suggestion::Suggestion;
use crate::vertical::Vertical;

/// High-level counts, handy for CLI summaries and UI headers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Summary {
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
    pub external_nodes: usize,
    pub diagnostics: usize,
    pub high_severity: usize,
    pub suggestions: usize,
}

/// Everything produced by a run of the pipeline.
#[derive(Debug, Default)]
pub struct AnalysisReport {
    pub graph: CodeGraph,
    pub metrics: Vec<Metric>,
    pub diagnostics: Vec<Diagnostic>,
    pub suggestions: Vec<Suggestion>,
    /// Detected or declared industry vertical.
    pub vertical: Vertical,
}

impl AnalysisReport {
    pub fn new(graph: CodeGraph) -> Self {
        AnalysisReport {
            graph,
            ..Default::default()
        }
    }

    /// Compute a fresh [`Summary`] from the current contents.
    pub fn summary(&self) -> Summary {
        let external_nodes = self
            .graph
            .nodes()
            .filter(|n| matches!(n.kind, crate::graph::NodeKind::External))
            .count();
        let high_severity = self
            .diagnostics
            .iter()
            .filter(|d| d.severity >= Severity::High)
            .count();
        Summary {
            files: self.graph.file_count(),
            nodes: self.graph.node_count(),
            edges: self.graph.edge_count(),
            external_nodes,
            diagnostics: self.diagnostics.len(),
            high_severity,
            suggestions: self.suggestions.len(),
        }
    }

    /// A fully serializable snapshot (graph flattened + findings), used by the
    /// CLI `--format json` output and by golden tests.
    pub fn export(&self) -> ReportExport<'_> {
        ReportExport {
            vertical: self.vertical,
            summary: self.summary(),
            graph: self.graph.export(),
            metrics: &self.metrics,
            diagnostics: &self.diagnostics,
            suggestions: &self.suggestions,
        }
    }

    /// An **owned**, round-trippable snapshot. This is what the engine hands to
    /// front ends over a transport boundary (Tauri `invoke`, a VS Code webview
    /// `postMessage`); [`from_snapshot`] rebuilds a working report on the other
    /// side so the wasm renderer can consume it.
    ///
    /// [`from_snapshot`]: AnalysisReport::from_snapshot
    pub fn snapshot(&self) -> ReportSnapshot {
        ReportSnapshot {
            vertical: self.vertical,
            summary: self.summary(),
            graph: self.graph.snapshot(),
            metrics: self.metrics.clone(),
            diagnostics: self.diagnostics.clone(),
            suggestions: self.suggestions.clone(),
        }
    }

    /// Rebuild a report from a [`ReportSnapshot`] (reconstructs the graph).
    pub fn from_snapshot(snapshot: ReportSnapshot) -> Self {
        AnalysisReport {
            graph: CodeGraph::from_snapshot(snapshot.graph),
            metrics: snapshot.metrics,
            diagnostics: snapshot.diagnostics,
            suggestions: snapshot.suggestions,
            vertical: snapshot.vertical,
        }
    }
}

/// Borrowed serializable projection of [`AnalysisReport`] (zero-copy export).
#[derive(Debug, Serialize)]
pub struct ReportExport<'a> {
    pub vertical: Vertical,
    pub summary: Summary,
    pub graph: crate::graph::GraphExport<'a>,
    pub metrics: &'a [Metric],
    pub diagnostics: &'a [Diagnostic],
    pub suggestions: &'a [Suggestion],
}

/// Owned, round-trippable projection of [`AnalysisReport`] (see
/// [`AnalysisReport::snapshot`]). Suitable for `serde` transport and caching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportSnapshot {
    pub vertical: Vertical,
    pub summary: Summary,
    pub graph: GraphSnapshot,
    pub metrics: Vec<Metric>,
    pub diagnostics: Vec<Diagnostic>,
    pub suggestions: Vec<Suggestion>,
}
