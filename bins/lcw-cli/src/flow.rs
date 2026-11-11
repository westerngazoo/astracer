//! Flow **rendering** for the CLI: the traced path(s) as an indented text
//! tree and as the renderer-side layered diagram (the JSON slice lives in
//! `lcw-agent`, shared with `lcw-mcp`). Traversal itself — resolving symbols
//! and the breadth-first shortest-path search — lives in `lcw-query`, shared
//! with the native viewer and the desktop app so the three can never disagree.

#[cfg(feature = "viewer")]
use lcw_core::NodeId;
use lcw_core::{CodeGraph, Node};
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
