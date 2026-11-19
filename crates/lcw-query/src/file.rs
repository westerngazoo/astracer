//! **File symbols**: every definition in one source file, in line order, with
//! its fan-in and fan-out — what an editor needs to annotate a whole file
//! (CodeLens, gutter marks) with one query instead of one per function.

use lcw_core::{display_path, CodeGraph, FileId, NodeId, NodeKind};
use serde::{Deserialize, Serialize};

/// One definition in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSymbol {
    pub id: NodeId,
    pub name: String,
    pub qualified_name: String,
    pub kind: NodeKind,
    /// 1-based, inclusive.
    pub start_line: u32,
    /// 1-based, inclusive (never before `start_line`).
    pub end_line: u32,
    /// Distinct callers (incoming edges).
    pub fan_in: usize,
    /// Distinct callees (outgoing edges), external ones included.
    pub fan_out: usize,
}

/// Definitions whose source file is `file`, sorted by start line. `file` is
/// compared with the graph's display paths (forward slashes), so pass it the
/// same way the graph stores it — repo-relative for a repo-relative graph.
pub fn file_symbols(graph: &CodeGraph, file: &str) -> Vec<FileSymbol> {
    let want = normalize(file);
    let files: Vec<FileId> = graph
        .files()
        .filter(|(_, p)| normalize(&display_path(p)) == want)
        .map(|(id, _)| id)
        .collect();
    if files.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<FileSymbol> = graph
        .node_ids()
        .filter_map(|id| {
            let n = graph.node(id);
            if n.kind == NodeKind::External
                || n.span.start_line == 0
                || !files.contains(&n.span.file())
            {
                return None;
            }
            Some(FileSymbol {
                id,
                name: n.name.clone(),
                qualified_name: n.qualified_name.clone(),
                kind: n.kind,
                start_line: n.span.start_line,
                end_line: n.span.end_line.max(n.span.start_line),
                fan_in: graph.in_degree(id),
                fan_out: graph.out_degree(id),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    out
}

fn normalize(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::app_graph;

    #[test]
    fn lists_definitions_in_line_order_with_degrees() {
        let g = app_graph();
        let syms = file_symbols(&g, "src/main.rs");
        let names: Vec<&str> = syms.iter().map(|s| s.qualified_name.as_str()).collect();
        assert_eq!(names, ["app::main", "app::run", "app::orphan"]);
        let run = &syms[1];
        assert_eq!((run.start_line, run.end_line), (20, 23));
        assert_eq!((run.fan_in, run.fan_out), (1, 2));
        assert_eq!(syms[2].fan_in, 0);
    }

    #[test]
    fn accepts_dot_slash_and_backslashes() {
        let g = app_graph();
        assert_eq!(file_symbols(&g, "./src/core.rs").len(), 3);
        assert_eq!(file_symbols(&g, "src\\core.rs").len(), 3);
    }

    #[test]
    fn unknown_file_is_empty() {
        let g = app_graph();
        assert!(file_symbols(&g, "src/missing.rs").is_empty());
        assert!(file_symbols(&g, "main.rs").is_empty());
    }
}
