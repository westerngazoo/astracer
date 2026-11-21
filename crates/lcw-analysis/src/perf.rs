//! Performance and structure lenses that follow calls through the graph:
//! async code reaching blocking I/O, I/O and large clones inside loops, and
//! call cycles. Every diagnostic carries the call site (`site`) and the
//! nodes involved (`related`) so it points at code, not at a metric.

use std::collections::{HashSet, VecDeque};

use lcw_config::{Config, LensesConfig};
use lcw_core::{CodeGraph, Diagnostic, Edge, EdgeKind, NodeId, NodeKind, Severity};
use petgraph::algo::tarjan_scc;

use crate::Lens;

/// Built-in blocking calls: synchronous file system / network I/O,
/// embedded-database transactions, blocking HTTP clients, password hashing
/// and sleeping. `path::*` matches a path prefix (`std::fs::write`); other
/// patterns match the whole called path or its trailing segments
/// (`begin_write` matches `db.begin_write()` and `Txn::begin_write`).
pub const DEFAULT_BLOCKING: &[&str] = &[
    "std::fs::*",
    "fs::*",
    "File::open",
    "File::create",
    "OpenOptions::open",
    "std::net::*",
    "TcpStream::connect",
    "UdpSocket::bind",
    "std::thread::sleep",
    "thread::sleep",
    "std::process::Command::*",
    "Database::create",
    "Database::open",
    "Database::builder",
    "begin_write",
    "begin_read",
    "open_table",
    "Connection::open",
    "Connection::open_in_memory",
    "execute_batch",
    "query_row",
    "sled::*",
    "open_tree",
    "rocksdb::*",
    "reqwest::blocking::*",
    "ureq::*",
    "argon2::*",
    "Argon2::*",
    "hash_password",
    "verify_password",
    "bcrypt::*",
    "scrypt::*",
    "pbkdf2::*",
];

/// Matches called paths against blocking-call patterns.
#[derive(Debug, Clone, Default)]
pub struct Blocking {
    prefixes: Vec<String>,
    names: Vec<String>,
}

impl Blocking {
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        let mut b = Blocking::default();
        for p in patterns {
            match p.strip_suffix("::*") {
                Some(prefix) => b.prefixes.push(format!("{prefix}::")),
                None => b.names.push(p.to_string()),
            }
        }
        b
    }

    /// The built-in list plus `blocking_calls`, minus `blocking_allow`.
    pub fn from_config(cfg: &LensesConfig) -> Self {
        let allow: HashSet<&str> = cfg.blocking_allow.iter().map(String::as_str).collect();
        Blocking::new(
            DEFAULT_BLOCKING
                .iter()
                .copied()
                .chain(cfg.blocking_calls.iter().map(String::as_str))
                .filter(|p| !allow.contains(p)),
        )
    }

    pub fn matches(&self, path: &str) -> bool {
        self.prefixes.iter().any(|p| path.starts_with(p.as_str()))
            || self.names.iter().any(|n| {
                path == n
                    || path
                        .strip_suffix(n.as_str())
                        .is_some_and(|head| head.ends_with("::"))
            })
    }
}

/// An edge that runs its callee on the caller's thread: not a spawn, and
/// not inside `block_in_place`.
fn runs_inline(e: &Edge) -> bool {
    e.kind != EdgeKind::Spawn && !e.blocking_ok
}

/// A call that can block the calling thread on `target`: inline, and not an
/// `.await`ed call into a library (that one is async, whatever its name).
fn may_block(graph: &CodeGraph, e: &Edge, target: NodeId) -> bool {
    runs_inline(e) && !(e.awaited && graph.node(target).kind == NodeKind::External)
}

/// How far each node is from a blocking call, following inline calls
/// backwards: `Some((0, None))` for a blocking external, `Some((k, next))`
/// for a function `k` calls away whose first hop is `next`. `through_async`
/// lets the walk pass through `async fn`s (they block too once awaited in a
/// loop); otherwise an `async fn` is a boundary, flagged on its own.
pub fn blocking_levels(
    graph: &CodeGraph,
    blocking: &Blocking,
    max_depth: u32,
    through_async: bool,
) -> Vec<Option<(u32, Option<NodeId>)>> {
    let mut level = vec![None; graph.node_count()];
    let mut queue = VecDeque::new();
    for id in graph.node_ids() {
        let n = graph.node(id);
        if n.kind == NodeKind::External && blocking.matches(&n.qualified_name) {
            level[id.0 as usize] = Some((0, None));
            queue.push_back(id);
        }
    }
    while let Some(t) = queue.pop_front() {
        let (d, _) = level[t.0 as usize].expect("queued nodes have a level");
        if d >= max_depth {
            continue;
        }
        for (from, e) in graph.edges_in(t) {
            let n = graph.node(from);
            if level[from.0 as usize].is_some()
                || n.kind == NodeKind::External
                || !may_block(graph, e, t)
                || (n.flags.is_async && !through_async)
            {
                continue;
            }
            level[from.0 as usize] = Some((d + 1, Some(t)));
            queue.push_back(from);
        }
    }
    level
}

/// The chain `start -> … -> blocking external` recorded by [`blocking_levels`].
pub fn blocking_chain(levels: &[Option<(u32, Option<NodeId>)>], start: NodeId) -> Vec<NodeId> {
    let mut chain = vec![start];
    let mut cur = start;
    while let Some((_, Some(next))) = levels[cur.0 as usize] {
        if chain.contains(&next) {
            break;
        }
        chain.push(next);
        cur = next;
    }
    chain
}

fn chain_text(graph: &CodeGraph, chain: &[NodeId]) -> String {
    chain
        .iter()
        .map(|&id| short(&graph.node(id).qualified_name))
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// The last two path segments (`Store::get_course`).
fn short(q: &str) -> String {
    let parts: Vec<&str> = q.rsplitn(3, "::").collect();
    match parts.as_slice() {
        [a, b, _] | [a, b] => format!("{b}::{a}"),
        _ => q.to_string(),
    }
}

/// `async fn`s that reach synchronous blocking calls on the executor thread.
pub struct AsyncBlockingLens;

impl Lens for AsyncBlockingLens {
    fn name(&self) -> &'static str {
        "async_blocking"
    }

    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic> {
        let cfg = &config.lenses;
        let blocking = Blocking::from_config(cfg);
        let depth = cfg.blocking_depth.max(1);
        let levels = blocking_levels(graph, &blocking, depth.saturating_sub(1), false);
        let mut out = Vec::new();
        for id in graph.node_ids() {
            let n = graph.node(id);
            if n.kind == NodeKind::External || !n.flags.is_async {
                continue;
            }
            let mut best: Option<(u32, NodeId, &Edge)> = None;
            let mut hits = 0u64;
            for (t, e) in graph.edges_out(id) {
                if !may_block(graph, e, t) || graph.node(t).flags.is_async {
                    continue;
                }
                let Some((d, _)) = levels[t.0 as usize] else {
                    continue;
                };
                hits += u64::from(e.count);
                let better = match best {
                    None => true,
                    Some((bd, _, be)) => {
                        (d, e.call_site.start_line) < (bd, be.call_site.start_line)
                    }
                };
                if better {
                    best = Some((d, t, e));
                }
            }
            let Some((d, target, edge)) = best else {
                continue;
            };
            let mut chain = vec![id];
            chain.extend(blocking_chain(&levels, target));
            let sink = &graph.node(*chain.last().expect("non-empty")).qualified_name;
            let severity = if d == 0 {
                Severity::High
            } else {
                Severity::Medium
            };
            out.push(
                Diagnostic::new(
                    "async_blocking",
                    "async_blocking",
                    severity,
                    format!(
                        "async `{}` blocks the executor: {}; move it into `spawn_blocking` \
                         or use an async API",
                        n.qualified_name,
                        if chain.len() == 2 {
                            format!("calls `{sink}` (sync I/O)")
                        } else {
                            format!(
                                "calls {}, which reaches `{sink}` (sync I/O)",
                                chain_text(graph, &chain[1..chain.len() - 1])
                            )
                        },
                    ),
                )
                .at(id)
                .with_site(edge.call_site)
                .with_related(chain[1..].iter().copied())
                .with_evidence("depth", u64::from(d) + 1)
                .with_evidence("blocking_calls", hits),
            );
        }
        out
    }
}

/// Collections and owned buffers whose `clone()` copies every element.
const HEAVY_CLONE_TYPES: &[&str] = &[
    "Vec", "VecDeque", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "String",
];

/// I/O (N+1) and large clones inside loops and per-element closures.
pub struct LoopLens;

impl Lens for LoopLens {
    fn name(&self) -> &'static str {
        "loops"
    }

    fn evaluate(&self, graph: &CodeGraph, config: &Config) -> Vec<Diagnostic> {
        let cfg = &config.lenses;
        let blocking = Blocking::from_config(cfg);
        let levels = blocking_levels(graph, &blocking, cfg.blocking_depth.max(1), true);
        let repo_types: HashSet<&str> = graph
            .nodes()
            .filter(|n| n.kind == NodeKind::Method)
            .filter_map(|n| n.module_path.rsplit("::").next())
            .collect();
        let mut out = Vec::new();
        for (from, to, e) in graph.edges() {
            let Some(site) = e.loop_site else { continue };
            if from == to || graph.node(from).kind == NodeKind::External {
                continue;
            }
            let caller = &graph.node(from).qualified_name;
            let callee = graph.node(to);
            if let (true, Some((d, _))) = (may_block(graph, e, to), levels[to.0 as usize]) {
                let chain = blocking_chain(&levels, to);
                let sink = &graph.node(*chain.last().expect("non-empty")).qualified_name;
                let via = if chain.len() > 1 {
                    format!(" (via {})", chain_text(graph, &chain))
                } else {
                    String::new()
                };
                out.push(
                    Diagnostic::new(
                        "loops",
                        "n_plus_one",
                        Severity::Medium,
                        format!(
                            "`{caller}` calls `{}` once per loop iteration and it does I/O \
                             (`{sink}`){via}; batch the reads or hoist them out of the loop",
                            short(&callee.qualified_name),
                        ),
                    )
                    .at(from)
                    .with_site(site)
                    .with_related(chain)
                    .with_evidence("depth", u64::from(d)),
                );
                continue;
            }
            if callee.kind != NodeKind::External {
                continue;
            }
            let Some((ty, method)) = callee.qualified_name.rsplit_once("::") else {
                continue;
            };
            let ty = ty.rsplit("::").next().unwrap_or(ty);
            if matches!(method, "clone" | "to_vec" | "to_owned")
                && (HEAVY_CLONE_TYPES.contains(&ty) || repo_types.contains(ty))
            {
                out.push(
                    Diagnostic::new(
                        "loops",
                        "clone_in_loop",
                        Severity::Low,
                        format!(
                            "`{caller}` clones a `{ty}` on every loop iteration; borrow it, \
                             clone once outside the loop, or share it behind an `Arc`"
                        ),
                    )
                    .at(from)
                    .with_site(site)
                    .with_related([to]),
                );
            }
        }
        out
    }
}

/// Call cycles: one diagnostic per strongly connected component, listing
/// its members. Plain recursion (one function, or a cycle inside one type
/// such as a recursive-descent parser) is low severity; cycles crossing
/// types or modules block modular reasoning and get louder with size.
pub struct CyclesLens;

impl Lens for CyclesLens {
    fn name(&self) -> &'static str {
        "cycles"
    }

    fn evaluate(&self, graph: &CodeGraph, _config: &Config) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for component in tarjan_scc(graph.raw()) {
            let mut members: Vec<NodeId> = component
                .iter()
                .map(|i| NodeId(i.index() as u32))
                .filter(|&id| graph.node(id).kind != NodeKind::External)
                .collect();
            let Some(&first) = members.first() else {
                continue;
            };
            if members.len() == 1 && !graph.neighbors_out(first).any(|t| t == first) {
                continue;
            }
            members.sort_by_key(|&id| {
                (
                    std::cmp::Reverse(graph.in_degree(id)),
                    graph.node(id).qualified_name.clone(),
                )
            });
            let head = members[0];
            let size = members.len();
            let scopes: HashSet<&str> = members
                .iter()
                .map(|&id| graph.node(id).module_path.as_str())
                .collect();
            let site = members
                .iter()
                .find_map(|&m| {
                    graph
                        .edges_out(m)
                        .find(|(t, _)| members.contains(t))
                        .map(|(_, e)| e.call_site)
                })
                .unwrap_or(graph.node(head).span);
            let (severity, what) = match (size, scopes.len()) {
                (1, _) => (Severity::Low, "recursive function".to_string()),
                (_, 1) => (
                    Severity::Low,
                    format!("{size} mutually recursive functions in one scope"),
                ),
                (s, k) if s >= 6 || k >= 3 => (
                    Severity::High,
                    format!("call cycle of {s} functions across {k} scopes"),
                ),
                (s, k) => (
                    Severity::Medium,
                    format!("call cycle of {s} functions across {k} scopes"),
                ),
            };
            let names: Vec<String> = members
                .iter()
                .take(6)
                .map(|&id| short(&graph.node(id).qualified_name))
                .collect();
            let more = if size > 6 {
                format!(" +{} more", size - 6)
            } else {
                String::new()
            };
            out.push(
                Diagnostic::new(
                    "cycles",
                    "cycle",
                    severity,
                    format!("{what}: {}{more}", names.join(", ")),
                )
                .at(head)
                .with_site(site)
                .with_related(members.iter().copied().skip(1))
                .with_evidence("size", size as u64)
                .with_evidence("scopes", scopes.len() as u64),
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Node, NodeFlags, NodeStats, SourceSpan};

    struct G {
        g: CodeGraph,
        line: u32,
    }

    impl G {
        fn new() -> Self {
            let mut g = CodeGraph::new();
            g.intern_file("src/lib.rs");
            G { g, line: 1 }
        }

        fn add(&mut self, q: &str, kind: NodeKind, is_async: bool) -> NodeId {
            let (module, name) = q.rsplit_once("::").unwrap_or(("", q));
            self.g.add_node(Node {
                id: NodeId(0),
                name: name.into(),
                qualified_name: q.into(),
                module_path: module.into(),
                kind,
                span: SourceSpan::new(lcw_core::FileId(0), 1, 0, 2, 0),
                flags: NodeFlags {
                    is_async,
                    ..Default::default()
                },
                stats: NodeStats::default(),
            })
        }

        fn ext(&mut self, q: &str) -> NodeId {
            let n = Node::external(q);
            self.g.add_node(n)
        }

        fn call(&mut self, a: NodeId, b: NodeId, kind: EdgeKind, in_loop: bool, ok: bool) {
            self.line += 1;
            let site = SourceSpan::new(lcw_core::FileId(0), self.line, 0, self.line, 1);
            self.g
                .add_edge(a, b, Edge::new(kind, site).with_context(in_loop, ok));
        }

        fn call_awaited(&mut self, a: NodeId, b: NodeId) {
            self.line += 1;
            let site = SourceSpan::new(lcw_core::FileId(0), self.line, 0, self.line, 1);
            self.g.add_edge(
                a,
                b,
                Edge::new(EdgeKind::Unresolved, site).with_awaited(true),
            );
        }
    }

    #[test]
    fn blocking_patterns_match_prefixes_and_trailing_segments() {
        let b = Blocking::new(DEFAULT_BLOCKING.iter().copied());
        assert!(b.matches("std::fs::read_to_string"));
        assert!(b.matches("fs::remove_file"));
        assert!(!b.matches("tokio::fs::remove_file"));
        assert!(b.matches("begin_write"));
        assert!(b.matches("WriteTransaction::begin_write"));
        assert!(!b.matches("my_begin_write"));
        assert!(b.matches("redb::Database::create"));
        assert!(!b.matches("usvg::fontdb::Database::new"));
        let cfg = LensesConfig {
            blocking_calls: vec!["Store::load".into()],
            blocking_allow: vec!["fs::*".into()],
            ..Default::default()
        };
        let b = Blocking::from_config(&cfg);
        assert!(b.matches("crate::Store::load"));
        assert!(!b.matches("fs::read"));
    }

    #[test]
    fn async_fns_reaching_sync_io_are_flagged_unless_offloaded() {
        let mut t = G::new();
        let handler = t.add("srv::get_course", NodeKind::Function, true);
        let offloaded = t.add("srv::offloaded", NodeKind::Function, true);
        let in_place = t.add("srv::in_place", NodeKind::Function, true);
        let sync_caller = t.add("srv::sync_caller", NodeKind::Function, false);
        let store = t.add("store::Store::get_course", NodeKind::Method, false);
        let closure = t.add("srv::offloaded::<spawned@L3>", NodeKind::Function, false);
        let begin = t.ext("begin_read");
        t.call(handler, store, EdgeKind::MethodCall, false, false);
        t.call(store, begin, EdgeKind::Unresolved, false, false);
        t.call(offloaded, closure, EdgeKind::Spawn, false, false);
        t.call(closure, store, EdgeKind::MethodCall, false, false);
        t.call(in_place, store, EdgeKind::MethodCall, false, true);
        t.call(sync_caller, store, EdgeKind::MethodCall, false, false);
        let upload = t.add("srv::upload", NodeKind::Function, true);
        let tokio_file = t.ext("File::create");
        t.call_awaited(upload, tokio_file);

        let d = AsyncBlockingLens.evaluate(&t.g, &Config::default());
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].node, Some(handler));
        assert_eq!(d[0].evidence("depth"), Some(2));
        assert_eq!(d[0].site.unwrap().start_line, 2);
        assert_eq!(d[0].related, vec![store, begin]);
        assert!(d[0]
            .message
            .contains("calls Store::get_course, which reaches `begin_read`"));
    }

    #[test]
    fn loops_flag_io_per_iteration_and_heavy_clones() {
        let mut t = G::new();
        let view = t.add("srv::foundations_view", NodeKind::Function, true);
        let get = t.add("store::Store::get_concept", NodeKind::Method, false);
        let pure = t.add("srv::format", NodeKind::Function, false);
        let begin = t.ext("begin_read");
        let clone = t.ext("Vec::clone");
        let small = t.ext("Uuid::clone");
        t.call(get, begin, EdgeKind::Unresolved, false, false);
        t.call(view, get, EdgeKind::MethodCall, true, false);
        t.call(view, pure, EdgeKind::DirectCall, true, false);
        t.call(view, clone, EdgeKind::Unresolved, true, false);
        t.call(view, small, EdgeKind::Unresolved, true, false);

        let d = LoopLens.evaluate(&t.g, &Config::default());
        let codes: Vec<_> = d.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(codes, vec!["n_plus_one", "clone_in_loop"], "{d:#?}");
        assert_eq!(d[0].site.unwrap().start_line, 3);
    }

    #[test]
    fn cycles_get_one_diagnostic_each_with_members() {
        let mut t = G::new();
        let a = t.add("app::a::run", NodeKind::Function, false);
        let b = t.add("app::b::step", NodeKind::Function, false);
        let c = t.add("app::c::back", NodeKind::Function, false);
        let parse = t.add("app::Parser::expr", NodeKind::Method, false);
        let unary = t.add("app::Parser::unary", NodeKind::Method, false);
        let fact = t.add("app::fact", NodeKind::Function, false);
        t.call(a, b, EdgeKind::DirectCall, false, false);
        t.call(b, c, EdgeKind::DirectCall, false, false);
        t.call(c, a, EdgeKind::DirectCall, false, false);
        t.call(parse, unary, EdgeKind::MethodCall, false, false);
        t.call(unary, parse, EdgeKind::MethodCall, false, false);
        t.call(fact, fact, EdgeKind::DirectCall, false, false);

        let mut d = CyclesLens.evaluate(&t.g, &Config::default());
        d.sort_by_key(|d| std::cmp::Reverse(d.evidence("size")));
        assert_eq!(d.len(), 3, "{d:#?}");
        assert_eq!(d[0].severity, Severity::High);
        assert_eq!(d[0].related.len(), 2);
        assert_eq!(d[1].severity, Severity::Low, "parser recursion: {:?}", d[1]);
        assert_eq!((d[2].node, d[2].severity), (Some(fact), Severity::Low));
    }
}
