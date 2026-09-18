//! Search / filter highlighting (Feature 3).
//!
//! The frontend owns the text `<input>`; this module owns the *policy*: which
//! nodes match a query and how the scene is re-colored to emphasize them. All
//! pure and native-testable — the wasm side only forwards a query string and
//! the node name table.

use crate::scene::SceneData;

/// Case-insensitive substring match against a node's short and qualified name.
/// An empty (whitespace-only) query matches everything, i.e. "no filter".
pub fn matches(query: &str, name: &str, qualified: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&q) || qualified.to_lowercase().contains(&q)
}

/// A query is "active" (actually filtering) when it is non-empty after trim.
pub fn is_active(query: &str) -> bool {
    !query.trim().is_empty()
}

/// Compute a per-node match mask from `(name, qualified_name)` pairs, indexed to
/// line up with `SceneData::nodes` (node index == pick_id − 1).
pub fn compute_matches<'a, I>(query: &str, items: I) -> Vec<bool>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    items
        .into_iter()
        .map(|(name, qualified)| matches(query, name, qualified))
        .collect()
}

/// How matched vs unmatched geometry is styled.
#[derive(Debug, Clone, Copy)]
pub struct FilterStyle {
    /// Multiplier applied to the alpha of non-matching nodes and any edge that
    /// touches a non-matching node (`0.0..=1.0`).
    pub dim_alpha: f32,
}

impl Default for FilterStyle {
    fn default() -> Self {
        FilterStyle { dim_alpha: 0.12 }
    }
}

/// Return a copy of `base` where matching nodes keep their color and everything
/// else is dimmed, so a search visually isolates the matches without changing
/// layout (positions/radii are untouched, so hit-testing still works).
///
/// `matched[i]` corresponds to node `i`; a missing/short mask is treated as
/// "matches" (fail open). An edge is dimmed when *either* endpoint is dimmed.
pub fn apply_filter(base: &SceneData, matched: &[bool], style: FilterStyle) -> SceneData {
    let dim = style.dim_alpha.clamp(0.0, 1.0);
    let is_match = |i: usize| matched.get(i).copied().unwrap_or(true);

    let mut nodes = base.nodes.clone();
    for (i, node) in nodes.iter_mut().enumerate() {
        if !is_match(i) {
            node.color[3] *= dim;
        }
    }

    let mut edges = base.edges.clone();
    for (seg, pair) in base.edge_nodes.iter().enumerate() {
        let keep = is_match(pair[0] as usize) && is_match(pair[1] as usize);
        if keep {
            continue;
        }
        if let Some(v) = edges.get_mut(seg * 2) {
            v.color[3] *= dim;
        }
        if let Some(v) = edges.get_mut(seg * 2 + 1) {
            v.color[3] *= dim;
        }
    }

    SceneData {
        nodes,
        edges,
        edge_nodes: base.edge_nodes.clone(),
        min: base.min,
        max: base.max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{EdgeVertex, NodeInstance};

    #[test]
    fn matching_is_case_insensitive_and_covers_both_names() {
        assert!(matches("parse", "parse", "crate::a::parse"));
        assert!(matches("PARSE", "parse", "crate::a::parse"));
        assert!(matches("a::par", "parse", "crate::a::parse")); // qualified only
        assert!(!matches("zzz", "parse", "crate::a::parse"));
        // Empty query = everything matches (no filter).
        assert!(matches("   ", "anything", "any::thing"));
        assert!(!is_active("  "));
        assert!(is_active("x"));
    }

    #[test]
    fn compute_matches_lines_up_with_nodes() {
        let items = [
            ("parse", "a::parse"),
            ("build", "a::build"),
            ("parser", "b::parser"),
        ];
        let mask = compute_matches("pars", items);
        assert_eq!(mask, vec![true, false, true]);
    }

    fn scene() -> SceneData {
        let mk = |c: [f32; 2], pick: u32| NodeInstance {
            center: c,
            radius: 6.0,
            color: [0.5, 0.5, 0.5, 1.0],
            pick_id: pick,
        };
        let ev = |p: [f32; 2]| EdgeVertex {
            pos: p,
            color: [0.7, 0.7, 0.7, 0.55],
        };
        SceneData {
            nodes: vec![mk([0.0, 0.0], 1), mk([10.0, 0.0], 2)],
            edges: vec![ev([0.0, 0.0]), ev([10.0, 0.0])],
            edge_nodes: vec![[0, 1]],
            min: [0.0, 0.0],
            max: [10.0, 0.0],
        }
    }

    #[test]
    fn apply_filter_dims_non_matches_and_touching_edges() {
        let base = scene();
        let out = apply_filter(&base, &[true, false], FilterStyle { dim_alpha: 0.1 });
        assert!((out.nodes[0].color[3] - 1.0).abs() < 1e-6); // match kept
        assert!((out.nodes[1].color[3] - 0.1).abs() < 1e-6); // dimmed
                                                             // Edge touches the dimmed node -> both its vertices dim.
        assert!((out.edges[0].color[3] - 0.055).abs() < 1e-6);
        assert!((out.edges[1].color[3] - 0.055).abs() < 1e-6);
        // Positions/radii are never touched.
        assert_eq!(out.nodes[1].center, base.nodes[1].center);
        assert_eq!(out.nodes[1].radius, base.nodes[1].radius);
    }

    #[test]
    fn apply_filter_all_match_is_noop_on_alpha() {
        let base = scene();
        let out = apply_filter(&base, &[true, true], FilterStyle::default());
        assert!((out.nodes[0].color[3] - 1.0).abs() < 1e-6);
        assert!((out.nodes[1].color[3] - 1.0).abs() < 1e-6);
        assert!((out.edges[0].color[3] - 0.55).abs() < 1e-6);
    }
}
