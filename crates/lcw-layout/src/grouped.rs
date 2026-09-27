//! The **crate-boxed layout**: every function stays on screen, but each crate's
//! functions are laid out on their own and drawn inside a box, and the boxes
//! stack in rows by call order — the parts nothing else calls on top, the
//! foundations at the bottom, external code last.
//!
//! One force simulation over the whole graph mixes every crate into a single
//! cloud, because a call between crates pulls exactly as hard as a call within
//! one. Laying each crate out separately, using only the calls inside it, keeps
//! a crate's structure visible; calls between crates become the edges running
//! between boxes, and with the rows in call order most of them point down.
//!
//! Which crate a node belongs to, and the row order, come from
//! [`lcw_query::crate_groups`]; this module only does geometry.

use lcw_core::{CodeGraph, GroupBox, GROUP_HEADER};
use lcw_query::{crate_groups, Groups};
use rayon::prelude::*;

use crate::{force, Layout, LayoutParams, Vec2};

/// Space between a box's border and the nearest node center. Roomy enough for
/// the largest node discs the renderer draws.
const PAD: f32 = 48.0;
/// Narrowest box, so a one-function crate still has room for its name.
const MIN_WIDTH: f32 = 240.0;
/// Horizontal gap between boxes in a row, and vertical gap between the rows a
/// wide layer wraps into.
const GAP: f32 = 80.0;
/// Vertical gap between layers: wider than [`GAP`], so the call order reads as
/// distinct bands and edges between them have room to be seen.
const LAYER_GAP: f32 = 200.0;
/// Width of a row relative to the square root of the total box area: wider
/// than square, for landscape screens.
const ASPECT: f32 = 1.6;
/// Empty space kept around the whole diagram in [`Layout::min`]/[`Layout::max`].
const MARGIN: f32 = 60.0;

/// Lay `graph` out one crate per box (see the module docs).
pub fn layout_by_crate(graph: &CodeGraph, params: &LayoutParams) -> Layout {
    layout_grouped(graph, params, &crate_groups(graph))
}

/// Lay `graph` out one box per group of `groups`, with the rows following
/// [`lcw_query::Group::layer`].
pub fn layout_grouped(graph: &CodeGraph, params: &LayoutParams, groups: &Groups) -> Layout {
    let n = graph.node_count();
    if n == 0 || groups.groups.is_empty() {
        return Layout {
            positions: Vec::new(),
            min: [0.0, 0.0],
            max: [0.0, 0.0],
            groups: Vec::new(),
        };
    }

    // Members of each group, and each node's index within its group.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups.groups.len()];
    let mut local = vec![0usize; n];
    for (i, &g) in groups.of_node.iter().enumerate().take(n) {
        let list = &mut members[g as usize];
        local[i] = list.len();
        list.push(i);
    }
    // Calls inside each group, in local indices. Calls between groups do not
    // shape a box's interior; they are drawn between boxes.
    let mut inner: Vec<Vec<(usize, usize)>> = vec![Vec::new(); groups.groups.len()];
    for (from, to, _) in graph.edges() {
        let (a, b) = (from.0 as usize, to.0 as usize);
        if a == b || a >= n || b >= n || groups.of_node[a] != groups.of_node[b] {
            continue;
        }
        inner[groups.of_node[a] as usize].push((local[a], local[b]));
    }

    // Each group's own simulation. Independent, so they run in parallel, and
    // `collect` keeps group order, so the result is still deterministic.
    let interiors: Vec<Vec<Vec2>> = members
        .par_iter()
        .zip(inner.par_iter())
        .map(|(m, e)| force(m.len(), e, params))
        .collect();

    let sizes: Vec<Vec2> = interiors.iter().map(|p| box_size(p)).collect();
    let corners = pack_rows(groups, &sizes);

    let mut positions = vec![[0.0f32; 2]; n];
    let mut boxes = Vec::with_capacity(groups.groups.len());
    for (g, group) in groups.groups.iter().enumerate() {
        let [left, top] = corners[g];
        let [w, h] = sizes[g];
        let (lo, hi) = crate::bounds(&interiors[g]);
        // Center the nodes horizontally (a box may be wider than its nodes,
        // for its name) and sit them on the bottom padding.
        let dx = left + (w - (hi[0] - lo[0])) * 0.5 - lo[0];
        let dy = top - h + PAD - lo[1];
        for (&node, p) in members[g].iter().zip(&interiors[g]) {
            positions[node] = [p[0] + dx, p[1] + dy];
        }
        boxes.push(GroupBox {
            name: group.name.clone(),
            min: [left, top - h],
            max: [left + w, top],
            members: members[g].len() as u32,
            external: group.external,
        });
    }

    let (min, max) = extent(&boxes);
    Layout {
        positions,
        min,
        max,
        groups: boxes,
    }
}

/// Width and height of the box around one group's laid-out nodes.
fn box_size(interior: &[Vec2]) -> Vec2 {
    let (lo, hi) = crate::bounds(interior);
    let w = (hi[0] - lo[0]) + 2.0 * PAD;
    let h = (hi[1] - lo[1]) + 2.0 * PAD + GROUP_HEADER;
    [w.max(MIN_WIDTH), h]
}

/// Top-left corner of every box. Layers go top to bottom; within a layer boxes
/// go left to right in group order, wrapping onto a new row past the target
/// width, and every row is centered on `x = 0`.
fn pack_rows(groups: &Groups, sizes: &[Vec2]) -> Vec<Vec2> {
    let area: f32 = sizes.iter().map(|s| s[0] * s[1]).sum();
    let widest = sizes.iter().map(|s| s[0]).fold(0.0f32, f32::max);
    let target = (area.sqrt() * ASPECT).max(widest);

    // Rows as runs of consecutive group indices (groups are sorted by layer).
    let mut rows: Vec<(u32, Vec<usize>)> = Vec::new();
    for (g, group) in groups.groups.iter().enumerate() {
        let fits = rows.last().is_some_and(|(layer, row)| {
            *layer == group.layer && row_width(row, sizes) + GAP + sizes[g][0] <= target
        });
        if fits {
            rows.last_mut().expect("checked above").1.push(g);
        } else {
            rows.push((group.layer, vec![g]));
        }
    }

    let mut corners = vec![[0.0f32; 2]; sizes.len()];
    let mut top = 0.0f32;
    let mut prev_layer: Option<u32> = None;
    for (layer, row) in &rows {
        if let Some(prev) = prev_layer {
            top -= if prev == *layer { GAP } else { LAYER_GAP };
        }
        let mut x = -row_width(row, sizes) * 0.5;
        let mut tallest = 0.0f32;
        for &g in row {
            corners[g] = [x, top];
            x += sizes[g][0] + GAP;
            tallest = tallest.max(sizes[g][1]);
        }
        top -= tallest;
        prev_layer = Some(*layer);
    }
    corners
}

fn row_width(row: &[usize], sizes: &[Vec2]) -> f32 {
    let boxes: f32 = row.iter().map(|&g| sizes[g][0]).sum();
    boxes + GAP * row.len().saturating_sub(1) as f32
}

fn extent(boxes: &[GroupBox]) -> (Vec2, Vec2) {
    let mut min = [f32::MAX, f32::MAX];
    let mut max = [f32::MIN, f32::MIN];
    for b in boxes {
        min = [min[0].min(b.min[0]), min[1].min(b.min[1])];
        max = [max[0].max(b.max[0]), max[1].max(b.max[1])];
    }
    (
        [min[0] - MARGIN, min[1] - MARGIN],
        [max[0] + MARGIN, max[1] + MARGIN],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeId, NodeKind, SourceSpan};

    fn func(g: &mut CodeGraph, qn: &str) -> NodeId {
        let file = g.intern_file("src/lib.rs");
        let (module, name) = qn.rsplit_once("::").unwrap();
        g.add_node(Node {
            id: NodeId(0),
            name: name.into(),
            qualified_name: qn.into(),
            module_path: module.into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(file, 1, 0, 2, 1),
            flags: Default::default(),
            stats: Default::default(),
        })
    }

    fn call(g: &mut CodeGraph, a: NodeId, b: NodeId) {
        let file = g.intern_file("src/lib.rs");
        g.add_edge(
            a,
            b,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(file, 1, 0, 1, 1)),
        );
    }

    /// `app` (3 fns) calls into `engine` (4) and `util` (1); `engine` calls
    /// `core` (6); everything prints through an external.
    fn workspace() -> CodeGraph {
        let mut g = CodeGraph::new();
        let main = func(&mut g, "app::main");
        let cli = func(&mut g, "app::cli::parse");
        let help = func(&mut g, "app::cli::help");
        let engine: Vec<_> = (0..4)
            .map(|i| func(&mut g, &format!("engine::e{i}")))
            .collect();
        let core: Vec<_> = (0..6)
            .map(|i| func(&mut g, &format!("core::graph::c{i}")))
            .collect();
        let util = func(&mut g, "util::fmt");
        let print = g.add_node(Node::external("std::println"));
        call(&mut g, main, cli);
        call(&mut g, cli, help);
        call(&mut g, main, engine[0]);
        call(&mut g, main, util);
        for w in engine.windows(2) {
            call(&mut g, w[0], w[1]);
        }
        for &c in &core {
            call(&mut g, engine[3], c);
        }
        call(&mut g, core[0], core[1]);
        call(&mut g, help, print);
        call(&mut g, util, print);
        g
    }

    fn fast() -> LayoutParams {
        LayoutParams {
            iterations: 60,
            ..Default::default()
        }
    }

    fn named<'a>(l: &'a Layout, name: &str) -> &'a GroupBox {
        l.groups.iter().find(|b| b.name == name).unwrap()
    }

    #[test]
    fn every_node_sits_in_its_crates_box_below_the_label() {
        let g = workspace();
        let l = layout_by_crate(&g, &fast());
        let groups = crate_groups(&g);
        assert_eq!(l.positions.len(), g.node_count());
        assert_eq!(l.groups.len(), groups.groups.len());
        for (i, &p) in l.positions.iter().enumerate() {
            let b = &l.groups[groups.of_node[i] as usize];
            assert!(b.contains(p), "node {i} at {p:?} outside {}", b.name);
            assert!(
                p[1] <= b.max[1] - GROUP_HEADER,
                "node {i} in {}'s label band",
                b.name
            );
            assert!(p[0] >= b.min[0] + PAD - 0.01 && p[0] <= b.max[0] - PAD + 0.01);
        }
        let members: u32 = l.groups.iter().map(|b| b.members).sum();
        assert_eq!(members as usize, g.node_count());
    }

    #[test]
    fn boxes_never_overlap() {
        let l = layout_by_crate(&workspace(), &fast());
        for (i, a) in l.groups.iter().enumerate() {
            for b in &l.groups[i + 1..] {
                let apart = a.max[0] <= b.min[0]
                    || b.max[0] <= a.min[0]
                    || a.max[1] <= b.min[1]
                    || b.max[1] <= a.min[1];
                assert!(apart, "{} overlaps {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn callers_sit_above_callees_and_externals_at_the_bottom() {
        let l = layout_by_crate(&workspace(), &fast());
        let (app, engine, core, util) = (
            named(&l, "app"),
            named(&l, "engine"),
            named(&l, "core"),
            named(&l, "util"),
        );
        let external = l.groups.last().unwrap();
        assert!(external.external);
        // Strictly below: each lower layer's top is under the upper one's bottom.
        assert!(engine.max[1] < app.min[1]);
        assert!(util.max[1] < app.min[1]);
        assert!(core.max[1] < engine.min[1]);
        for b in &l.groups[..l.groups.len() - 1] {
            assert!(external.max[1] < b.min[1], "external above {}", b.name);
        }
        // `engine` and `util` share layer 1: same row, tops aligned.
        assert_eq!(engine.max[1], util.max[1]);
    }

    #[test]
    fn the_diagram_extent_covers_every_box() {
        let l = layout_by_crate(&workspace(), &fast());
        for b in &l.groups {
            assert!(b.min[0] > l.min[0] && b.min[1] > l.min[1]);
            assert!(b.max[0] < l.max[0] && b.max[1] < l.max[1]);
            assert!(b.max[0] - b.min[0] >= MIN_WIDTH);
        }
    }

    #[test]
    fn a_wide_layer_wraps_instead_of_running_off() {
        // Twenty crates nothing calls: all layer 0, so they must wrap.
        let mut g = CodeGraph::new();
        for i in 0..20 {
            func(&mut g, &format!("c{i:02}::f"));
        }
        let l = layout_by_crate(&g, &fast());
        let rows: std::collections::BTreeSet<i64> =
            l.groups.iter().map(|b| b.max[1] as i64).collect();
        assert!(rows.len() > 1, "all 20 boxes in one row");
        let width = l.max[0] - l.min[0];
        let height = l.max[1] - l.min[1];
        assert!(width < height * 4.0, "{width} x {height} is a ribbon");
    }

    #[test]
    fn is_deterministic() {
        let g = workspace();
        let a = layout_by_crate(&g, &fast());
        let b = layout_by_crate(&g, &fast());
        assert_eq!(a.positions, b.positions);
        assert_eq!(a.groups, b.groups);
    }

    #[test]
    fn empty_graph_has_no_boxes() {
        let l = layout_by_crate(&CodeGraph::new(), &fast());
        assert!(l.positions.is_empty() && l.groups.is_empty());
    }

    #[test]
    fn the_plain_layout_has_no_boxes() {
        assert!(crate::layout(&workspace(), &fast()).groups.is_empty());
    }
}
