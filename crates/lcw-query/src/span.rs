//! **Span**: the exact definition range of a symbol, `file:start-end`, so an
//! editor (or an agent) can read or replace precisely that body instead of the
//! whole file. The range is the node's parsed span: for Rust it already covers
//! leading attributes and doc comments, the same lines `read_snippet`
//! highlights.

use lcw_core::{CodeGraph, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

/// Where one definition lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSpan {
    pub id: NodeId,
    pub qualified_name: String,
    pub kind: NodeKind,
    pub file: String,
    /// 1-based, inclusive.
    pub start_line: u32,
    /// 1-based, inclusive (never before `start_line`).
    pub end_line: u32,
}

impl SymbolSpan {
    /// `file:start-end`.
    pub fn range(&self) -> String {
        format!("{}:{}-{}", self.file, self.start_line, self.end_line)
    }

    pub fn line_count(&self) -> u32 {
        self.end_line - self.start_line + 1
    }
}

/// The definition span of `id`, or `None` for external nodes, out-of-range ids
/// and nodes without a source file.
pub fn span(graph: &CodeGraph, id: NodeId) -> Option<SymbolSpan> {
    let n = graph.try_node(id)?;
    if n.kind == NodeKind::External || n.span.start_line == 0 {
        return None;
    }
    let file = graph.file_path(n.span.file())?.display().to_string();
    Some(SymbolSpan {
        id,
        qualified_name: n.qualified_name.clone(),
        kind: n.kind,
        file,
        start_line: n.span.start_line,
        end_line: n.span.end_line.max(n.span.start_line),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id};

    #[test]
    fn span_reports_file_and_inclusive_line_range() {
        let g = app_graph();
        let s = span(&g, id(&g, "app::core::parse")).unwrap();
        assert_eq!(s.file, "src/core.rs");
        assert_eq!((s.start_line, s.end_line), (8, 11));
        assert_eq!(s.range(), "src/core.rs:8-11");
        assert_eq!(s.line_count(), 4);
        assert_eq!(s.kind, NodeKind::Function);
    }

    #[test]
    fn span_is_none_for_externals_and_bad_ids() {
        let g = app_graph();
        let ext = g.node_by_qualified("std::fs::read").unwrap();
        assert!(span(&g, ext).is_none());
        assert!(span(&g, NodeId(999)).is_none());
    }
}
