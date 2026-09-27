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
    let p = pattern.trim().to_lowercase();
    if p.is_empty() {
        return Vec::new();
    }
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
            } else if qn.ends_with(&format!("::{p}")) {
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
    hits.into_iter().map(|(_, _, id)| id).collect()
}

/// The single best match for `pattern`, if any.
pub fn best_match(graph: &CodeGraph, pattern: &str) -> Option<NodeId> {
    resolve(graph, pattern).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id};

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
}
