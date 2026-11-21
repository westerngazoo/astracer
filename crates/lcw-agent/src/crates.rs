//! `lcw crates` / MCP `crates`: the crate-level view of a Cargo workspace,
//! rendered as schema-1 JSON or a compact text table.

use std::fmt::Write as _;
use std::path::Path;

use lcw_core::{CodeGraph, CrateGraph, CrateIdx};
use lcw_query::{CrateUsage, CrateView};
use serde_json::{json, Map, Value};

use crate::json::with_schema;
use crate::Repo;

/// The crate view of a loaded repo; `None` when it has no Cargo workspace.
pub fn crate_view(repo: &Repo) -> Option<CrateView> {
    crate_view_of(repo, &repo.graph)
}

/// [`crate_view`] over another graph of the same repo (a filtered one).
pub fn crate_view_of(repo: &Repo, graph: &CodeGraph) -> Option<CrateView> {
    let crates = repo.crates.as_ref().filter(|c| !c.is_empty())?;
    let (file_crate, file_lines) = file_tables(&repo.root, crates, graph);
    Some(lcw_query::crate_view(
        crates,
        graph,
        &file_crate,
        &file_lines,
    ))
}

/// Per graph file: its crate and line count (graph paths may be relative to
/// the repo root or absolute).
pub(crate) fn file_tables(
    root: &Path,
    crates: &CrateGraph,
    graph: &CodeGraph,
) -> (Vec<Option<CrateIdx>>, Vec<u64>) {
    let base = crate::repo_dir(root);
    graph
        .files()
        .map(|(_, p)| {
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            };
            let lines = std::fs::read(&abs)
                .map(|b| bytecount_lines(&b))
                .unwrap_or(0);
            (crates.crate_of(&abs), lines)
        })
        .unzip()
}

fn bytecount_lines(b: &[u8]) -> u64 {
    let n = b.iter().filter(|&&c| c == b'\n').count() as u64;
    n + u64::from(!b.is_empty() && !b.ends_with(b"\n"))
}

fn names(v: &CrateView, ids: &[CrateIdx]) -> Vec<String> {
    ids.iter().map(|&i| v.name(i).to_string()).collect()
}

/// Rows sorted for display: shipped first, then by size.
fn ordered(v: &CrateView) -> Vec<&lcw_query::CrateRow> {
    let mut rows: Vec<_> = v.crates.iter().collect();
    rows.sort_by(|a, b| {
        (a.usage != CrateUsage::Shipped)
            .cmp(&(b.usage != CrateUsage::Shipped))
            .then(b.lines.cmp(&a.lines))
            .then(a.name.cmp(&b.name))
    });
    rows
}

/// Schema-1 JSON; `limit` caps the crate and call lists.
pub fn crates_json(v: &CrateView, limit: usize) -> Value {
    let rows: Vec<Value> = ordered(v)
        .into_iter()
        .map(|c| {
            let mut o = json!({
                "name": c.name,
                "usage": c.usage.as_str(),
                "files": c.files,
                "lines": c.lines,
                "functions": c.functions,
                "deps": names(v, &c.deps),
                "dependents": c.dependents.len(),
                "rebuild": c.rebuild.len(),
                "depth": c.depth,
                "calls_in": c.calls_in,
                "calls_out": c.calls_out,
            });
            if c.root {
                o["root"] = json!(true);
            }
            if c.test_functions > 0 {
                o["test_functions"] = json!(c.test_functions);
            }
            if !c.dev_deps.is_empty() {
                o["dev_deps"] = json!(names(v, &c.dev_deps));
            }
            if !c.member {
                o["member"] = json!(false);
            }
            if v.has_default_members {
                o["default_member"] = json!(c.default_member);
            }
            o
        })
        .collect();
    let calls: Vec<Value> = v
        .calls
        .iter()
        .map(|&(a, b, pairs, sites)| {
            json!({ "from": v.name(a), "to": v.name(b), "pairs": pairs, "sites": sites })
        })
        .collect();
    let mut o = Map::new();
    o.insert("critical_path".into(), json!(names(v, &v.critical_path)));
    let not_shipped: Vec<Value> = ordered(v)
        .into_iter()
        .filter(|c| c.usage != CrateUsage::Shipped)
        .map(|c| json!({ "name": c.name, "usage": c.usage.as_str() }))
        .collect();
    if !not_shipped.is_empty() {
        o.insert("not_shipped".into(), Value::Array(not_shipped));
    }
    crate::json::put_list(&mut o, "crates", rows, limit);
    crate::json::put_list(&mut o, "calls", calls, limit);
    with_schema(Value::Object(o))
}

/// Compact text table.
pub fn crates_text(v: &CrateView, limit: usize) -> String {
    let mut out = String::new();
    let rows = ordered(v);
    let width = rows.iter().map(|c| c.name.len()).max().unwrap_or(5).max(5);
    let _ = writeln!(
        out,
        "{:<width$}  {:>6}  {:>5}  {:>5}  {:>7}  {:>5}  {:>7}  usage",
        "crate", "lines", "fns", "depth", "rebuild", "in", "out"
    );
    for c in rows.iter().take(limit) {
        let usage = match (c.usage, c.root) {
            (CrateUsage::Shipped, true) => "shipped (root)",
            (u, _) => u.as_str(),
        };
        let _ = writeln!(
            out,
            "{:<width$}  {:>6}  {:>5}  {:>5}  {:>7}  {:>5}  {:>7}  {usage}",
            c.name,
            c.lines,
            c.functions,
            c.depth,
            c.rebuild.len(),
            c.calls_in,
            c.calls_out,
        );
    }
    if rows.len() > limit {
        let _ = writeln!(out, "... +{} more", rows.len() - limit);
    }
    if v.critical_path.len() > 1 {
        let _ = writeln!(
            out,
            "\ncritical path: {}",
            names(v, &v.critical_path).join(" -> ")
        );
    }
    if !v.calls.is_empty() {
        let _ = writeln!(out, "\ncross-crate calls (distinct caller->callee pairs):");
        for &(a, b, pairs, sites) in v.calls.iter().take(limit) {
            let _ = writeln!(
                out,
                "  {:>5}  {} -> {}  ({sites} sites)",
                pairs,
                v.name(a),
                v.name(b)
            );
        }
        if v.calls.len() > limit {
            let _ = writeln!(out, "  ... +{} more", v.calls.len() - limit);
        }
    }
    let idle: Vec<String> = rows
        .iter()
        .filter(|c| c.usage != CrateUsage::Shipped)
        .map(|c| format!("{} ({})", c.name, c.usage.as_str()))
        .collect();
    if !idle.is_empty() {
        let _ = writeln!(
            out,
            "\nnot built by any shipped binary: {}",
            idle.join(", ")
        );
    }
    out
}
