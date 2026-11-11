//! The versioned JSON views (`"schema": 1`). Conventions, kept identical
//! across commands so an agent learns them once:
//!
//! * locations are strings: `"at": "file:line"` (definition), `"site":
//!   "file:line"` (call site, in the caller's file), `"span": "file:a-b"`;
//! * a list cut by a limit or budget gets a sibling `"<list>_more": N`;
//! * optional fields are omitted rather than `null` / `false`.

use lcw_core::{CodeGraph, NodeId, NodeKind, SourceSpan};
use lcw_query::{
    edge_kind_str, kind_str, CallRef, CallTreeNode, Direction, EntryKind, EntryPoint, FlowPaths,
    Impact, Lookup, Outline, OutlineNode, SymbolSpan,
};
use serde_json::{json, Map, Value};

use crate::SCHEMA;

/// `file:line` for a node's definition, or `<external>`.
pub fn at(graph: &CodeGraph, id: NodeId) -> String {
    let n = graph.node(id);
    match graph.file_path(n.span.file()) {
        Some(p) if n.kind != NodeKind::External => format!("{}:{}", p.display(), n.span.start_line),
        _ => "<external>".to_string(),
    }
}

/// `file:line` for a call site, or `None` when its file is unknown.
pub fn site(graph: &CodeGraph, span: SourceSpan) -> Option<String> {
    graph
        .file_path(span.file())
        .filter(|_| span.start_line > 0)
        .map(|p| format!("{}:{}", p.display(), span.start_line))
}

/// `{"schema": 1, ...fields}`.
fn with_schema(fields: Value) -> Value {
    let mut obj = Map::new();
    obj.insert("schema".into(), json!(SCHEMA));
    if let Value::Object(m) = fields {
        obj.extend(m);
    }
    Value::Object(obj)
}

/// Insert `items` (capped at `limit`) under `key`, plus `key_more` when cut.
fn put_list(obj: &mut Map<String, Value>, key: &str, items: Vec<Value>, limit: usize) {
    let total = items.len();
    let kept: Vec<Value> = items.into_iter().take(limit).collect();
    let more = total - kept.len();
    obj.insert(key.into(), Value::Array(kept));
    if more > 0 {
        obj.insert(format!("{key}_more"), json!(more));
    }
}

/// A compact reference to a symbol: name, kind and definition location.
pub fn symbol(graph: &CodeGraph, id: NodeId) -> Value {
    let n = graph.node(id);
    json!({
        "qualified_name": n.qualified_name,
        "kind": kind_str(n.kind),
        "at": at(graph, id),
    })
}

fn call_ref(graph: &CodeGraph, c: &CallRef) -> Value {
    let mut o = Map::new();
    o.insert("qualified_name".into(), json!(c.qualified_name));
    o.insert("edge".into(), json!(edge_kind_str(c.kind)));
    if c.count > 1 {
        o.insert("count".into(), json!(c.count));
    }
    if c.external {
        o.insert("external".into(), json!(true));
    } else {
        o.insert("at".into(), json!(at(graph, c.id)));
    }
    if let Some(s) = site(graph, c.call_site) {
        o.insert("site".into(), json!(s));
    }
    Value::Object(o)
}

/// `explain`: the node card — location, metrics, callers and callees.
pub fn explain(graph: &CodeGraph, id: NodeId, limit: usize) -> Option<Value> {
    let card = lcw_query::node_card(graph, id)?;
    let span = lcw_query::span(graph, id);
    let mut o = Map::new();
    o.insert("qualified_name".into(), json!(card.qualified_name));
    o.insert("kind".into(), json!(kind_str(card.kind)));
    o.insert("at".into(), json!(card.location()));
    if let Some(s) = &span {
        o.insert("span".into(), json!(s.range()));
    }
    if let Some(e) = card.entry {
        o.insert("entry".into(), json!(e.as_str()));
    }
    if !card.flags.is_empty() {
        o.insert("flags".into(), json!(card.flags));
    }
    let m = &card.metrics;
    o.insert(
        "metrics".into(),
        json!({
            "cyclomatic": m.cyclomatic,
            "params": m.parameters,
            "loc": m.lines_of_code,
            "nesting": m.max_nesting,
            "allocs": m.allocations,
        }),
    );
    o.insert("fan_in".into(), json!(card.fan_in));
    o.insert("fan_out".into(), json!(card.fan_out));
    let callers = card.inputs.iter().map(|c| call_ref(graph, c)).collect();
    let callees = card.outputs.iter().map(|c| call_ref(graph, c)).collect();
    put_list(&mut o, "callers", callers, limit);
    put_list(&mut o, "callees", callees, limit);
    Some(with_schema(Value::Object(o)))
}

fn tree_node(graph: &CodeGraph, t: &CallTreeNode) -> Value {
    let n = graph.node(t.id);
    let mut o = Map::new();
    o.insert("qualified_name".into(), json!(n.qualified_name));
    if n.kind == NodeKind::External {
        o.insert("external".into(), json!(true));
    } else {
        o.insert("at".into(), json!(at(graph, t.id)));
    }
    if let Some(s) = t.site.and_then(|s| site(graph, s)) {
        o.insert("site".into(), json!(s));
    }
    if t.repeat {
        o.insert("repeat".into(), json!(true));
    }
    if !t.children.is_empty() {
        let kids = t.children.iter().map(|c| tree_node(graph, c)).collect();
        o.insert("children".into(), Value::Array(kids));
    }
    if t.truncated > 0 {
        o.insert("children_more".into(), json!(t.truncated));
    }
    Value::Object(o)
}

/// `calls` / `callers`: a bounded call tree. Children of a callees tree are in
/// call order; `site` is where the parent calls the child (callees) or the
/// child calls the parent (callers).
pub fn call_tree(graph: &CodeGraph, tree: &CallTreeNode, dir: Direction, depth: u32) -> Value {
    with_schema(json!({
        "direction": match dir { Direction::Callees => "callees", Direction::Callers => "callers" },
        "depth": depth,
        "tree": tree_node(graph, tree),
    }))
}

/// `impact`: transitive callers (blast radius) with depth histogram and the
/// entry points the change can reach.
pub fn impact(graph: &CodeGraph, imp: &Impact, limit: usize) -> Value {
    let mut o = Map::new();
    o.insert("symbol".into(), symbol(graph, imp.root));
    o.insert("depth".into(), json!(imp.max_depth));
    o.insert("total_callers".into(), json!(imp.callers.len()));
    o.insert("by_depth".into(), json!(imp.by_depth));
    o.insert("files".into(), json!(imp.files));
    if imp.beyond_depth {
        o.insert("beyond_depth".into(), json!(true));
    }
    let entries = imp
        .entry_points()
        .map(|c| {
            json!({
                "qualified_name": graph.node(c.id).qualified_name,
                "entry": c.entry.map(EntryKind::as_str),
                "depth": c.depth,
                "at": at(graph, c.id),
            })
        })
        .collect();
    put_list(&mut o, "entry_points", entries, limit);
    let callers = imp
        .callers
        .iter()
        .map(|c| {
            let mut r = Map::new();
            r.insert(
                "qualified_name".into(),
                json!(graph.node(c.id).qualified_name),
            );
            r.insert("depth".into(), json!(c.depth));
            r.insert("at".into(), json!(at(graph, c.id)));
            if let Some(s) = site(graph, c.call_site) {
                r.insert("site".into(), json!(s));
            }
            r.insert("via".into(), json!(graph.node(c.via).qualified_name));
            Value::Object(r)
        })
        .collect();
    put_list(&mut o, "callers", callers, limit);
    with_schema(Value::Object(o))
}

/// `span`: the exact definition range, optionally with its code.
pub fn span(s: &SymbolSpan, code: Option<&str>) -> Value {
    let mut o = json!({
        "qualified_name": s.qualified_name,
        "kind": kind_str(s.kind),
        "file": s.file,
        "start_line": s.start_line,
        "end_line": s.end_line,
        "lines": s.line_count(),
        "span": s.range(),
    });
    if let Some(c) = code {
        o["code"] = json!(c);
    }
    with_schema(o)
}

/// The exact source lines of a span (`start..=end`), read from disk with
/// [`lcw_core::read_snippet`] so it highlights the same range. `repo_root`
/// resolves relative paths.
pub fn span_code(repo_root: &str, s: &SymbolSpan) -> Option<String> {
    let path = lcw_core::resolve_source_path(repo_root, &s.file);
    let snip = lcw_core::read_snippet(
        &path.to_string_lossy(),
        s.start_line,
        s.end_line,
        0,
        s.line_count().max(1),
    )?;
    let body: Vec<&str> = snip
        .lines
        .iter()
        .filter(|l| l.number >= snip.highlight_start && l.number <= snip.highlight_end)
        .map(|l| l.text.as_str())
        .collect();
    Some(body.join("\n"))
}

/// `flow`: shortest call path(s), each an ordered list of hops; with
/// `snippets`, each hop carries its source. `repo_root` resolves relative
/// paths for snippet reads.
pub fn flow(
    graph: &CodeGraph,
    fp: &FlowPaths,
    snippets: bool,
    repo_root: &str,
    limit: usize,
) -> Value {
    let hop = |id: NodeId| -> Value {
        let mut v = symbol(graph, id);
        if snippets {
            if let Some(code) = lcw_query::span(graph, id).and_then(|s| span_code(repo_root, &s)) {
                v["code"] = json!(code);
            }
        }
        v
    };
    let paths: Vec<Value> = fp
        .paths
        .iter()
        .map(|p| Value::Array(p.iter().map(|&id| hop(id)).collect()))
        .collect();
    let mut o = Map::new();
    o.insert("source".into(), json!(graph.node(fp.source).qualified_name));
    o.insert("target".into(), json!(graph.node(fp.target).qualified_name));
    o.insert("hops".into(), json!(fp.hops()));
    put_list(&mut o, "paths", paths, limit);
    with_schema(Value::Object(o))
}

/// Outline rendering knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutlineOpts {
    /// Collapse scopes at this depth (0 = crates): counts only, no children.
    pub max_depth: Option<u32>,
    pub scopes_only: bool,
}

fn outline_node(graph: &CodeGraph, n: &OutlineNode, opts: &OutlineOpts, limit: usize) -> Value {
    let mut o = Map::new();
    o.insert("name".into(), json!(n.label));
    if let (false, Some(id)) = (n.is_scope(), n.node) {
        o.insert("at".into(), json!(at(graph, id)));
        o.insert("cc".into(), json!(n.cyclomatic));
        if let Some(e) = n.entry {
            o.insert("entry".into(), json!(e.as_str()));
        }
        return Value::Object(o);
    }
    o.insert("functions".into(), json!(n.functions));
    if n.entries > 0 {
        o.insert("entries".into(), json!(n.entries));
    }
    if opts.max_depth.is_some_and(|d| n.depth >= d) {
        o.insert("collapsed".into(), json!(true));
        return Value::Object(o);
    }
    let kids: Vec<Value> = n
        .children
        .iter()
        .filter(|c| !opts.scopes_only || c.is_scope())
        .map(|c| outline_node(graph, c, opts, limit))
        .collect();
    if !kids.is_empty() {
        put_list(&mut o, "children", kids, limit);
    }
    Value::Object(o)
}

/// `outline`: crate ▸ module ▸ type ▸ function; `limit` caps children per scope.
pub fn outline(graph: &CodeGraph, outline: &Outline, opts: &OutlineOpts, limit: usize) -> Value {
    let mut o = Map::new();
    o.insert("functions".into(), json!(outline.functions));
    o.insert("scopes".into(), json!(outline.scopes));
    let roots = outline
        .roots
        .iter()
        .map(|r| outline_node(graph, r, opts, limit))
        .collect();
    put_list(&mut o, "roots", roots, limit);
    with_schema(Value::Object(o))
}

/// Entry points ranked for display: by kind, then by reach (most code driven
/// first), then name; capped at `per_kind` per kind. `reach(id)` is computed
/// for `main`s and exports always and for the rest when `all_reach`.
/// Returns the kept rows and how many were cut.
pub fn rank_entries(
    graph: &CodeGraph,
    entries: Vec<EntryPoint>,
    all_reach: bool,
    per_kind: usize,
) -> (Vec<(EntryPoint, Option<usize>)>, usize) {
    let mut rows: Vec<(EntryPoint, Option<usize>)> = entries
        .into_iter()
        .map(|e| {
            let always = matches!(e.kind, EntryKind::Main | EntryKind::Exported);
            let reach = (all_reach || always)
                .then(|| lcw_query::reach_count(graph, e.id, Direction::Callees));
            (e, reach)
        })
        .collect();
    rows.sort_by(|(ea, ra), (eb, rb)| {
        ea.kind
            .cmp(&eb.kind)
            .then_with(|| rb.cmp(ra))
            .then_with(|| {
                graph
                    .node(ea.id)
                    .qualified_name
                    .cmp(&graph.node(eb.id).qualified_name)
            })
    });
    let total = rows.len();
    let mut per = std::collections::HashMap::new();
    rows.retain(|(e, _)| {
        let n = per.entry(e.kind).or_insert(0usize);
        *n += 1;
        *n <= per_kind
    });
    let cut = total - rows.len();
    (rows, cut)
}

/// `entries`: ranked entry points (see [`rank_entries`]); `more` is how many
/// the per-kind cap dropped.
pub fn entries(graph: &CodeGraph, rows: &[(EntryPoint, Option<usize>)], more: usize) -> Value {
    let list: Vec<Value> = rows
        .iter()
        .map(|(e, reach)| {
            let mut v = symbol(graph, e.id);
            v["entry"] = json!(e.kind.as_str());
            if let Some(r) = reach {
                v["reach"] = json!(r);
            }
            v
        })
        .collect();
    let mut o = Map::new();
    o.insert("entries".into(), Value::Array(list));
    if more > 0 {
        o.insert("entries_more".into(), json!(more));
    }
    with_schema(Value::Object(o))
}

/// `search_symbol`: symbols matching `query` (substring ranking), or fuzzy
/// suggestions when nothing contains it (`"fuzzy": true`).
pub fn search(graph: &CodeGraph, query: &str, limit: usize) -> Value {
    let mut hits = lcw_query::resolve(graph, query);
    let fuzzy = hits.is_empty();
    if fuzzy {
        hits = lcw_query::suggest(graph, query, limit.min(crate::MAX_CANDIDATES * 4));
    }
    let mut o = Map::new();
    o.insert("query".into(), json!(query));
    if fuzzy {
        o.insert("fuzzy".into(), json!(true));
    }
    let list = hits.iter().map(|&id| symbol(graph, id)).collect();
    put_list(&mut o, "matches", list, limit);
    with_schema(Value::Object(o))
}

/// The error object for a symbol that is ambiguous or unknown:
/// `{"schema":1,"error":"ambiguous"|"not_found","query":…,"candidates":[…]}`.
/// `None` for [`Lookup::Found`].
pub fn lookup_error(graph: &CodeGraph, query: &str, lookup: &Lookup) -> Option<Value> {
    let (error, ids) = match lookup {
        Lookup::Found(_) => return None,
        Lookup::Ambiguous(ids) => ("ambiguous", ids),
        Lookup::NotFound(ids) => ("not_found", ids),
    };
    Some(with_schema(json!({
        "error": error,
        "query": query,
        "candidates": ids.iter().map(|&id| symbol(graph, id)).collect::<Vec<_>>(),
    })))
}

/// A plain error object: `{"schema":1,"error":"…","message":"…"}`.
pub fn error(kind: &str, message: &str) -> Value {
    with_schema(json!({ "error": kind, "message": message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeFlags, NodeStats};
    use lcw_query::{entry_points, CallTreeOptions};

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

    /// main -> run -> step (x2), main -> leaf{0..4}
    fn graph() -> CodeGraph {
        let mut g = CodeGraph::new();
        let main = def(&mut g, "app::main", 1);
        let run = def(&mut g, "app::core::run", 10);
        let step = def(&mut g, "app::core::step", 20);
        let f = g.intern_file("src/lib.rs");
        let edge = |l| Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, l, 0, l, 5));
        g.add_edge(main, run, edge(2));
        g.add_edge(run, step, edge(11));
        g.add_edge(run, step, edge(12));
        for i in 0..5 {
            let leaf = def(&mut g, &format!("app::leaf{i}"), 30 + i * 3);
            g.add_edge(main, leaf, edge(3 + i));
        }
        g
    }

    #[test]
    fn every_view_has_schema_and_string_locations() {
        let g = graph();
        let step = g.node_by_qualified("app::core::step").unwrap();
        let v = explain(&g, step, 20).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["at"], "src/lib.rs:20");
        assert_eq!(v["span"], "src/lib.rs:20-22");
        assert_eq!(v["callers"][0]["qualified_name"], "app::core::run");
        assert_eq!(v["callers"][0]["count"], 2);
        assert_eq!(v["callers"][0]["site"], "src/lib.rs:11");
        assert!(v.get("callers_more").is_none());

        let main = g.node_by_qualified("app::main").unwrap();
        let cut = explain(&g, main, 2).unwrap();
        assert_eq!(cut["callees"].as_array().unwrap().len(), 2);
        assert_eq!(cut["callees_more"], 4);

        let rows = rank_entries(&g, entry_points(&g), false, 40).0;
        let e = entries(&g, &rows, 0);
        assert_eq!(e["schema"], 1);
        assert_eq!(e["entries"][0]["qualified_name"], "app::main");
        assert_eq!(e["entries"][0]["reach"], 7);
    }

    #[test]
    fn call_tree_json_carries_sites_and_truncation() {
        let g = graph();
        let main = g.node_by_qualified("app::main").unwrap();
        let opts = CallTreeOptions {
            max_children: 3,
            ..Default::default()
        };
        let tree = lcw_query::call_tree(&g, main, &opts);
        let v = call_tree(&g, &tree, Direction::Callees, 3);
        assert_eq!(v["schema"], 1);
        assert_eq!(v["direction"], "callees");
        let root = &v["tree"];
        assert!(root.get("site").is_none());
        assert_eq!(root["children"][0]["qualified_name"], "app::core::run");
        assert_eq!(root["children"][0]["site"], "src/lib.rs:2");
        assert_eq!(
            root["children"][0]["children"][0]["qualified_name"],
            "app::core::step"
        );
        assert_eq!(root["children_more"], 3);
    }

    #[test]
    fn impact_and_lookup_errors() {
        let mut g = graph();
        let step = g.node_by_qualified("app::core::step").unwrap();
        let imp = lcw_query::impact(&g, step, 3).unwrap();
        let v = impact(&g, &imp, 10);
        assert_eq!(v["total_callers"], 2);
        assert_eq!(v["by_depth"], json!([1, 1]));
        assert_eq!(v["callers"][1]["via"], "app::core::run");
        assert_eq!(v["callers"][1]["site"], "src/lib.rs:2");
        assert_eq!(v["entry_points"][0]["entry"], "main");
        let short = impact(&g, &imp, 1);
        assert_eq!(short["callers_more"], 1);

        def(&mut g, "app::other::run", 90);
        let l = lcw_query::lookup(&g, "run", 8);
        let err = lookup_error(&g, "run", &l).unwrap();
        assert_eq!(err["error"], "ambiguous");
        assert_eq!(err["candidates"].as_array().unwrap().len(), 2);
        let l = lcw_query::lookup(&g, "stpe", 8);
        let err = lookup_error(&g, "stpe", &l).unwrap();
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["candidates"][0]["qualified_name"], "app::core::step");

        let s = search(&g, "leaf", 2);
        assert_eq!(s["matches"].as_array().unwrap().len(), 2);
        assert_eq!(s["matches_more"], 3);
    }

    #[test]
    fn span_json_and_code_read_exact_lines() {
        let dir = std::env::temp_dir().join(format!("lcw-agent-span-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "// head\n/// doc\nfn a() {\n    1;\n}\n// tail\n",
        )
        .unwrap();
        let s = SymbolSpan {
            id: NodeId(0),
            qualified_name: "m::a".into(),
            kind: NodeKind::Function,
            file: "src/lib.rs".into(),
            start_line: 2,
            end_line: 5,
        };
        let code = span_code(&dir.to_string_lossy(), &s).unwrap();
        assert_eq!(code, "/// doc\nfn a() {\n    1;\n}");
        let v = span(&s, Some(&code));
        assert_eq!(v["span"], "src/lib.rs:2-5");
        assert_eq!(v["lines"], 4);
        assert_eq!(v["code"], code);
        let _ = std::fs::remove_dir_all(dir);
    }
}
