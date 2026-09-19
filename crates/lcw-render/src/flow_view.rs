//! The **flow view**: render a traced set of call paths (source → … → target)
//! as a left-to-right layered diagram — one column per hop distance, arrows
//! following the direction of calls. This is the "how does control get from
//! here to there" surface: pick an entry point and a concept, and see the
//! route(s) between them, each node labelled with its name and `file:line`.
//!
//! Pure presentation: the caller supplies an already-traced [`FlowGraph`]
//! (nodes with a precomputed `column` = distance from the source, plus edges).
//! Layout and geometry live here; graph traversal lives in the CLI.

use std::collections::BTreeMap;

use crate::module_view::crate_color;
use crate::scene::{EdgeVertex, NodeInstance, SceneData};
use crate::text;

/// One node on a traced flow.
#[derive(Debug, Clone)]
pub struct FlowNode {
    /// Short display name (e.g. `analyze`).
    pub label: String,
    /// Secondary line under the node (e.g. `engine/src/lib.rs:42`).
    pub detail: String,
    /// Crate the node belongs to (drives its color).
    pub crate_name: String,
    /// Column = distance in hops from the source (0 = source).
    pub column: usize,
    /// Fully qualified name, surfaced on click.
    pub pick: String,
    /// Highlight (source / target endpoints).
    pub emphasize: bool,
}

/// A traced flow ready to draw.
#[derive(Debug, Clone, Default)]
pub struct FlowGraph {
    pub title: String,
    pub nodes: Vec<FlowNode>,
    /// Directed edges as `(from_index, to_index)` into `nodes`.
    pub edges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Copy)]
pub struct FlowOptions {
    pub labels: bool,
}

impl Default for FlowOptions {
    fn default() -> Self {
        FlowOptions { labels: true }
    }
}

const COL_W: f32 = 500.0;
const ROW_H: f32 = 184.0;
const RADIUS: f32 = 26.0;

/// Lay out and emit a [`FlowGraph`] into a [`SceneData`], returning per-node
/// pick labels aligned with `NodeInstance` order.
pub fn build(fg: &FlowGraph, opts: &FlowOptions) -> (SceneData, Vec<String>) {
    // Bucket node indices by column, then stack each column vertically centered.
    let mut columns: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, n) in fg.nodes.iter().enumerate() {
        columns.entry(n.column).or_default().push(i);
    }
    let mut pos = vec![[0.0f32; 2]; fg.nodes.len()];
    for (&col, idxs) in &columns {
        let total_h = idxs.len().saturating_sub(1) as f32 * ROW_H;
        for (row, &ni) in idxs.iter().enumerate() {
            pos[ni] = [col as f32 * COL_W, total_h * 0.5 - row as f32 * ROW_H];
        }
    }

    let mut scene = SceneData::default();
    let mut picks = vec![String::new(); fg.nodes.len()];

    // Arrows: trimmed to the node discs, with a filled arrowhead near the target.
    for &(a, b) in &fg.edges {
        let (pa, pb) = (pos[a], pos[b]);
        let dir = normalize(sub(pb, pa));
        let start = add(pa, scale(dir, RADIUS));
        let end = sub(pb, scale(dir, RADIUS + 3.0));
        let color = [0.78, 0.83, 0.95, 0.75];
        scene.edges.push(EdgeVertex { pos: start, color });
        scene.edges.push(EdgeVertex { pos: end, color });
        scene.edge_nodes.push([a as u32, b as u32]);
        push_arrowhead(&mut scene.labels, end, dir, color);
    }

    // Nodes (+ optional white halo for endpoints) and labels.
    for (i, n) in fg.nodes.iter().enumerate() {
        let p = pos[i];
        if n.emphasize {
            scene.nodes.push(NodeInstance {
                center: p,
                radius: RADIUS + 6.0,
                color: [0.97, 0.98, 1.0, 0.85],
                pick_id: 0, // halo isn't pickable
            });
        }
        let rgb = crate_color(&n.crate_name);
        scene.nodes.push(NodeInstance {
            center: p,
            radius: RADIUS,
            color: [rgb[0], rgb[1], rgb[2], 1.0],
            pick_id: (i as u32) + 1,
        });
        picks[i] = n.pick.clone();

        if opts.labels {
            // Keep label sizes uniform: cap the pixel size so short names don't
            // balloon, and shrink long names just enough to fit the column.
            let unit = text::width(&n.label, 1.0).max(1.0);
            let px = (COL_W * 0.88 / unit).clamp(1.7, 2.8);
            let w = text::width(&n.label, px);
            text::emit(
                &mut scene.labels,
                &n.label,
                [p[0] - w * 0.5, p[1] + RADIUS + 8.0 * px + 8.0],
                px,
                [0.97, 0.98, 1.0, 1.0],
            );
            if !n.detail.is_empty() {
                let dpx = 2.0;
                let dw = text::width(&n.detail, dpx);
                text::emit(
                    &mut scene.labels,
                    &n.detail,
                    [p[0] - dw * 0.5, p[1] - RADIUS - 8.0],
                    dpx,
                    [0.80, 0.85, 0.94, 1.0],
                );
            }
        }
    }

    let (mut min, mut max) = point_bounds(&pos);
    min = [min[0] - COL_W * 0.5, min[1] - ROW_H];
    max = [max[0] + COL_W * 0.5, max[1] + ROW_H];
    if opts.labels && !fg.title.is_empty() {
        text::emit(
            &mut scene.labels,
            &fg.title,
            [min[0] + 24.0, max[1] - 16.0],
            4.0,
            [0.90, 0.95, 1.0, 1.0],
        );
    }
    scene.min = min;
    scene.max = max;
    (scene, picks)
}

fn push_arrowhead(out: &mut Vec<EdgeVertex>, tip: [f32; 2], dir: [f32; 2], color: [f32; 4]) {
    const SIZE: f32 = 11.0;
    let back = sub(tip, scale(dir, SIZE));
    let perp = [-dir[1], dir[0]];
    let left = add(back, scale(perp, SIZE * 0.5));
    let right = sub(back, scale(perp, SIZE * 0.5));
    let v = |p: [f32; 2]| EdgeVertex { pos: p, color };
    out.push(v(tip));
    out.push(v(left));
    out.push(v(right));
}

fn point_bounds(pos: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    if pos.is_empty() {
        return ([0.0, 0.0], [1.0, 1.0]);
    }
    let mut min = [f32::MAX, f32::MAX];
    let mut max = [f32::MIN, f32::MIN];
    for p in pos {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }
    (min, max)
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn scale(a: [f32; 2], s: f32) -> [f32; 2] {
    [a[0] * s, a[1] * s]
}
fn normalize(a: [f32; 2]) -> [f32; 2] {
    let len = (a[0] * a[0] + a[1] * a[1]).sqrt();
    if len > 1e-6 {
        [a[0] / len, a[1] / len]
    } else {
        [1.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str, column: usize) -> FlowNode {
        FlowNode {
            label: label.into(),
            detail: "src/x.rs:1".into(),
            crate_name: "krate".into(),
            column,
            pick: format!("krate::{label}"),
            emphasize: column == 0,
        }
    }

    #[test]
    fn columns_place_nodes_left_to_right() {
        let fg = FlowGraph {
            title: "flow".into(),
            nodes: vec![node("main", 0), node("mid", 1), node("target", 2)],
            edges: vec![(0, 1), (1, 2)],
        };
        let (scene, picks) = build(&fg, &FlowOptions::default());
        // 3 flow nodes + 1 halo (source emphasized).
        assert_eq!(scene.nodes.len(), 4);
        assert_eq!(picks.len(), 3);
        assert_eq!(picks[0], "krate::main");
        // Two arrows -> two edge segments; labels + arrowheads are non-empty.
        assert_eq!(scene.edges.len(), 4); // 2 segments * 2 verts
        assert!(!scene.labels.is_empty());
        assert!(scene.max[0] > scene.min[0]);
    }

    #[test]
    fn empty_flow_is_safe() {
        let (scene, picks) = build(&FlowGraph::default(), &FlowOptions::default());
        assert!(scene.nodes.is_empty());
        assert!(picks.is_empty());
    }
}
