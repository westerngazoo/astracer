//! `lcw advise` / MCP `advise`: ranked, located improvement findings.
//!
//! Joins the Layer 2 diagnostics (async blocking, N+1, clones, cycles) with
//! graph and workspace facts (I/O hot paths, god types, oversized files,
//! crate rebuild blast radius, crates nothing ships, missing timeouts and
//! limits) into findings with a priority (P0 > P1 > P2), a dimension, a
//! `file:line`, evidence numbers, why it matters and a suggested fix.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use lcw_analysis::perf::{blocking_levels, Blocking};
use lcw_core::{AnalysisReport, CodeGraph, Diagnostic, NodeId, NodeKind, Severity};
use lcw_query::{CrateUsage, CrateView};
use serde_json::{json, Map, Value};

use crate::json::{at, put_list, site, with_schema};
use crate::Repo;

/// What a finding improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dimension {
    Perf,
    Composability,
    Incrementality,
    Scalability,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::Perf => "perf",
            Dimension::Composability => "composability",
            Dimension::Incrementality => "incrementality",
            Dimension::Scalability => "scalability",
        }
    }

    pub fn parse(s: &str) -> Option<Option<Dimension>> {
        Some(match s {
            "all" => None,
            "perf" | "performance" => Some(Dimension::Perf),
            "composability" => Some(Dimension::Composability),
            "incrementality" => Some(Dimension::Incrementality),
            "scalability" => Some(Dimension::Scalability),
            _ => return None,
        })
    }
}

/// One ranked finding.
#[derive(Debug, Clone)]
pub struct Finding {
    /// 0 = P0 (fix first) ..= 2.
    pub priority: u8,
    pub dimension: Dimension,
    /// Stable machine name (`async_blocking`, `god_type`, ...).
    pub kind: &'static str,
    pub title: String,
    /// `file:line` to start at.
    pub at: Option<String>,
    pub symbol: Option<String>,
    pub evidence: Vec<(&'static str, u64)>,
    /// Further `file:line symbol` locations (other handlers, call sites).
    pub sites: Vec<String>,
    pub why: String,
    pub fix: String,
    /// Tie-break within a priority: bigger first.
    pub weight: u64,
}

/// Thresholds; defaults suit a mid-size workspace.
#[derive(Debug, Clone, Copy)]
pub struct AdviseOptions {
    /// Distinct callers that make an I/O-reaching function a hot path.
    pub hot_fan_in: usize,
    /// Methods on one type that make it a god type.
    pub god_methods: usize,
    /// Summed method lines that make a type a god type.
    pub god_lines: u64,
    /// Lines that make one source file an oversized compilation unit.
    pub big_file_lines: u64,
    /// Sites listed per aggregated finding.
    pub max_sites: usize,
}

impl Default for AdviseOptions {
    fn default() -> Self {
        AdviseOptions {
            hot_fan_in: 8,
            god_methods: 40,
            god_lines: 1500,
            big_file_lines: 2000,
            max_sites: 8,
        }
    }
}

/// Re-run the lenses on `graph` (cached graphs carry no diagnostics, and a
/// filtered graph needs its own) and return its findings for `dim`.
pub fn advise_repo(repo: &Repo, graph: &CodeGraph, dim: Option<Dimension>) -> Vec<Finding> {
    let report = repo.engine().report_for(graph.clone());
    let view = crate::crates::crate_view_of(repo, graph);
    filter(
        advise(
            repo,
            graph,
            &report,
            view.as_ref(),
            &AdviseOptions::default(),
        ),
        dim,
    )
}

/// All findings for `repo` over `graph` (the repo graph, or a filtered one),
/// best first. `report` must hold the lens diagnostics for `graph`.
pub fn advise(
    repo: &Repo,
    graph: &CodeGraph,
    report: &AnalysisReport,
    crates: Option<&CrateView>,
    opts: &AdviseOptions,
) -> Vec<Finding> {
    let lines = file_lines(repo, graph);
    let mut out = Vec::new();
    async_blocking(graph, &report.diagnostics, crates, repo, opts, &mut out);
    per_diagnostic(graph, &report.diagnostics, opts, &mut out);
    hot_paths(repo, graph, opts, &mut out);
    god_types(graph, &lines, opts, &mut out);
    big_files(graph, &lines, opts, &mut out);
    if let Some(v) = crates {
        crate_findings(v, &mut out);
    }
    limits(graph, &mut out);
    out.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.weight.cmp(&a.weight))
            .then(a.title.cmp(&b.title))
    });
    out
}

fn file_lines(repo: &Repo, graph: &CodeGraph) -> Vec<u64> {
    let base = crate::repo_dir(&repo.root);
    graph
        .files()
        .map(|(_, p)| {
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                base.join(p)
            };
            std::fs::read(abs).map_or(0, |b| {
                let n = b.iter().filter(|&&c| c == b'\n').count() as u64;
                n + u64::from(!b.is_empty() && !b.ends_with(b"\n"))
            })
        })
        .collect()
}

fn qname(graph: &CodeGraph, id: NodeId) -> String {
    graph.node(id).qualified_name.clone()
}

fn diag_at(graph: &CodeGraph, d: &Diagnostic) -> Option<String> {
    d.site
        .and_then(|s| site(graph, s))
        .or_else(|| d.node.map(|n| at(graph, n)))
}

fn top_crate(q: &str) -> &str {
    q.split("::").next().unwrap_or(q)
}

/// P0: async fns blocking the executor, one finding per crate.
fn async_blocking(
    graph: &CodeGraph,
    diags: &[Diagnostic],
    crates: Option<&CrateView>,
    repo: &Repo,
    opts: &AdviseOptions,
    out: &mut Vec<Finding>,
) {
    let mut by_crate: BTreeMap<String, Vec<&Diagnostic>> = BTreeMap::new();
    for d in diags.iter().filter(|d| d.code == "async_blocking") {
        let Some(n) = d.node else { continue };
        let krate = crate_name(repo, graph, n, crates)
            .unwrap_or_else(|| top_crate(&graph.node(n).qualified_name).to_string());
        by_crate.entry(krate).or_default().push(d);
    }
    for (krate, mut ds) in by_crate {
        ds.sort_by_key(|d| (std::cmp::Reverse(d.evidence("blocking_calls")), d.node));
        let mut sinks: BTreeMap<String, u64> = BTreeMap::new();
        for d in &ds {
            if let Some(&last) = d.related.last() {
                *sinks
                    .entry(short(&graph.node(last).qualified_name))
                    .or_default() += 1;
            }
        }
        let mut sink_list: Vec<_> = sinks.into_iter().collect();
        sink_list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let sink_text = sink_list
            .iter()
            .take(3)
            .map(|(s, n)| format!("{s} x{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let n = ds.len() as u64;
        let calls: u64 = ds.iter().filter_map(|d| d.evidence("blocking_calls")).sum();
        let first = ds[0];
        let sites = ds
            .iter()
            .skip(1)
            .take(opts.max_sites)
            .filter_map(|d| {
                Some(format!(
                    "{} {}",
                    diag_at(graph, d)?,
                    short(&qname(graph, d.node?))
                ))
            })
            .collect();
        out.push(Finding {
            priority: 0,
            dimension: Dimension::Perf,
            kind: "async_blocking",
            title: format!(
                "{krate}: {n} async fn(s) run blocking calls on the executor ({sink_text})"
            ),
            at: diag_at(graph, first),
            symbol: first.node.map(|id| qname(graph, id)),
            evidence: vec![("async_fns", n), ("blocking_call_sites", calls)],
            sites,
            why: "Each blocking call parks a runtime worker thread; under load every other \
                  request on that worker waits, so latency grows with the slowest disk or \
                  database operation."
                .into(),
            fix: "Run the store / file work in `tokio::task::spawn_blocking` (one wrapper per \
                  handler, or make the store API async and do the offload inside it), or move \
                  to an async client."
                .into(),
            weight: n * 100 + calls,
        });
    }
}

fn crate_name(
    repo: &Repo,
    graph: &CodeGraph,
    id: NodeId,
    view: Option<&CrateView>,
) -> Option<String> {
    let crates = repo.crates.as_ref()?;
    let view = view?;
    let path = graph.file_path(graph.node(id).span.file())?;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::repo_dir(&repo.root).join(path)
    };
    crates.crate_of(&abs).map(|c| view.name(c).to_string())
}

fn short(q: &str) -> String {
    let parts: Vec<&str> = q.rsplitn(3, "::").collect();
    match parts.as_slice() {
        [a, b, _] | [a, b] => format!("{b}::{a}"),
        _ => q.to_string(),
    }
}

/// N+1, large clones and real cycles: one finding per caller / cycle.
fn per_diagnostic(
    graph: &CodeGraph,
    diags: &[Diagnostic],
    opts: &AdviseOptions,
    out: &mut Vec<Finding>,
) {
    let mut n1: BTreeMap<NodeId, Vec<&Diagnostic>> = BTreeMap::new();
    for d in diags.iter().filter(|d| d.code == "n_plus_one") {
        if let Some(n) = d.node {
            n1.entry(n).or_default().push(d);
        }
    }
    for (caller, ds) in n1 {
        let callees: Vec<String> = ds
            .iter()
            .filter_map(|d| d.related.first().map(|&t| short(&qname(graph, t))))
            .collect();
        let sites = ds
            .iter()
            .skip(1)
            .take(opts.max_sites)
            .filter_map(|d| diag_at(graph, d))
            .collect();
        out.push(Finding {
            priority: 1,
            dimension: Dimension::Perf,
            kind: "n_plus_one",
            title: format!(
                "N+1: `{}` does I/O per loop iteration ({})",
                short(&qname(graph, caller)),
                callees.join(", ")
            ),
            at: diag_at(graph, ds[0]),
            symbol: Some(qname(graph, caller)),
            evidence: vec![("io_calls_in_loop", ds.len() as u64)],
            sites,
            why: "One store / file round trip per element: cost grows linearly with the \
                  collection, and each call repeats transaction setup."
                .into(),
            fix: "Fetch the whole batch before the loop (one range scan or a multi-get in a \
                  single read transaction), then loop over memory."
                .into(),
            weight: 50 + ds.len() as u64,
        });
    }
    for d in diags.iter().filter(|d| d.code == "clone_in_loop") {
        let Some(n) = d.node else { continue };
        out.push(Finding {
            priority: 2,
            dimension: Dimension::Perf,
            kind: "clone_in_loop",
            title: d.message.clone(),
            at: diag_at(graph, d),
            symbol: Some(qname(graph, n)),
            evidence: vec![],
            sites: vec![],
            why: "A deep copy per iteration multiplies allocations by the loop length.".into(),
            fix: "Borrow, clone once before the loop, or share behind an `Arc`.".into(),
            weight: 1,
        });
    }
    for d in diags.iter().filter(|d| d.code == "cycle") {
        if d.severity <= Severity::Low {
            continue;
        }
        let Some(n) = d.node else { continue };
        let size = d.evidence("size").unwrap_or(0);
        out.push(Finding {
            priority: if d.severity >= Severity::High { 1 } else { 2 },
            dimension: Dimension::Composability,
            kind: "cycle",
            title: d.message.clone(),
            at: diag_at(graph, d),
            symbol: Some(qname(graph, n)),
            evidence: vec![
                ("size", size),
                ("scopes", d.evidence("scopes").unwrap_or(0)),
            ],
            sites: d
                .related
                .iter()
                .take(opts.max_sites)
                .map(|&r| format!("{} {}", at(graph, r), short(&qname(graph, r))))
                .collect(),
            why: "Functions in a cycle across modules cannot be understood, tested or moved \
                  independently."
                .into(),
            fix: "Cut one edge: pass a callback / trait object, or move the shared part into \
                  a module both sides depend on."
                .into(),
            weight: size,
        });
    }
}

/// P1: functions many callers use that reach blocking I/O.
fn hot_paths(repo: &Repo, graph: &CodeGraph, opts: &AdviseOptions, out: &mut Vec<Finding>) {
    let blocking = Blocking::from_config(&repo.config.lenses);
    let levels = blocking_levels(graph, &blocking, repo.config.lenses.blocking_depth, true);
    let mut hot: Vec<(NodeId, usize)> = graph
        .node_ids()
        .filter(|&id| {
            let n = graph.node(id);
            n.kind != NodeKind::External && levels[id.0 as usize].is_some()
        })
        .map(|id| {
            let callers: HashSet<NodeId> = graph.neighbors_in(id).filter(|&c| c != id).collect();
            (id, callers.len())
        })
        .filter(|&(_, k)| k >= opts.hot_fan_in)
        .collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for &(id, callers) in hot.iter().take(5) {
        let sites: u64 = graph.edges_in(id).map(|(_, e)| u64::from(e.count)).sum();
        let chain = lcw_analysis::perf::blocking_chain(&levels, id);
        let sink = chain.last().map(|&s| qname(graph, s)).unwrap_or_default();
        out.push(Finding {
            priority: 1,
            dimension: Dimension::Perf,
            kind: "hot_io_path",
            title: format!(
                "Hot I/O path: `{}` has {callers} callers and reaches `{sink}`",
                short(&qname(graph, id))
            ),
            at: Some(at(graph, id)),
            symbol: Some(qname(graph, id)),
            evidence: vec![("callers", callers as u64), ("call_sites", sites)],
            sites: vec![],
            why: "Every request path through it pays a store / disk round trip; it is the \
                  first place where caching or batching pays off across the codebase."
                .into(),
            fix: "Cache the result (per request, or an in-memory map invalidated on write), \
                  or let callers pass in what they already loaded."
                .into(),
            weight: callers as u64,
        });
    }
}

/// P1: types with too many methods / lines in one place.
fn god_types(graph: &CodeGraph, lines: &[u64], opts: &AdviseOptions, out: &mut Vec<Finding>) {
    struct Ty {
        methods: Vec<NodeId>,
        loc: u64,
        files: HashMap<u32, usize>,
    }
    let mut types: BTreeMap<&str, Ty> = BTreeMap::new();
    for id in graph.node_ids() {
        let n = graph.node(id);
        if n.kind != NodeKind::Method || n.flags.is_test || n.flags.is_example {
            continue;
        }
        let t = types.entry(n.module_path.as_str()).or_insert(Ty {
            methods: Vec::new(),
            loc: 0,
            files: HashMap::new(),
        });
        t.methods.push(id);
        t.loc += u64::from(n.stats.lines_of_code);
        *t.files.entry(n.span.file).or_default() += 1;
    }
    for (path, t) in types {
        let m = t.methods.len();
        if m < opts.god_methods && t.loc < opts.god_lines {
            continue;
        }
        let Some((&file, _)) = t
            .files
            .iter()
            .max_by_key(|(f, k)| (**k, std::cmp::Reverse(**f)))
        else {
            continue;
        };
        let file_len = lines.get(file as usize).copied().unwrap_or(0);
        let callers: HashSet<NodeId> = t
            .methods
            .iter()
            .flat_map(|&id| graph.neighbors_in(id))
            .filter(|c| !t.methods.contains(c))
            .collect();
        let file_name = graph
            .file_path(lcw_core::FileId(file))
            .map(lcw_core::display_path)
            .unwrap_or_default();
        let anchor = t
            .methods
            .iter()
            .copied()
            .min_by_key(|&id| graph.node(id).span.start_line)
            .expect("non-empty");
        out.push(Finding {
            priority: 1,
            dimension: Dimension::Composability,
            kind: "god_type",
            title: format!(
                "God type `{}`: {m} methods, {} method lines, {file_len}-line {file_name}",
                short(path),
                t.loc
            ),
            at: Some(at(graph, anchor)),
            symbol: Some(path.to_string()),
            evidence: vec![
                ("methods", m as u64),
                ("method_lines", t.loc),
                ("file_lines", file_len),
                ("outside_callers", callers.len() as u64),
            ],
            sites: vec![],
            why: "Every caller depends on the whole type, so any change to it touches (and \
                  recompiles) all of them, and it cannot be split across crates or tested in \
                  parts."
                .into(),
            fix: "Split by responsibility into smaller types or modules (for a store: one \
                  repository per aggregate, e.g. courses / concepts / progress), each in its \
                  own file, behind narrow traits that callers depend on."
                .into(),
            weight: m as u64,
        });
    }
}

/// P2: single files large enough to dominate recompiles and reviews.
fn big_files(graph: &CodeGraph, lines: &[u64], opts: &AdviseOptions, out: &mut Vec<Finding>) {
    let mut fns_per_file: HashMap<u32, usize> = HashMap::new();
    for n in graph.nodes() {
        if n.kind != NodeKind::External {
            *fns_per_file.entry(n.span.file).or_default() += 1;
        }
    }
    for (fid, path) in graph.files() {
        let len = lines.get(fid.0 as usize).copied().unwrap_or(0);
        if len < opts.big_file_lines {
            continue;
        }
        let shown = lcw_core::display_path(path);
        out.push(Finding {
            priority: 2,
            dimension: Dimension::Incrementality,
            kind: "big_file",
            title: format!("Oversized file: {shown} has {len} lines"),
            at: Some(format!("{shown}:1")),
            symbol: None,
            evidence: vec![
                ("lines", len),
                (
                    "functions",
                    fns_per_file.get(&fid.0).copied().unwrap_or(0) as u64,
                ),
            ],
            sites: vec![],
            why: "One huge file is one unit of change: every edit re-parses and re-checks all \
                  of it, and parallel work on it conflicts."
                .into(),
            fix: "Split it into modules along its sections (tests into their own file too).".into(),
            weight: len,
        });
    }
}

/// Crate rebuild blast radius and crates nothing ships.
fn crate_findings(v: &CrateView, out: &mut Vec<Finding>) {
    for c in &v.crates {
        let rebuild_lines: u64 = c
            .rebuild
            .iter()
            .filter_map(|&r| v.row(r))
            .map(|r| r.lines)
            .sum();
        if c.rebuild.len() >= 2 && c.lines >= 2000 {
            out.push(Finding {
                priority: if c.lines >= 4000 { 1 } else { 2 },
                dimension: Dimension::Incrementality,
                kind: "crate_blast_radius",
                title: format!(
                    "Changing `{}` ({} lines) recompiles {} crates ({} lines): {}",
                    c.name,
                    c.lines,
                    c.rebuild.len(),
                    rebuild_lines,
                    c.rebuild
                        .iter()
                        .map(|&r| v.name(r))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                at: None,
                symbol: Some(c.name.clone()),
                evidence: vec![
                    ("lines", c.lines),
                    ("rebuild_crates", c.rebuild.len() as u64),
                    ("rebuild_lines", rebuild_lines),
                    ("calls_in", c.calls_in as u64),
                ],
                sites: vec![],
                why: "A low-level crate that changes often forces every dependent to rebuild; \
                      the bigger it is, the more unrelated edits trigger that."
                    .into(),
                fix: "Split the stable core (types, traits) from the frequently edited parts \
                      so dependents only link the core."
                    .into(),
                weight: c.lines * (c.rebuild.len() as u64 + 1),
            });
        }
        if c.usage != CrateUsage::Shipped {
            let what = match c.usage {
                CrateUsage::DevOnly => "is only reached from examples, tests or dev-deps",
                _ => "is not built by anything in the workspace",
            };
            out.push(Finding {
                priority: 2,
                dimension: Dimension::Incrementality,
                kind: "crate_not_shipped",
                title: format!(
                    "`{}` ({} lines) {what}; no shipped binary uses it",
                    c.name, c.lines
                ),
                at: None,
                symbol: Some(c.name.clone()),
                evidence: vec![("lines", c.lines), ("functions", c.functions as u64)],
                sites: vec![],
                why: "`cargo build` / `cargo test --workspace` still compile it, so it costs \
                      build time without shipping."
                    .into(),
                fix: "Leave it out of `default-members` (or move it to an `examples/` / \
                      experimental workspace) until a shipped crate depends on it."
                    .into(),
                weight: c.lines,
            });
        }
    }
}

/// P2 scalability heuristics over called library paths: HTTP clients
/// without timeouts, servers without timeout / body / concurrency limits,
/// unbounded channels.
fn limits(graph: &CodeGraph, out: &mut Vec<Finding>) {
    let mut called: HashMap<&str, NodeId> = HashMap::new();
    for id in graph.node_ids() {
        let n = graph.node(id);
        if n.kind == NodeKind::External {
            called.insert(n.qualified_name.as_str(), id);
        }
    }
    let any = |pred: &dyn Fn(&str) -> bool| called.keys().any(|q| pred(q));
    let first_caller = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
        let mut hits: Vec<_> = called
            .iter()
            .filter(|(q, _)| pred(q))
            .flat_map(|(_, &id)| graph.edges_in(id).map(|(c, e)| (c, e.call_site)))
            .collect();
        hits.sort_by_key(|(c, s)| (c.0, s.start_line));
        hits.first().and_then(|&(_, s)| site(graph, s))
    };
    let last = |q: &str| q.rsplit("::").next().unwrap_or(q).to_string();

    let http_client = |q: &str| {
        q.ends_with("reqwest::Client::new")
            || q == "Client::builder"
            || q.ends_with("reqwest::Client::builder")
            || q.starts_with("ureq::")
    };
    if any(&http_client)
        && !any(&|q| {
            matches!(
                last(q).as_str(),
                "timeout" | "timeout_read" | "connect_timeout"
            )
        })
    {
        out.push(Finding {
            priority: 2,
            dimension: Dimension::Scalability,
            kind: "no_http_timeout",
            title: "HTTP client built without a timeout".into(),
            at: first_caller(&http_client),
            symbol: None,
            evidence: vec![],
            sites: vec![],
            why: "A peer that stops answering holds the request (and its task, connection and \
                  memory) forever."
                .into(),
            fix: "Build the client with `.timeout(..)` / `.connect_timeout(..)`.".into(),
            weight: 3,
        });
    }

    let server = |q: &str| {
        q.ends_with("axum::serve")
            || q.ends_with("TcpListener::bind")
            || q.ends_with("HttpServer::new")
            || q.ends_with("Server::bind")
    };
    if any(&server) {
        let has = |names: &[&str]| any(&|q| names.iter().any(|n| q.contains(n)));
        let mut missing = Vec::new();
        if !has(&["TimeoutLayer", "timeout"]) {
            missing.push("request timeout");
        }
        if !has(&[
            "DefaultBodyLimit",
            "RequestBodyLimitLayer",
            "body_limit",
            "limit",
        ]) {
            missing.push("body size limit");
        }
        if !has(&[
            "ConcurrencyLimit",
            "RateLimit",
            "load_shed",
            "GovernorLayer",
            "Semaphore",
        ]) {
            missing.push("concurrency / rate limit");
        }
        if !missing.is_empty() {
            out.push(Finding {
                priority: 2,
                dimension: Dimension::Scalability,
                kind: "server_limits",
                title: format!("Server without {}", missing.join(", ")),
                at: first_caller(&server),
                symbol: None,
                evidence: vec![("missing", missing.len() as u64)],
                sites: vec![],
                why: "Without them, slow or abusive clients tie up workers and memory, and a \
                      burst can exhaust the store or file handles."
                    .into(),
                fix: "Add `tower_http` layers: `TimeoutLayer`, `RequestBodyLimitLayer` (or \
                      axum `DefaultBodyLimit`), and a `ConcurrencyLimitLayer` / rate limiter."
                    .into(),
                weight: 2,
            });
        }
    }

    let unbounded = |q: &str| last(q) == "unbounded_channel" || q.ends_with("mpsc::unbounded");
    if any(&unbounded) {
        out.push(Finding {
            priority: 2,
            dimension: Dimension::Scalability,
            kind: "unbounded_channel",
            title: "Unbounded channel".into(),
            at: first_caller(&unbounded),
            symbol: None,
            evidence: vec![],
            sites: vec![],
            why: "A slow consumer lets the queue (and memory) grow without limit.".into(),
            fix: "Use a bounded channel and decide what to do when it is full.".into(),
            weight: 1,
        });
    }
}

fn p(priority: u8) -> &'static str {
    match priority {
        0 => "P0",
        1 => "P1",
        _ => "P2",
    }
}

/// Keep findings of one dimension (`None` = all).
pub fn filter(findings: Vec<Finding>, dim: Option<Dimension>) -> Vec<Finding> {
    match dim {
        None => findings,
        Some(d) => findings.into_iter().filter(|f| f.dimension == d).collect(),
    }
}

/// Schema-1 JSON; `limit` caps the findings list.
pub fn advise_json(findings: &[Finding], excluded_tests: bool, limit: usize) -> Value {
    let mut counts = Map::new();
    for f in findings {
        let e = counts.entry(p(f.priority)).or_insert(json!(0));
        *e = json!(e.as_u64().unwrap_or(0) + 1);
    }
    let list = findings
        .iter()
        .map(|f| {
            let mut o = Map::new();
            o.insert("priority".into(), json!(p(f.priority)));
            o.insert("dimension".into(), json!(f.dimension.as_str()));
            o.insert("kind".into(), json!(f.kind));
            o.insert("title".into(), json!(f.title));
            if let Some(a) = &f.at {
                o.insert("at".into(), json!(a));
            }
            if let Some(s) = &f.symbol {
                o.insert("symbol".into(), json!(s));
            }
            if !f.evidence.is_empty() {
                let ev: Map<String, Value> = f
                    .evidence
                    .iter()
                    .map(|(k, v)| ((*k).into(), json!(v)))
                    .collect();
                o.insert("evidence".into(), Value::Object(ev));
            }
            if !f.sites.is_empty() {
                o.insert("sites".into(), json!(f.sites));
            }
            o.insert("why".into(), json!(f.why));
            o.insert("fix".into(), json!(f.fix));
            Value::Object(o)
        })
        .collect();
    let mut o = Map::new();
    o.insert("counts".into(), Value::Object(counts));
    if excluded_tests {
        o.insert("excluded_tests".into(), json!(true));
    }
    put_list(&mut o, "findings", list, limit);
    with_schema(Value::Object(o))
}

/// Compact text: one block per finding.
pub fn advise_text(findings: &[Finding], limit: usize) -> String {
    let mut out = String::new();
    let count = |k: u8| findings.iter().filter(|f| f.priority == k).count();
    let _ = writeln!(
        out,
        "{} findings: {} P0, {} P1, {} P2",
        findings.len(),
        count(0),
        count(1),
        count(2)
    );
    for f in findings.iter().take(limit) {
        let _ = writeln!(
            out,
            "\n{} {:<14} {}",
            p(f.priority),
            f.dimension.as_str(),
            f.title
        );
        if let Some(a) = &f.at {
            let _ = writeln!(out, "   at {a}");
        }
        if !f.evidence.is_empty() {
            let ev: Vec<String> = f.evidence.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let _ = writeln!(out, "   evidence: {}", ev.join(" "));
        }
        if !f.sites.is_empty() {
            let _ = writeln!(out, "   also: {}", f.sites.join("; "));
        }
        let _ = writeln!(out, "   why: {}", f.why);
        let _ = writeln!(out, "   fix: {}", f.fix);
    }
    if findings.len() > limit {
        let _ = writeln!(out, "\n... +{} more", findings.len() - limit);
    }
    out
}
