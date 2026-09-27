//! Flow **rendering** for the CLI: the traced path(s) as an indented text
//! tree, as a JSON slice (optionally with each hop's source), and as the
//! renderer-side layered diagram. Traversal itself — resolving symbols and the
//! breadth-first shortest-path search — lives in `lcw-query`, shared with the
//! native viewer and the desktop app so the three can never disagree.

use lcw_core::{CodeGraph, Node, NodeId};
use lcw_query::{kind_str, FlowPaths};

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
        }
    }
    out
}

/// Read lines `[start, end]` (1-based, inclusive) from `file`, best-effort.
fn read_snippet(file: &str, start: u32, end: u32) -> Option<String> {
    if file.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let start = (start.max(1) as usize).min(lines.len());
    if start == 0 || start > lines.len() {
        return None;
    }
    let end = (end.max(start as u32) as usize).min(lines.len());
    Some(lines[start - 1..end].join("\n"))
}

/// Serialize a traced flow to JSON — a scoped, LLM-friendly slice: the endpoints
/// plus each shortest path as an ordered list of nodes with `file:line` (and,
/// when `snippets`, the source of each hop).
pub fn to_json(graph: &CodeGraph, fp: &FlowPaths, snippets: bool) -> serde_json::Value {
    let node_json = |id: NodeId| -> serde_json::Value {
        let n = graph.node(id);
        let file = graph
            .file_path(n.span.file())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut obj = serde_json::json!({
            "qualified_name": n.qualified_name,
            "name": n.name,
            "kind": kind_str(n.kind),
            "file": file,
            "line": n.span.start_line,
        });
        if snippets {
            if let Some(code) = read_snippet(&file, n.span.start_line, n.span.end_line) {
                obj["code"] = serde_json::Value::String(code);
            }
        }
        obj
    };
    let paths: Vec<serde_json::Value> = fp
        .paths
        .iter()
        .map(|p| serde_json::Value::Array(p.iter().map(|&id| node_json(id)).collect()))
        .collect();
    serde_json::json!({
        "source": graph.node(fp.source).qualified_name,
        "target": graph.node(fp.target).qualified_name,
        "hops": fp.hops(),
        "path_count": fp.paths.len(),
        "paths": paths,
    })
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
