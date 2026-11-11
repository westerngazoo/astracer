//! Text renderers for the navigation commands (`explain`, `outline`,
//! `entries`, `calls`, `callers`, `impact`, `search`). Pure functions over
//! `lcw-query` results, so they are unit-tested without running the engine.
//! The JSON views live in `lcw-agent` (shared with `lcw-mcp`).

use std::fmt::Write as _;

use lcw_core::{CodeGraph, NodeId, NodeKind};
use lcw_query::{
    edge_kind_str, kind_str, CallRef, CallTreeNode, Direction, EntryKind, EntryPoint, Impact,
    Lookup, NodeCard, Outline, OutlineNode,
};

/// `file:line` for a node, or `<external>`.
pub fn location(graph: &CodeGraph, id: NodeId) -> String {
    let n = graph.node(id);
    match graph.file_path(n.span.file()) {
        Some(p) if n.kind != NodeKind::External => format!("{}:{}", p.display(), n.span.start_line),
        _ => "<external>".to_string(),
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ---------------------------------------------------------------------------
// explain
// ---------------------------------------------------------------------------

/// `[direct x2 @ main.rs:32]` — the edge behind a caller/callee line.
fn call_tag(c: &CallRef) -> String {
    let mut s = format!("[{}", edge_kind_str(c.kind));
    if c.count > 1 {
        let _ = write!(s, " x{}", c.count);
    }
    if !c.site_file.is_empty() {
        let _ = write!(
            s,
            " @ {}:{}",
            base_name(&c.site_file),
            c.call_site.start_line
        );
    }
    s.push(']');
    s
}

/// The `explain` text view of a node card.
pub fn format_card(card: &NodeCard, limit: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", card.qualified_name);
    let _ = writeln!(out, "  kind:    {}", kind_str(card.kind));
    let _ = writeln!(out, "  where:   {}", card.location());
    if let Some(entry) = card.entry {
        let _ = writeln!(out, "  entry:   {entry}");
    }
    let m = &card.metrics;
    let _ = writeln!(
        out,
        "  metrics: cc {}  params {}  loc {}  nesting {}  allocs {}",
        m.cyclomatic, m.parameters, m.lines_of_code, m.max_nesting, m.allocations
    );
    if !card.flags.is_empty() {
        let _ = writeln!(out, "  flags:   {}", card.flags.join(" "));
    }

    let _ = writeln!(out, "\n  inputs — {} caller(s):", card.inputs.len());
    for c in card.inputs.iter().take(limit) {
        let _ = writeln!(out, "    <- {}   {}", c.qualified_name, call_tag(c));
    }
    if card.inputs.len() > limit {
        let _ = writeln!(out, "    ... +{} more", card.inputs.len() - limit);
    }

    let _ = writeln!(out, "\n  outputs — {} callee(s):", card.outputs.len());
    for c in card.outputs.iter().take(limit) {
        let ext = if c.external { "  (external)" } else { "" };
        let _ = writeln!(out, "    -> {}{}   {}", c.qualified_name, ext, call_tag(c));
    }
    if card.outputs.len() > limit {
        let _ = writeln!(out, "    ... +{} more", card.outputs.len() - limit);
    }
    out
}

// ---------------------------------------------------------------------------
// outline
// ---------------------------------------------------------------------------

/// How much of the outline to print.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutlineStyle {
    /// Collapse scopes at this depth (0 = crates) into a one-line summary.
    pub max_depth: Option<u32>,
    /// Print only scopes (with function counts), never individual functions.
    pub scopes_only: bool,
}

fn entry_marker(entry: Option<EntryKind>) -> &'static str {
    match entry {
        Some(EntryKind::Main) => "  ★ main",
        Some(EntryKind::Exported) => "  ⇥ export",
        Some(EntryKind::PublicRoot) => "  ◇ pub root",
        Some(EntryKind::Root) => "  ○ root",
        Some(EntryKind::Test) => "  ⚑ test",
        None => "",
    }
}

/// Render the outline as a box-drawing tree.
pub fn format_outline(outline: &Outline, graph: &CodeGraph, style: &OutlineStyle) -> String {
    let mut out = format!(
        "outline: {} function(s) in {} scope(s)\n",
        outline.functions, outline.scopes
    );
    let n = outline.roots.len();
    for (i, root) in outline.roots.iter().enumerate() {
        write_outline_node(&mut out, root, graph, "", i + 1 == n, style);
    }
    out
}

fn write_outline_node(
    out: &mut String,
    node: &OutlineNode,
    graph: &CodeGraph,
    prefix: &str,
    last: bool,
    style: &OutlineStyle,
) {
    let connector = if last { "└── " } else { "├── " };
    let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });

    if node.is_scope() {
        let collapsed = style.max_depth.is_some_and(|d| node.depth >= d);
        let entries = if node.entries > 0 {
            format!(
                ", {} entr{}",
                node.entries,
                if node.entries == 1 { "y" } else { "ies" }
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "{prefix}{connector}{}/  ({} fn{entries}){}",
            node.label,
            node.functions,
            if collapsed { "  …" } else { "" }
        );
        if collapsed {
            return;
        }
        let shown: Vec<&OutlineNode> = node
            .children
            .iter()
            .filter(|c| !style.scopes_only || c.is_scope())
            .collect();
        let n = shown.len();
        for (i, c) in shown.into_iter().enumerate() {
            write_outline_node(out, c, graph, &child_prefix, i + 1 == n, style);
        }
    } else if let Some(id) = node.node {
        let _ = writeln!(
            out,
            "{prefix}{connector}{}  cc {} · in {} · out {}{}   {}",
            node.label,
            node.cyclomatic,
            graph.in_degree(id),
            graph.out_degree(id),
            entry_marker(node.entry),
            location(graph, id)
        );
    }
}

// ---------------------------------------------------------------------------
// entries
// ---------------------------------------------------------------------------

/// Render entry points grouped by kind. `reach[i]`, when present, is the
/// number of internal functions reachable from `entries[i]`.
pub fn format_entries(
    graph: &CodeGraph,
    entries: &[EntryPoint],
    reach: &[Option<usize>],
) -> String {
    let mut out = String::new();
    if entries.is_empty() {
        out.push_str("no entry points found\n");
        return out;
    }
    let mut current: Option<EntryKind> = None;
    for (i, e) in entries.iter().enumerate() {
        if current != Some(e.kind) {
            current = Some(e.kind);
            let count = entries.iter().filter(|x| x.kind == e.kind).count();
            let title = match e.kind {
                EntryKind::Main => "main — where the program starts",
                EntryKind::Exported => {
                    "exported — called from outside this code: bootloader, hardware, WASM host, FFI"
                }
                EntryKind::PublicRoot => "public roots — API surface nobody calls internally",
                EntryKind::Root => "private roots — uncalled: dynamic dispatch or dead code",
                EntryKind::Test => "tests — roots for the test harness",
            };
            let _ = writeln!(out, "{}{title} ({count})", if i > 0 { "\n" } else { "" });
        }
        let reach_str = match reach.get(i).copied().flatten() {
            Some(r) => format!("   reaches {r} fn"),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "  {}   [{}]{}",
            graph.node(e.id).qualified_name,
            location(graph, e.id),
            reach_str
        );
    }
    out
}

// ---------------------------------------------------------------------------
// calls
// ---------------------------------------------------------------------------

/// Render a bounded call tree.
pub fn format_call_tree(graph: &CodeGraph, tree: &CallTreeNode, dir: Direction) -> String {
    let root = graph.node(tree.id);
    let mut out = match dir {
        Direction::Callees => format!(
            "calls from {}   [{}]\n",
            root.qualified_name,
            location(graph, tree.id)
        ),
        Direction::Callers => format!(
            "callers of {}   [{}]\n",
            root.qualified_name,
            location(graph, tree.id)
        ),
    };
    let n = tree.children.len() + usize::from(tree.truncated > 0);
    for (i, c) in tree.children.iter().enumerate() {
        write_call_node(&mut out, graph, c, "", i + 1 == n, dir);
    }
    if tree.truncated > 0 {
        let _ = writeln!(out, "└── … +{} more", tree.truncated);
    }
    out
}

/// `  calls @ file:line` for a caller-tree row (where the caller makes the call).
fn caller_site(graph: &CodeGraph, node: &CallTreeNode, dir: Direction) -> String {
    match (dir, node.site) {
        (Direction::Callers, Some(s)) => match graph.file_path(s.file()) {
            Some(p) if s.start_line > 0 => format!("  calls @ {}:{}", p.display(), s.start_line),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn write_call_node(
    out: &mut String,
    graph: &CodeGraph,
    node: &CallTreeNode,
    prefix: &str,
    last: bool,
    dir: Direction,
) {
    let connector = if last { "└── " } else { "├── " };
    let arrow = match dir {
        Direction::Callees => "→",
        Direction::Callers => "←",
    };
    let n = graph.node(node.id);
    let ext = if n.kind == NodeKind::External {
        "  (external)"
    } else {
        ""
    };
    let repeat = if node.repeat { "  ↺" } else { "" };
    let _ = writeln!(
        out,
        "{prefix}{connector}{arrow} {}{ext}{repeat}   [{}]{}",
        n.qualified_name,
        location(graph, node.id),
        caller_site(graph, node, dir)
    );
    let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
    let count = node.children.len() + usize::from(node.truncated > 0);
    for (i, c) in node.children.iter().enumerate() {
        write_call_node(out, graph, c, &child_prefix, i + 1 == count, dir);
    }
    if node.truncated > 0 {
        let _ = writeln!(out, "{child_prefix}└── … +{} more", node.truncated);
    }
}

// ---------------------------------------------------------------------------
// impact
// ---------------------------------------------------------------------------

fn site_of(graph: &CodeGraph, s: lcw_core::SourceSpan) -> String {
    match graph.file_path(s.file()) {
        Some(p) if s.start_line > 0 => format!("{}:{}", p.display(), s.start_line),
        _ => "?".to_string(),
    }
}

/// The `impact` text view: summary line, entry points reached, then every
/// caller (`limit` per list) as `[depth] name  def  calls via @ site`.
pub fn format_impact(graph: &CodeGraph, imp: &Impact, limit: usize) -> String {
    let root = graph.node(imp.root);
    let mut out = format!(
        "impact of {}   [{}]\n",
        root.qualified_name,
        location(graph, imp.root)
    );
    let hist = imp
        .by_depth
        .iter()
        .enumerate()
        .map(|(i, n)| format!("d{} {n}", i + 1))
        .collect::<Vec<_>>()
        .join(" · ");
    let _ = writeln!(
        out,
        "  {} caller(s) in {} file(s) within depth {}{}{}",
        imp.callers.len(),
        imp.files,
        imp.max_depth,
        if hist.is_empty() {
            String::new()
        } else {
            format!(": {hist}")
        },
        if imp.beyond_depth {
            "  (more beyond this depth)"
        } else {
            ""
        }
    );

    let entries: Vec<_> = imp.entry_points().collect();
    if !entries.is_empty() {
        let _ = writeln!(out, "\n  entry points reached ({}):", entries.len());
        for c in entries.iter().take(limit) {
            let kind = c.entry.map(EntryKind::as_str).unwrap_or("");
            let _ = writeln!(
                out,
                "    {kind:<8} {}   [{}]",
                graph.node(c.id).qualified_name,
                location(graph, c.id)
            );
        }
        if entries.len() > limit {
            let _ = writeln!(out, "    ... +{} more", entries.len() - limit);
        }
    }

    if !imp.callers.is_empty() {
        let _ = writeln!(out, "\n  callers:");
    }
    for c in imp.callers.iter().take(limit) {
        let _ = writeln!(
            out,
            "    [{}] {}   [{}]  calls {} @ {}",
            c.depth,
            graph.node(c.id).qualified_name,
            location(graph, c.id),
            graph.node(c.via).name,
            site_of(graph, c.call_site)
        );
    }
    if imp.callers.len() > limit {
        let _ = writeln!(out, "    ... +{} more", imp.callers.len() - limit);
    }
    out
}

// ---------------------------------------------------------------------------
// lookup errors and search
// ---------------------------------------------------------------------------

/// The text shown when a symbol is ambiguous or unknown: the candidates, each
/// with its location, so the next command can use a qualified name.
pub fn format_lookup_error(graph: &CodeGraph, query: &str, lookup: &Lookup) -> String {
    let (head, ids) = match lookup {
        Lookup::Found(_) => return String::new(),
        Lookup::Ambiguous(ids) => (
            format!("{query:?} is ambiguous; candidates (pass a qualified name):"),
            ids,
        ),
        Lookup::NotFound(ids) if ids.is_empty() => {
            return format!("no symbol matches {query:?}\n");
        }
        Lookup::NotFound(ids) => (format!("no symbol matches {query:?}; did you mean:"), ids),
    };
    let mut out = format!("{head}\n");
    for &id in ids {
        let _ = writeln!(
            out,
            "  {}   [{}]",
            graph.node(id).qualified_name,
            location(graph, id)
        );
    }
    out
}

/// Text view of `lcw_agent::json::search` output.
pub fn format_search(v: &serde_json::Value) -> String {
    let mut out = String::new();
    let matches = v["matches"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if matches.is_empty() {
        let _ = writeln!(out, "no symbol matches {}", v["query"]);
        return out;
    }
    if v.get("fuzzy").is_some() {
        let _ = writeln!(out, "no symbol contains {}; closest names:", v["query"]);
    }
    for m in matches {
        let _ = writeln!(
            out,
            "  {}   [{}]",
            m["qualified_name"].as_str().unwrap_or(""),
            m["at"].as_str().unwrap_or("")
        );
    }
    if let Some(n) = v.get("matches_more") {
        let _ = writeln!(out, "  ... +{n} more");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeFlags, NodeStats, SourceSpan};
    use lcw_query::{call_tree, entry_points, node_card, outline, CallTreeOptions};

    fn def(g: &mut CodeGraph, qn: &str, line: u32, is_pub: bool) -> NodeId {
        let f = g.intern_file("src/lib.rs");
        g.add_node(Node {
            id: NodeId(0),
            name: qn.rsplit("::").next().unwrap().to_string(),
            qualified_name: qn.to_string(),
            module_path: qn.rsplit_once("::").map(|(m, _)| m.to_string()).unwrap(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, line, 0, line + 2, 1),
            flags: NodeFlags {
                is_pub,
                ..Default::default()
            },
            stats: NodeStats::default(),
        })
    }

    fn graph() -> CodeGraph {
        let mut g = CodeGraph::new();
        let main = def(&mut g, "app::main", 1, false);
        let run = def(&mut g, "app::core::run", 10, true);
        let step = def(&mut g, "app::core::step", 20, false);
        let f = g.intern_file("src/lib.rs");
        let edge = |l| Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, l, 0, l, 5));
        g.add_edge(main, run, edge(2));
        g.add_edge(run, step, edge(11));
        g.add_edge(run, step, edge(12));
        g
    }

    #[test]
    fn card_text_shows_edges_with_kind_and_site() {
        let g = graph();
        let step = g.node_by_qualified("app::core::step").unwrap();
        let text = format_card(&node_card(&g, step).unwrap(), 20);
        assert!(text.starts_with("app::core::step\n"));
        assert!(text.contains("where:   src/lib.rs:20"));
        assert!(text.contains("<- app::core::run   [direct x2 @ lib.rs:11]"));
        assert!(text.contains("outputs — 0 callee(s)"));
    }

    #[test]
    fn outline_tree_has_scopes_functions_and_markers() {
        let g = graph();
        let text = format_outline(&outline(&g), &g, &OutlineStyle::default());
        assert!(text.starts_with("outline: 3 function(s) in 2 scope(s)"));
        assert!(text.contains("└── app/  (3 fn, 1 entry)"));
        assert!(text.contains("    ├── core/  (2 fn)"));
        assert!(text.contains("    │   ├── run  cc 1 · in 1 · out 1   src/lib.rs:10"));
        assert!(text.contains("    └── main  cc 1 · in 0 · out 1  ★ main   src/lib.rs:1"));

        let collapsed = format_outline(
            &outline(&g),
            &g,
            &OutlineStyle {
                max_depth: Some(1),
                scopes_only: false,
            },
        );
        assert!(collapsed.contains("core/  (2 fn)  …"));
        assert!(!collapsed.contains("run  cc"));
    }

    #[test]
    fn entries_and_call_tree_render() {
        let g = graph();
        let entries = entry_points(&g);
        let text = format_entries(&g, &entries, &[Some(2)]);
        assert!(text.contains("main — where the program starts (1)"));
        assert!(text.contains("  app::main   [src/lib.rs:1]   reaches 2 fn"));

        let main = g.node_by_qualified("app::main").unwrap();
        let tree = call_tree(&g, main, &CallTreeOptions::default());
        let text = format_call_tree(&g, &tree, Direction::Callees);
        assert!(text.starts_with("calls from app::main"));
        assert!(text.contains("└── → app::core::run"));
        assert!(text.contains("    └── → app::core::step"));

        let step = g.node_by_qualified("app::core::step").unwrap();
        let up = CallTreeOptions {
            direction: Direction::Callers,
            ..Default::default()
        };
        let text = format_call_tree(&g, &call_tree(&g, step, &up), Direction::Callers);
        assert!(text.contains("← app::core::run   [src/lib.rs:10]  calls @ src/lib.rs:11"));
    }

    #[test]
    fn impact_and_lookup_errors_render() {
        let mut g = graph();
        let step = g.node_by_qualified("app::core::step").unwrap();
        let imp = lcw_query::impact(&g, step, 3).unwrap();
        let text = format_impact(&g, &imp, 10);
        assert!(text.starts_with("impact of app::core::step   [src/lib.rs:20]"));
        assert!(text.contains("2 caller(s) in 1 file(s) within depth 3: d1 1 · d2 1"));
        assert!(text.contains("main     app::main   [src/lib.rs:1]"));
        assert!(text.contains("[2] app::main   [src/lib.rs:1]  calls run @ src/lib.rs:2"));
        assert!(format_impact(&g, &imp, 1).contains("... +1 more"));

        def(&mut g, "app::other::run", 40, false);
        let l = lcw_query::lookup(&g, "run", 8);
        let text = format_lookup_error(&g, "run", &l);
        assert!(text.starts_with("\"run\" is ambiguous"));
        assert!(text.contains("  app::other::run   [src/lib.rs:40]"));
        let l = lcw_query::lookup(&g, "zzzz", 8);
        assert_eq!(
            format_lookup_error(&g, "zzzz", &l),
            "no symbol matches \"zzzz\"\n"
        );
    }
}
