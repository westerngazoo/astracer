//! Flow tracing: resolve human-friendly symbol patterns to graph nodes and find
//! the shortest call path(s) between an entry point and a target concept. This
//! is the CLI half of the "flow view" — pure graph traversal over
//! [`CodeGraph`]; rendering the result lives behind the `viewer` feature.

use std::collections::{HashMap, HashSet, VecDeque};

use lcw_core::{CodeGraph, Node, NodeId, NodeKind};

/// A traced set of shortest paths from `source` to `target`.
pub struct FlowPaths {
    pub source: NodeId,
    pub target: NodeId,
    /// One or more equally-short paths, each `source .. target` inclusive.
    pub paths: Vec<Vec<NodeId>>,
    /// Distance (in hops) from the source, keyed by `NodeId.0`. Used as the
    /// column for the layered flow view (viewer builds only).
    #[cfg_attr(not(feature = "viewer"), allow(dead_code))]
    pub depth: HashMap<u32, u32>,
}

/// `file:line` for a node (best-effort; empty file for external nodes).
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

/// Resolve a pattern to internal nodes whose qualified name contains it
/// (case-insensitive). Best matches first: exact short-name, then shorter
/// qualified names (favoring the "most direct" symbol).
pub fn resolve(graph: &CodeGraph, pattern: &str) -> Vec<NodeId> {
    let p = pattern.trim().to_lowercase();
    let mut hits: Vec<NodeId> = graph
        .node_ids()
        .filter(|&id| {
            let n = graph.node(id);
            n.kind != NodeKind::External && n.qualified_name.to_lowercase().contains(&p)
        })
        .collect();
    hits.sort_by_key(|&id| {
        let n = graph.node(id);
        let exact = if n.name.to_lowercase() == p { 0 } else { 1 };
        (exact, n.qualified_name.len(), n.qualified_name.clone())
    });
    hits
}

/// Breadth-first search for the shortest path(s) from any `sources` node to the
/// nearest `targets` node, recording *all* shortest-path predecessors so a few
/// alternate routes of the same length can be enumerated.
pub fn shortest_paths(
    graph: &CodeGraph,
    sources: &[NodeId],
    targets: &[NodeId],
    max_paths: usize,
    max_depth: u32,
) -> Option<FlowPaths> {
    if sources.is_empty() || targets.is_empty() {
        return None;
    }
    let target_set: HashSet<u32> = targets.iter().map(|t| t.0).collect();
    let source_set: HashSet<u32> = sources.iter().map(|s| s.0).collect();

    let mut dist: HashMap<u32, u32> = HashMap::new();
    let mut parents: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    for s in sources {
        if dist.insert(s.0, 0).is_none() {
            queue.push_back(s.0);
        }
    }

    let mut found_at: Option<u32> = None;
    let mut reached: Option<u32> = None;
    while let Some(u) = queue.pop_front() {
        let du = dist[&u];
        if let Some(fd) = found_at {
            if du > fd {
                break; // fully explored the layer that first hit a target
            }
        }
        if target_set.contains(&u) {
            found_at = Some(du);
            reached.get_or_insert(u);
            continue; // don't expand past a target
        }
        if du >= max_depth {
            continue;
        }
        for v in graph.neighbors_out(NodeId(u)) {
            let nv = v.0;
            let dv = du + 1;
            match dist.get(&nv) {
                None => {
                    dist.insert(nv, dv);
                    parents.entry(nv).or_default().push(u);
                    queue.push_back(nv);
                }
                Some(&existing) if existing == dv && !source_set.contains(&nv) => {
                    parents.entry(nv).or_default().push(u);
                }
                _ => {}
            }
        }
    }

    let target = reached?;
    let source = *sources.iter().find(|s| dist.get(&s.0) == Some(&0))?;

    // Walk predecessors backward from the target to enumerate up to `max_paths`.
    let mut raw: Vec<Vec<u32>> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    enumerate(
        target,
        &parents,
        &source_set,
        &mut stack,
        &mut raw,
        max_paths,
    );

    let paths: Vec<Vec<NodeId>> = raw
        .into_iter()
        .map(|p| p.into_iter().map(NodeId).collect())
        .collect();
    if paths.is_empty() {
        return None;
    }
    Some(FlowPaths {
        source,
        target: NodeId(target),
        paths,
        depth: dist,
    })
}

fn enumerate(
    node: u32,
    parents: &HashMap<u32, Vec<u32>>,
    sources: &HashSet<u32>,
    stack: &mut Vec<u32>,
    out: &mut Vec<Vec<u32>>,
    max: usize,
) {
    if out.len() >= max {
        return;
    }
    stack.push(node);
    match parents.get(&node) {
        None => {
            // A node with no shortest-path predecessor is a source.
            if sources.contains(&node) {
                out.push(stack.iter().rev().copied().collect());
            }
        }
        Some(preds) => {
            for &p in preds {
                enumerate(p, parents, sources, stack, out, max);
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    stack.pop();
}

/// Render the traced flow as an indented text tree.
pub fn format_text(graph: &CodeGraph, fp: &FlowPaths) -> String {
    let src = graph.node(fp.source);
    let tgt = graph.node(fp.target);
    let hops = fp
        .paths
        .first()
        .map(|p| p.len().saturating_sub(1))
        .unwrap_or(0);
    let mut out = format!(
        "flow: {}  →  {}\n  {} hop(s), {} shortest path(s)\n",
        src.qualified_name,
        tgt.qualified_name,
        hops,
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

fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
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
        "hops": fp.paths.first().map(|p| p.len().saturating_sub(1)).unwrap_or(0),
        "path_count": fp.paths.len(),
        "paths": paths,
    })
}

/// Build the renderer-side [`lcw_render::FlowGraph`] from a traced flow.
#[cfg(feature = "viewer")]
pub fn to_flow_graph(graph: &CodeGraph, fp: &FlowPaths) -> lcw_render::FlowGraph {
    let mut index: HashMap<u32, usize> = HashMap::new();
    let mut nodes: Vec<lcw_render::FlowNode> = Vec::new();
    for path in &fp.paths {
        for &id in path {
            if index.contains_key(&id.0) {
                continue;
            }
            index.insert(id.0, nodes.len());
            let n = graph.node(id);
            nodes.push(lcw_render::FlowNode {
                label: n.name.clone(),
                detail: location_short(graph, n),
                crate_name: n.module_path.split("::").next().unwrap_or("").to_string(),
                column: *fp.depth.get(&id.0).unwrap_or(&0) as usize,
                pick: n.qualified_name.clone(),
                emphasize: id == fp.source || id == fp.target,
            });
        }
    }

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for path in &fp.paths {
        for pair in path.windows(2) {
            let a = index[&pair[0].0];
            let b = index[&pair[1].0];
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
