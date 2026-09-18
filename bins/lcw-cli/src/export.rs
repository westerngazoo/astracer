//! Output formatters for the CLI: a human summary, plus Graphviz DOT and
//! GraphML graph exports. JSON is handled directly via `serde_json`.

use lcw_core::{AnalysisReport, CodeGraph, EdgeKind, Node, NodeKind};
use std::cmp::Reverse;

fn node_kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

fn edge_kind_str(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::DirectCall => "direct",
        EdgeKind::MethodCall => "method",
        EdgeKind::AssociatedCall => "associated",
        EdgeKind::MacroCall => "macro",
        EdgeKind::TraitDispatch => "trait",
        EdgeKind::Unresolved => "unresolved",
    }
}

// ---------------------------------------------------------------------------
// DOT
// ---------------------------------------------------------------------------

fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

/// Render the call graph as Graphviz DOT.
pub fn export_dot(graph: &CodeGraph) -> String {
    let mut out = String::from(
        "digraph livewalk {\n  rankdir=LR;\n  node [shape=box, fontname=\"monospace\"];\n",
    );
    for node in graph.nodes() {
        let mut attrs = format!(
            "label=\"{}\", tooltip=\"{}\"",
            dot_escape(&node.name),
            dot_escape(&node.qualified_name)
        );
        if node.kind == NodeKind::External {
            attrs.push_str(", style=dashed, color=gray");
        } else if node.cyclomatic_complexity() > 10 {
            attrs.push_str(", color=red");
        }
        out.push_str(&format!("  {} [{}];\n", node.id, attrs));
    }
    for (from, to, edge) in graph.edges() {
        out.push_str(&format!(
            "  {from} -> {to} [label=\"{}\"];\n",
            edge_kind_str(edge.kind)
        ));
    }
    out.push_str("}\n");
    out
}

// ---------------------------------------------------------------------------
// GraphML
// ---------------------------------------------------------------------------

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Render the call graph as GraphML.
pub fn export_graphml(graph: &CodeGraph) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n");
    out.push_str("  <key id=\"name\" for=\"node\" attr.name=\"name\" attr.type=\"string\"/>\n");
    out.push_str(
        "  <key id=\"qname\" for=\"node\" attr.name=\"qualified_name\" attr.type=\"string\"/>\n",
    );
    out.push_str("  <key id=\"kind\" for=\"node\" attr.name=\"kind\" attr.type=\"string\"/>\n");
    out.push_str("  <key id=\"cc\" for=\"node\" attr.name=\"cyclomatic\" attr.type=\"int\"/>\n");
    out.push_str("  <key id=\"ekind\" for=\"edge\" attr.name=\"kind\" attr.type=\"string\"/>\n");
    out.push_str("  <graph id=\"G\" edgedefault=\"directed\">\n");
    for node in graph.nodes() {
        out.push_str(&format!("    <node id=\"{}\">\n", node.id));
        out.push_str(&format!(
            "      <data key=\"name\">{}</data>\n",
            xml_escape(&node.name)
        ));
        out.push_str(&format!(
            "      <data key=\"qname\">{}</data>\n",
            xml_escape(&node.qualified_name)
        ));
        out.push_str(&format!(
            "      <data key=\"kind\">{}</data>\n",
            node_kind_str(node.kind)
        ));
        out.push_str(&format!(
            "      <data key=\"cc\">{}</data>\n",
            node.cyclomatic_complexity()
        ));
        out.push_str("    </node>\n");
    }
    for (eid, (from, to, edge)) in graph.edges().enumerate() {
        out.push_str(&format!(
            "    <edge id=\"e{eid}\" source=\"{from}\" target=\"{to}\"><data key=\"ekind\">{}</data></edge>\n",
            edge_kind_str(edge.kind)
        ));
    }
    out.push_str("  </graph>\n</graphml>\n");
    out
}

// ---------------------------------------------------------------------------
// Human summary
// ---------------------------------------------------------------------------

fn is_internal(n: &Node) -> bool {
    n.kind != NodeKind::External
}

/// Render a compact human-readable summary with the worst offenders.
pub fn format_summary(report: &AnalysisReport, top: usize) -> String {
    let g = &report.graph;
    let s = report.summary();
    let mut out = String::new();

    out.push_str("Live Code Walk - analysis summary\n");
    out.push_str("=================================\n");
    out.push_str(&format!("vertical:     {}\n", report.vertical));
    out.push_str(&format!("files:        {}\n", s.files));
    out.push_str(&format!(
        "functions:    {} ({} external/unresolved)\n",
        s.nodes - s.external_nodes,
        s.external_nodes
    ));
    out.push_str(&format!("call edges:   {}\n", s.edges));
    out.push_str(&format!(
        "diagnostics:  {} ({} high severity)\n",
        s.diagnostics, s.high_severity
    ));
    out.push_str(&format!("suggestions:  {}\n", s.suggestions));

    // Top by cyclomatic complexity.
    let mut by_cc: Vec<&Node> = g.nodes().filter(|n| is_internal(n)).collect();
    by_cc.sort_by_key(|n| Reverse(n.cyclomatic_complexity()));
    if !by_cc.is_empty() {
        out.push_str(&format!("\nTop {top} by cyclomatic complexity:\n"));
        for n in by_cc.iter().take(top) {
            out.push_str(&format!(
                "  {:>4}  {}\n",
                n.cyclomatic_complexity(),
                n.qualified_name
            ));
        }
    }

    // Top by fan-in (most depended-upon).
    let mut ids: Vec<_> = g.node_ids().filter(|&id| is_internal(g.node(id))).collect();
    ids.sort_by_key(|&id| Reverse(g.in_degree(id)));
    let has_edges = ids.iter().any(|&id| g.in_degree(id) > 0);
    if has_edges {
        out.push_str(&format!("\nTop {top} by fan-in (callers):\n"));
        for &id in ids.iter().take(top) {
            let deg = g.in_degree(id);
            if deg == 0 {
                break;
            }
            out.push_str(&format!("  {:>4}  {}\n", deg, g.node(id).qualified_name));
        }
    }

    // Diagnostics (populated once Layer 2 lenses run).
    if !report.diagnostics.is_empty() {
        out.push_str("\nDiagnostics:\n");
        let mut diags: Vec<_> = report.diagnostics.iter().collect();
        diags.sort_by_key(|d| Reverse(d.severity));
        for d in diags.iter().take(top) {
            out.push_str(&format!("  [{}] {} ({})\n", d.severity, d.message, d.code));
        }
    }

    // Suggestions (populated once Layer 3 advisors run).
    if !report.suggestions.is_empty() {
        out.push_str("\nSuggestions:\n");
        let mut sugg: Vec<_> = report.suggestions.iter().collect();
        sugg.sort_by_key(|s| Reverse(s.priority));
        for su in sugg.iter().take(top) {
            out.push_str(&format!(
                "  ({:>3}) {}\n        {}\n",
                su.priority, su.title, su.rationale
            ));
        }
    }

    out
}
