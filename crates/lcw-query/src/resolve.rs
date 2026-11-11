//! Human-friendly symbol lookup: turn a pattern someone typed (`main`,
//! `Engine::analyze`, `lcw_core::graph`) into graph nodes, best match first.

use lcw_core::{CodeGraph, NodeId, NodeKind};

/// Resolve `pattern` to internal (non-external) nodes whose qualified name
/// contains it, case-insensitively. Ranked best-first:
///
/// 1. exact qualified name (`app::io::load`),
/// 2. exact short name (`load`),
/// 3. a trailing path segment match (`io::load` matches `app::io::load`),
/// 4. any substring,
///
/// with shorter qualified names first inside a rank (favoring the most direct
/// symbol) and the name itself as the final tiebreak, so results are stable.
pub fn resolve(graph: &CodeGraph, pattern: &str) -> Vec<NodeId> {
    ranked(graph, pattern)
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}

/// [`resolve`] with each hit's rank (0 = exact qualified name ... 3 = substring).
fn ranked(graph: &CodeGraph, pattern: &str) -> Vec<(u8, NodeId)> {
    let p = pattern.trim().to_lowercase();
    if p.is_empty() {
        return Vec::new();
    }
    let suffix = format!("::{p}");
    let mut hits: Vec<(u8, usize, NodeId)> = graph
        .node_ids()
        .filter_map(|id| {
            let n = graph.node(id);
            if n.kind == NodeKind::External {
                return None;
            }
            let qn = n.qualified_name.to_lowercase();
            if !qn.contains(&p) {
                return None;
            }
            let rank = if qn == p {
                0
            } else if n.name.to_lowercase() == p {
                1
            } else if qn.ends_with(&suffix) {
                2
            } else {
                3
            };
            Some((rank, n.qualified_name.len(), id))
        })
        .collect();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| {
            graph
                .node(a.2)
                .qualified_name
                .cmp(&graph.node(b.2).qualified_name)
        })
    });
    hits.into_iter().map(|(r, _, id)| (r, id)).collect()
}

/// The single best match for `pattern`, if any.
pub fn best_match(graph: &CodeGraph, pattern: &str) -> Option<NodeId> {
    resolve(graph, pattern).into_iter().next()
}

/// The outcome of a strict lookup: one symbol, or candidates to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Exactly one best match (an exact qualified name always is).
    Found(NodeId),
    /// Several symbols tie for the best rank (e.g. many `new`s); the tied ones
    /// come first, then weaker matches.
    Ambiguous(Vec<NodeId>),
    /// Nothing contains the pattern; these are the closest names (fuzzy).
    NotFound(Vec<NodeId>),
}

/// Strict variant of [`best_match`] for tools that must not guess: returns
/// [`Lookup::Ambiguous`] when more than one symbol shares the best rank (and
/// the pattern is not an exact qualified name), and fuzzy suggestions when
/// nothing matches. At most `max_candidates` ids are returned.
pub fn lookup(graph: &CodeGraph, pattern: &str, max_candidates: usize) -> Lookup {
    let hits = ranked(graph, pattern);
    let Some(&(best, first)) = hits.first() else {
        return Lookup::NotFound(suggest(graph, pattern, max_candidates));
    };
    let tied = hits.iter().take_while(|h| h.0 == best).count();
    if best == 0 || tied == 1 {
        return Lookup::Found(first);
    }
    Lookup::Ambiguous(
        hits.into_iter()
            .take(max_candidates)
            .map(|(_, id)| id)
            .collect(),
    )
}

/// Fuzzy "did you mean": internal symbols whose short name is within a small
/// edit distance of the pattern's last path segment, then symbols whose
/// qualified name contains the pattern as a subsequence. Closest first.
pub fn suggest(graph: &CodeGraph, pattern: &str, max: usize) -> Vec<NodeId> {
    let p = pattern.trim().to_lowercase();
    if p.is_empty() || max == 0 {
        return Vec::new();
    }
    let last = p.rsplit("::").next().unwrap_or(&p);
    let budget = (last.chars().count().div_ceil(3)).max(1);
    let needle: Vec<char> = p.chars().filter(|c| *c != ':').collect();

    let mut scored: Vec<(usize, usize, NodeId)> = graph
        .node_ids()
        .filter_map(|id| {
            let n = graph.node(id);
            if n.kind == NodeKind::External {
                return None;
            }
            let d = edit_distance(last, &n.name.to_lowercase());
            let score = if d <= budget {
                d
            } else if is_subsequence(&needle, &n.qualified_name.to_lowercase()) {
                budget + 1
            } else {
                return None;
            };
            Some((score, n.qualified_name.len(), id))
        })
        .collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| {
            graph
                .node(a.2)
                .qualified_name
                .cmp(&graph.node(b.2).qualified_name)
        })
    });
    scored.into_iter().take(max).map(|(_, _, id)| id).collect()
}

fn is_subsequence(needle: &[char], hay: &str) -> bool {
    let mut it = needle.iter().peekable();
    for c in hay.chars() {
        if it.peek() == Some(&&c) {
            it.next();
        }
    }
    it.peek().is_none()
}

/// Edit distance over chars where an adjacent transposition (`prase` →
/// `parse`) costs 1, the most common typo (optimal string alignment).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Three rolling rows: i-2, i-1, i.
    let mut pp: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(pp[j - 2] + 1);
            }
            cur[j] = d;
        }
        std::mem::swap(&mut pp, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id, plain};

    #[test]
    fn exact_short_name_beats_substring() {
        let g = app_graph();
        let hits = resolve(&g, "parse");
        assert_eq!(hits[0], id(&g, "app::core::parse"));
        // `test_parse` also contains "parse" but only as a substring.
        assert!(hits.contains(&id(&g, "app::core::tests::test_parse")));
    }

    #[test]
    fn qualified_suffix_and_case_insensitivity() {
        let g = app_graph();
        assert_eq!(best_match(&g, "IO::LOAD"), Some(id(&g, "app::io::load")));
        assert_eq!(
            best_match(&g, "app::io::load"),
            Some(id(&g, "app::io::load"))
        );
        assert_eq!(
            best_match(&g, "Lexer::next"),
            Some(id(&g, "app::core::Lexer::next"))
        );
    }

    #[test]
    fn externals_and_empty_patterns_never_match() {
        let g = app_graph();
        assert!(resolve(&g, "std::fs::read").is_empty());
        assert!(resolve(&g, "   ").is_empty());
        assert!(resolve(&g, "no_such_symbol").is_empty());
    }

    #[test]
    fn lookup_finds_unique_best_and_flags_ties() {
        let mut g = app_graph();
        assert_eq!(
            lookup(&g, "parse", 5),
            Lookup::Found(id(&g, "app::core::parse"))
        );
        let a = plain(&mut g, "app::a::new", "src/a.rs", 1);
        let b = plain(&mut g, "app::b::new", "src/b.rs", 1);
        assert_eq!(lookup(&g, "new", 5), Lookup::Ambiguous(vec![a, b]));
        // An exact qualified name is never ambiguous.
        assert_eq!(lookup(&g, "app::b::new", 5), Lookup::Found(b));
        assert_eq!(lookup(&g, "new", 1), Lookup::Ambiguous(vec![a]));
    }

    #[test]
    fn lookup_suggests_close_names_when_nothing_matches() {
        let g = app_graph();
        let Lookup::NotFound(c) = lookup(&g, "prase", 3) else {
            panic!("expected NotFound");
        };
        assert_eq!(c[0], id(&g, "app::core::parse"));
        // Qualifier typo: the short name still matches exactly.
        let Lookup::NotFound(c) = lookup(&g, "Lexr::next", 3) else {
            panic!("expected NotFound");
        };
        assert_eq!(c[0], id(&g, "app::core::Lexer::next"));
        // Subsequence fallback.
        assert!(suggest(&g, "aiold", 3).contains(&id(&g, "app::io::load")));
        assert!(suggest(&g, "zzzzzz", 3).is_empty());
        assert!(suggest(&g, "std::fs::read", 3)
            .iter()
            .all(|&i| i != g.node_by_qualified("std::fs::read").unwrap()));
    }

    #[test]
    fn edit_distance_counts_transpositions_once() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
        assert_eq!(edit_distance("prase", "parse"), 1);
        assert_eq!(edit_distance("hbu", "hub"), 1);
    }
}
