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

/// The innermost definition in `file` whose span contains `line` (1-based):
/// the closure inside a method inside an `impl`, not the method. Ties go to
/// the later start (the nested one).
pub fn node_at(graph: &CodeGraph, file: &str, line: u32) -> Option<NodeId> {
    file_symbols(graph, file)
        .into_iter()
        .filter(|s| s.start_line <= line && line <= s.end_line)
        .min_by(|a, b| {
            (a.end_line - a.start_line)
                .cmp(&(b.end_line - b.start_line))
                .then_with(|| b.start_line.cmp(&a.start_line))
        })
        .map(|s| s.id)
}

/// The definition in `file` closest to `line` (1-based): the innermost one
/// containing it ([`node_at`]), else the one with the fewest lines between its
/// span and `line`. Ties go to the definition below the line, since the gap
/// above a function is usually its doc comment or attributes.
pub fn nearest_at(graph: &CodeGraph, file: &str, line: u32) -> Option<NodeId> {
    if let Some(id) = node_at(graph, file, line) {
        return Some(id);
    }
    file_symbols(graph, file)
        .into_iter()
        .min_by_key(|s| {
            if line < s.start_line {
                (s.start_line - line, 0)
            } else {
                (line - s.end_line, 1)
            }
        })
        .map(|s| s.id)
}

/// Where an editor location lands in the graph: the definition nearest `line`
/// ([`nearest_at`]: the one under it, else the closest — cursor between
/// functions), or the file's first definition without a line (a file picked
/// in an explorer). `file` may be absolute: it is tried relative to
/// `repo_root` first, then as given, since graphs store repo-relative paths.
pub fn focus_target(
    graph: &CodeGraph,
    repo_root: &str,
    file: &str,
    line: Option<u32>,
) -> Option<NodeId> {
    let rel = repo_relative(repo_root, file);
    let candidates = [rel.as_str(), file];
    for f in candidates {
        let found = match line {
            Some(l) => nearest_at(graph, f, l),
            None => file_symbols(graph, f).first().map(|s| s.id),
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// `file` relative to `root` (forward slashes), or `file` itself when it is
/// not under `root`.
pub fn repo_relative(root: &str, file: &str) -> String {
    let f = normalize(file);
    let r = normalize(root);
    let r = r.trim_end_matches('/');
    if r.is_empty() {
        return f;
    }
    match f.strip_prefix(r) {
        Some(rest) if rest.starts_with('/') => rest.trim_start_matches('/').to_string(),
        _ => f,
    }
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
    fn node_at_picks_the_innermost_definition() {
        let mut g = app_graph();
        // `run` spans 20..=23; nest a closure at 21..=22 inside it.
        let f = g.intern_file("src/main.rs");
        let closure = g.add_node(lcw_core::Node {
            id: NodeId(0),
            name: "{closure}".into(),
            qualified_name: "app::run::{closure}".into(),
            module_path: "app::run".into(),
            kind: NodeKind::Closure,
            span: lcw_core::SourceSpan::new(f, 21, 4, 22, 5),
            flags: Default::default(),
            stats: Default::default(),
        });
        let run = crate::fixtures::id(&g, "app::run");
        assert_eq!(node_at(&g, "src/main.rs", 21), Some(closure));
        assert_eq!(node_at(&g, "src/main.rs", 20), Some(run));
        assert_eq!(node_at(&g, "src/main.rs", 23), Some(run));
        assert_eq!(node_at(&g, "src/main.rs", 40), None);
    }

    #[test]
    fn nearest_at_prefers_the_closest_span_then_the_one_below() {
        let g = app_graph();
        let load = crate::fixtures::id(&g, "app::io::load");
        let save = crate::fixtures::id(&g, "app::io::save");
        // load spans 5..=8, save 30..=33.
        assert_eq!(nearest_at(&g, "src/io.rs", 6), Some(load));
        assert_eq!(nearest_at(&g, "src/io.rs", 1), Some(load));
        assert_eq!(nearest_at(&g, "src/io.rs", 12), Some(load));
        assert_eq!(nearest_at(&g, "src/io.rs", 28), Some(save));
        // 19 is 11 below load and 11 above save: the one below wins.
        assert_eq!(nearest_at(&g, "src/io.rs", 19), Some(save));
        assert_eq!(nearest_at(&g, "src/io.rs", 900), Some(save));
        assert_eq!(nearest_at(&g, "src/missing.rs", 3), None);
    }

    #[test]
    fn focus_target_falls_back_to_the_nearest_definition() {
        let g = app_graph();
        let main = crate::fixtures::id(&g, "app::main");
        let save = crate::fixtures::id(&g, "app::io::save");
        let load = crate::fixtures::id(&g, "app::io::load");
        assert_eq!(
            focus_target(&g, "/repo", "/repo/src/io.rs", Some(31)),
            Some(save)
        );
        assert_eq!(
            focus_target(&g, "/repo", "/repo/src/io.rs", Some(14)),
            Some(load)
        );
        assert_eq!(
            focus_target(&g, "/repo", "/repo/src/io.rs", Some(25)),
            Some(save)
        );
        assert_eq!(
            focus_target(&g, "/repo/", "/repo/src/main.rs", None),
            Some(main)
        );
        assert_eq!(focus_target(&g, "", "src/io.rs", Some(32)), Some(save));
        assert_eq!(
            focus_target(&g, "/repo", "/elsewhere/src/io.rs", Some(31)),
            None
        );
    }

    #[test]
    fn repo_relative_strips_only_whole_components() {
        assert_eq!(repo_relative("/r", "/r/src/a.rs"), "src/a.rs");
        assert_eq!(repo_relative("/r/", "/r/src/a.rs"), "src/a.rs");
        assert_eq!(repo_relative("C:\\r", "C:\\r\\src\\a.rs"), "src/a.rs");
        assert_eq!(repo_relative("/r", "/rx/a.rs"), "/rx/a.rs");
        assert_eq!(repo_relative("", "src/a.rs"), "src/a.rs");
    }

    #[test]
    fn unknown_file_is_empty() {
        let g = app_graph();
        assert!(file_symbols(&g, "src/missing.rs").is_empty());
        assert!(file_symbols(&g, "main.rs").is_empty());
    }
}
