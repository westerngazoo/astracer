//! The crate-level view of a Cargo workspace: per-crate size, the dependency
//! graph, cross-crate call counts, rebuild blast radius, the critical path,
//! and crates no shipped artifact builds.
//!
//! Pure: the caller maps each graph file to its crate and supplies line
//! counts (that needs the filesystem).

use lcw_core::{CodeGraph, CrateGraph, CrateIdx, NodeKind};

/// How a crate is reached from what the workspace ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateUsage {
    /// Built into a default-member binary / cdylib (or is one).
    Shipped,
    /// Built only for tests, benches or examples: as a dev-dependency, or
    /// by nothing but its own `tests/` / `examples/` / `benches/`.
    DevOnly,
    /// Nothing in the workspace builds it.
    Unused,
}

impl CrateUsage {
    pub fn as_str(self) -> &'static str {
        match self {
            CrateUsage::Shipped => "shipped",
            CrateUsage::DevOnly => "dev_only",
            CrateUsage::Unused => "unused",
        }
    }
}

/// One crate's row.
#[derive(Debug, Clone)]
pub struct CrateRow {
    pub idx: CrateIdx,
    pub name: String,
    pub member: bool,
    pub default_member: bool,
    /// A binary or cdylib/staticlib: something is built from it.
    pub root: bool,
    pub files: usize,
    pub lines: u64,
    /// Functions defined (tests included) / of them tests or examples.
    pub functions: usize,
    pub test_functions: usize,
    pub deps: Vec<CrateIdx>,
    pub dev_deps: Vec<CrateIdx>,
    /// Crates that depend on this one directly (normal deps).
    pub dependents: Vec<CrateIdx>,
    /// Crates recompiled when this one changes (transitive dependents).
    pub rebuild: Vec<CrateIdx>,
    /// Longest chain of normal dependencies below this crate (0 = leaf).
    pub depth: u32,
    /// Distinct caller→callee pairs leaving / entering the crate.
    pub calls_out: usize,
    pub calls_in: usize,
    pub usage: CrateUsage,
}

/// The whole crate view.
#[derive(Debug, Clone)]
pub struct CrateView {
    pub crates: Vec<CrateRow>,
    /// `(from, to, distinct call pairs, call sites)`, most calls first.
    pub calls: Vec<(CrateIdx, CrateIdx, usize, u64)>,
    /// The longest normal-dependency chain, top crate first.
    pub critical_path: Vec<CrateIdx>,
    pub has_default_members: bool,
}

impl CrateView {
    pub fn name(&self, idx: CrateIdx) -> &str {
        self.crates
            .iter()
            .find(|c| c.idx == idx)
            .map_or("?", |c| c.name.as_str())
    }

    pub fn row(&self, idx: CrateIdx) -> Option<&CrateRow> {
        self.crates.iter().find(|c| c.idx == idx)
    }
}

/// Build the view. `file_crate[f]` is the crate owning graph file `f`, and
/// `file_lines[f]` its line count (missing entries count as 0).
pub fn crate_view(
    crates: &CrateGraph,
    graph: &CodeGraph,
    file_crate: &[Option<CrateIdx>],
    file_lines: &[u64],
) -> CrateView {
    let n = crates.crates.len();
    let of_file = |f: u32| file_crate.get(f as usize).copied().flatten();

    let mut files = vec![0usize; n];
    let mut lines = vec![0u64; n];
    for (fid, _) in graph.files() {
        if let Some(c) = of_file(fid.0) {
            files[c as usize] += 1;
            lines[c as usize] += file_lines.get(fid.0 as usize).copied().unwrap_or(0);
        }
    }

    let mut functions = vec![0usize; n];
    let mut tests = vec![0usize; n];
    for node in graph.nodes() {
        if node.kind == NodeKind::External {
            continue;
        }
        if let Some(c) = of_file(node.span.file) {
            functions[c as usize] += 1;
            if node.flags.is_test || node.flags.is_example {
                tests[c as usize] += 1;
            }
        }
    }

    let mut pairs: std::collections::BTreeMap<(CrateIdx, CrateIdx), (usize, u64)> =
        Default::default();
    for (from, to, e) in graph.edges() {
        let (a, b) = (graph.node(from), graph.node(to));
        if b.kind == NodeKind::External {
            continue;
        }
        let (Some(ca), Some(cb)) = (of_file(a.span.file), of_file(b.span.file)) else {
            continue;
        };
        if ca != cb {
            let slot = pairs.entry((ca, cb)).or_default();
            slot.0 += 1;
            slot.1 += u64::from(e.count);
        }
    }
    let mut calls_out = vec![0usize; n];
    let mut calls_in = vec![0usize; n];
    for (&(a, b), &(k, _)) in &pairs {
        calls_out[a as usize] += k;
        calls_in[b as usize] += k;
    }
    let mut calls: Vec<_> = pairs
        .into_iter()
        .map(|((a, b), (k, s))| (a, b, k, s))
        .collect();
    calls.sort_by(|x, y| y.2.cmp(&x.2).then(x.0.cmp(&y.0)).then(x.1.cmp(&y.1)));

    let heights = heights(crates);
    let usage = usage(crates);

    let rows = crates
        .crates
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let idx = i as CrateIdx;
            CrateRow {
                idx,
                name: info.name.clone(),
                member: info.member,
                default_member: info.default_member,
                root: info.is_root(),
                files: files[i],
                lines: lines[i],
                functions: functions[i],
                test_functions: tests[i],
                deps: info.deps.clone(),
                dev_deps: info.dev_deps.clone(),
                dependents: (0..n as CrateIdx)
                    .filter(|&j| crates.crates[j as usize].deps.contains(&idx))
                    .collect(),
                rebuild: crates.rebuild_set(idx),
                depth: heights[i],
                calls_out: calls_out[i],
                calls_in: calls_in[i],
                usage: usage[i],
            }
        })
        .collect();

    CrateView {
        crates: rows,
        calls,
        critical_path: critical_path(crates, &heights),
        has_default_members: crates.has_default_members,
    }
}

/// Longest normal-dependency chain below each crate (cycle-safe).
fn heights(crates: &CrateGraph) -> Vec<u32> {
    fn visit(crates: &CrateGraph, c: usize, memo: &mut [Option<u32>], on: &mut [bool]) -> u32 {
        if let Some(h) = memo[c] {
            return h;
        }
        if on[c] {
            return 0;
        }
        on[c] = true;
        let n = memo.len();
        let mut h = 0;
        for &d in &crates.crates[c].deps {
            if (d as usize) < n {
                h = h.max(visit(crates, d as usize, memo, on) + 1);
            }
        }
        on[c] = false;
        memo[c] = Some(h);
        h
    }
    let n = crates.crates.len();
    let mut memo = vec![None; n];
    let mut on = vec![false; n];
    (0..n)
        .map(|c| visit(crates, c, &mut memo, &mut on))
        .collect()
}

fn critical_path(crates: &CrateGraph, heights: &[u32]) -> Vec<CrateIdx> {
    let Some(mut cur) = (0..heights.len()).max_by_key(|&i| (heights[i], std::cmp::Reverse(i)))
    else {
        return Vec::new();
    };
    let mut path = vec![cur as CrateIdx];
    while heights[cur] > 0 {
        let next = crates.crates[cur]
            .deps
            .iter()
            .map(|&d| d as usize)
            .filter(|&d| d < heights.len() && heights[d] + 1 == heights[cur])
            .min();
        let Some(next) = next else { break };
        path.push(next as CrateIdx);
        cur = next;
    }
    path
}

/// Shipped = reachable through normal deps from a default-member root
/// (binary / cdylib); when the workspace has no root at all (a library
/// workspace), every default member counts as one.
fn usage(crates: &CrateGraph) -> Vec<CrateUsage> {
    let n = crates.crates.len();
    let roots: Vec<CrateIdx> = {
        let r: Vec<CrateIdx> = (0..n as CrateIdx)
            .filter(|&i| {
                let c = &crates.crates[i as usize];
                c.default_member && c.is_root()
            })
            .collect();
        if r.is_empty() {
            (0..n as CrateIdx)
                .filter(|&i| crates.crates[i as usize].default_member)
                .collect()
        } else {
            r
        }
    };
    let mut shipped = vec![false; n];
    for &r in &roots {
        for (i, hit) in crates.reach_mask(r, false).into_iter().enumerate() {
            shipped[i] |= hit;
        }
    }
    let mut dev = vec![false; n];
    for (i, c) in crates.crates.iter().enumerate() {
        if c.has_tests || c.has_examples || c.has_benches {
            for (j, hit) in crates
                .reach_mask(i as CrateIdx, true)
                .into_iter()
                .enumerate()
            {
                dev[j] |= hit;
            }
        }
        for &d in &c.dev_deps {
            for (j, hit) in crates.reach_mask(d, false).into_iter().enumerate() {
                dev[j] |= hit;
            }
        }
    }
    (0..n)
        .map(|i| {
            if shipped[i] {
                CrateUsage::Shipped
            } else if dev[i] {
                CrateUsage::DevOnly
            } else {
                CrateUsage::Unused
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{CrateInfo, Edge, EdgeKind, Node, NodeFlags, NodeId, NodeStats, SourceSpan};

    fn krate(name: &str, deps: Vec<CrateIdx>, bin: bool, examples: bool) -> CrateInfo {
        CrateInfo {
            name: name.into(),
            dir: format!("/w/{name}").into(),
            member: true,
            default_member: true,
            deps,
            dev_deps: vec![],
            has_bin: bin,
            is_dylib: false,
            has_tests: false,
            has_examples: examples,
            has_benches: false,
        }
    }

    #[test]
    fn rows_calls_critical_path_and_usage() {
        // server(bin) -> store -> content; domain(examples) -> graph
        let crates = CrateGraph {
            crates: vec![
                krate("server", vec![1], true, false),
                krate("store", vec![2], false, false),
                krate("content", vec![], false, false),
                krate("domain", vec![4], false, true),
                krate("graph", vec![], false, false),
            ],
            has_default_members: false,
        };
        let mut g = CodeGraph::new();
        let fs: Vec<_> = ["server", "store", "content"]
            .iter()
            .map(|c| g.intern_file(format!("/w/{c}/src/lib.rs")))
            .collect();
        let add = |g: &mut CodeGraph, name: &str, f: usize| {
            g.add_node(Node {
                id: NodeId(0),
                name: name.into(),
                qualified_name: name.into(),
                module_path: String::new(),
                kind: NodeKind::Function,
                span: SourceSpan::new(fs[f], 1, 0, 2, 0),
                flags: NodeFlags::default(),
                stats: NodeStats::default(),
            })
        };
        let h = add(&mut g, "handler", 0);
        let get = add(&mut g, "get", 1);
        let parse = add(&mut g, "parse", 2);
        let site = SourceSpan::new(fs[0], 1, 0, 1, 1);
        g.add_edge(h, get, Edge::new(EdgeKind::MethodCall, site));
        g.add_edge(
            h,
            get,
            Edge::new(EdgeKind::MethodCall, SourceSpan::new(fs[0], 2, 0, 2, 1)),
        );
        g.add_edge(get, parse, Edge::new(EdgeKind::DirectCall, site));

        let v = crate_view(&crates, &g, &[Some(0), Some(1), Some(2)], &[10, 20, 30]);
        let store = v.row(1).unwrap();
        assert_eq!((store.files, store.lines, store.functions), (1, 20, 1));
        assert_eq!((store.calls_in, store.calls_out), (1, 1));
        assert_eq!(store.dependents, vec![0]);
        assert_eq!(v.row(2).unwrap().rebuild, vec![0, 1]);
        assert_eq!(v.critical_path, vec![0, 1, 2]);
        assert_eq!(v.calls[0], (0, 1, 1, 2));
        let usage: Vec<_> = v.crates.iter().map(|c| c.usage).collect();
        assert_eq!(
            usage,
            vec![
                CrateUsage::Shipped,
                CrateUsage::Shipped,
                CrateUsage::Shipped,
                CrateUsage::DevOnly,
                CrateUsage::DevOnly
            ]
        );
    }
}
