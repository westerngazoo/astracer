//! Flow **rendering** for the CLI: the traced path(s) as an indented text
//! tree and as the renderer-side layered diagram (the JSON slice lives in
//! `lcw-agent`, shared with `lcw-mcp`). Traversal itself — resolving symbols
//! and the breadth-first shortest-path search — lives in `lcw-query`, shared
//! with the native viewer and the desktop app so the three can never disagree.

use lcw_core::{CodeGraph, Node, NodeId};
use lcw_query::FlowPaths;

/// `file:line` for a node (best-effort; `<external>` for external nodes).
pub fn location(graph: &CodeGraph, n: &Node) -> String {
    let file = graph
        .file_path(n.span.file())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if file.is_empty() {
        String::from("<external>")
    } else {
        format!("{}:{}", file, n.span.start_line)
    }
}

/// `basename:line` — compact form for the space-constrained flow diagram.
#[cfg(feature = "viewer")]
pub fn location_short(graph: &CodeGraph, n: &Node) -> String {
    let base = graph
        .file_path(n.span.file())
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if base.is_empty() {
        String::new()
    } else {
        format!("{}:{}", base, n.span.start_line)
    }
}

/// Render the traced flow as an indented text tree.
pub fn format_text(graph: &CodeGraph, fp: &FlowPaths) -> String {
    format_hops(graph, fp, None)
}

/// Source of one hop for [`format_text_with_snippets`]: its first line
/// number and its code.
pub type HopCode = (u32, String);

/// [`format_text`] with each hop's source under it, numbered and clipped to
/// `max_lines` lines. `code` returns a hop's source (`None` to skip it).
pub fn format_text_with_snippets(
    graph: &CodeGraph,
    fp: &FlowPaths,
    code: &dyn Fn(NodeId) -> Option<HopCode>,
    max_lines: usize,
) -> String {
    format_hops(graph, fp, Some((code, max_lines)))
}

type SnippetSource<'a> = (&'a dyn Fn(NodeId) -> Option<HopCode>, usize);

fn format_hops(graph: &CodeGraph, fp: &FlowPaths, snippets: Option<SnippetSource>) -> String {
    let src = graph.node(fp.source);
    let tgt = graph.node(fp.target);
    let mut out = format!(
        "flow: {}  →  {}\n  {} hop(s), {} shortest path(s)\n",
        src.qualified_name,
        tgt.qualified_name,
        fp.hops(),
        fp.paths.len()
    );
    for (i, path) in fp.paths.iter().enumerate() {
        out.push_str(&format!("\n  path {}:\n", i + 1));
        for (h, &id) in path.iter().enumerate() {
            let n = graph.node(id);
            let connector = if h == 0 { "   " } else { "  ↳" };
            let indent = "  ".repeat(h);
            out.push_str(&format!(
                "{}{} {}   [{}]\n",
                indent,
                connector,
                n.qualified_name,
                location(graph, n)
            ));
            if let Some((code, max_lines)) = snippets {
                if let Some((start, text)) = code(id) {
                    write_snippet(
                        &mut out,
                        &format!("{indent}      "),
                        start,
                        &text,
                        max_lines,
                    );
                }
            }
        }
    }
    out
}

/// `  168 | code` lines, then `… +N more line(s)` when clipped.
fn write_snippet(out: &mut String, indent: &str, start: u32, text: &str, max_lines: usize) {
    let lines: Vec<&str> = text.lines().collect();
    let last = start as usize + lines.len().saturating_sub(1);
    let width = last.to_string().len();
    for (i, line) in lines.iter().take(max_lines.max(1)).enumerate() {
        let num = start as usize + i;
        out.push_str(&format!("{indent}{num:>width$} | {line}\n"));
    }
    if lines.len() > max_lines.max(1) {
        out.push_str(&format!(
            "{indent}{:>width$} … +{} more line(s)\n",
            "",
            lines.len() - max_lines.max(1)
        ));
    }
}

/// Build the renderer-side [`lcw_render::FlowGraph`] from a traced flow.
#[cfg(feature = "viewer")]
pub fn to_flow_graph(graph: &CodeGraph, fp: &FlowPaths) -> lcw_render::FlowGraph {
    use std::collections::{HashMap, HashSet};

    let mut index: HashMap<NodeId, usize> = HashMap::new();
    let mut nodes: Vec<lcw_render::FlowNode> = Vec::new();
    for path in &fp.paths {
        for &id in path {
            if index.contains_key(&id) {
                continue;
            }
            index.insert(id, nodes.len());
            let n = graph.node(id);
            nodes.push(lcw_render::FlowNode {
                label: n.name.clone(),
                detail: location_short(graph, n),
                crate_name: n.module_path.split("::").next().unwrap_or("").to_string(),
                column: *fp.depth.get(&id).unwrap_or(&0) as usize,
                pick: n.qualified_name.clone(),
                emphasize: id == fp.source || id == fp.target,
            });
        }
    }

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for path in &fp.paths {
        for pair in path.windows(2) {
            let a = index[&pair[0]];
            let b = index[&pair[1]];
            if seen.insert((a, b)) {
                edges.push((a, b));
            }
        }
    }

    lcw_render::FlowGraph {
        title: format!(
            "flow: {}  ->  {}",
            graph.node(fp.source).name,
            graph.node(fp.target).name
        ),
        nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, NodeFlags, NodeKind, NodeStats, SourceSpan};

    fn def(g: &mut CodeGraph, qn: &str, line: u32) -> NodeId {
        let f = g.intern_file("src/lib.rs");
        g.add_node(Node {
            id: NodeId(0),
            name: qn.rsplit("::").next().unwrap().to_string(),
            qualified_name: qn.to_string(),
            module_path: qn.rsplit_once("::").map(|(m, _)| m.to_string()).unwrap(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, line, 0, line + 2, 1),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        })
    }

    #[test]
    fn snippets_render_as_numbered_clipped_text() {
        let mut g = CodeGraph::new();
        let main = def(&mut g, "app::main", 9);
        let run = def(&mut g, "app::run", 20);
        let f = g.intern_file("src/lib.rs");
        g.add_edge(
            main,
            run,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 10, 0, 10, 5)),
        );
        let fp = lcw_query::shortest_paths(&g, &[main], &[run], 1, 8).unwrap();
        let code = |id: NodeId| -> Option<HopCode> {
            if id == main {
                Some((9, "fn main() {\n    run();\n}".into()))
            } else {
                Some((20, "fn run() {\n    a();\n    b();\n}".into()))
            }
        };
        let text = format_text_with_snippets(&g, &fp, &code, 2);
        assert!(text.contains("    app::main   [src/lib.rs:9]\n"), "{text}");
        assert!(text.contains("       9 | fn main() {\n"), "{text}");
        assert!(text.contains("      10 |     run();\n"), "{text}");
        assert!(text.contains("  ↳ app::run   [src/lib.rs:20]\n"), "{text}");
        assert!(text.contains("        20 | fn run() {\n"), "{text}");
        assert!(text.contains("           … +2 more line(s)\n"), "{text}");
        assert!(!text.contains("b();"));
        assert!(!text.starts_with('{'));
    }
}
