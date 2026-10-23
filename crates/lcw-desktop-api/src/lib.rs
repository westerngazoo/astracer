//! Shared serde types for the Live Code Walk desktop shell (Tauri backend and
//! wasm frontend). No Tauri or wasm dependencies — only `lcw-core` + `serde`.

use lcw_core::{GroupBox, ReportSnapshot};
use serde::{Deserialize, Serialize};

/// A fully laid-out analysis result returned by `analyze_repo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphView {
    pub report: ReportSnapshot,
    pub positions: Vec<[f32; 2]>,
    /// One box per crate. Older fixtures omit this field.
    #[serde(default)]
    pub groups: Vec<GroupBox>,
}

/// Streaming progress emitted on the `analyze-progress` event channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub message: String,
}

/// Metrics forwarded to the LLM explainer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainMetrics {
    pub cyclomatic: u32,
    pub lines_of_code: u32,
    pub parameters: u32,
    pub max_nesting: u32,
    pub allocations: u32,
}

/// Context assembled from a node card + source snippet for function explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainContext {
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub metrics: ExplainMetrics,
    pub fan_in: usize,
    pub fan_out: usize,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_tree_summary: Option<String>,
}

/// Arguments for the `read_source_snippet` Tauri command (camelCase on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSourceSnippetArgs {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{GraphSnapshot, ReportSnapshot, Summary, Vertical};

    #[test]
    fn graph_view_roundtrip() {
        let view = GraphView {
            report: ReportSnapshot {
                vertical: Vertical::Unknown,
                summary: Summary::default(),
                graph: GraphSnapshot::default(),
                metrics: Vec::new(),
                diagnostics: Vec::new(),
                suggestions: Vec::new(),
            },
            positions: vec![[1.0, 2.0], [3.0, 4.0]],
            groups: Vec::new(),
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: GraphView = serde_json::from_str(&json).unwrap();
        assert_eq!(back.positions, view.positions);
        assert_eq!(back.groups.len(), view.groups.len());
    }

    #[test]
    fn explain_context_camel_case_roundtrip() {
        let ctx = ExplainContext {
            qualified_name: "app::run".into(),
            kind: "function".into(),
            file: "main.rs".into(),
            start_line: 1,
            end_line: 10,
            metrics: ExplainMetrics {
                cyclomatic: 2,
                lines_of_code: 9,
                parameters: 0,
                max_nesting: 1,
                allocations: 0,
            },
            fan_in: 1,
            fan_out: 3,
            callers: vec!["main".into()],
            callees: vec![],
            source: "fn run() {}".into(),
            call_tree_summary: None,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("qualifiedName"));
        assert!(json.contains("startLine"));
        let back: ExplainContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    #[test]
    fn read_source_args_camel_case() {
        let args = ReadSourceSnippetArgs {
            file: "lib.rs".into(),
            start_line: 5,
            end_line: 20,
        };
        let json = serde_json::to_string(&args).unwrap();
        assert!(json.contains("startLine"));
        let back: ReadSourceSnippetArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(args, back);
    }
}
