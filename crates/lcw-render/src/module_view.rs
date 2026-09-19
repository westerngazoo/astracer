//! The **module view**: collapse the function-level call graph into one node per
//! module, grouped into a labelled rectangle per crate, and lay the crates out
//! as a wrapped grid of boxed clusters. This is the "architecture at a glance"
//! surface — a few dozen labelled nodes instead of a thousand anonymous dots.
//!
//! It is pure presentation built on top of [`CodeGraph`]: aggregation and a
//! deterministic grid layout live here (not in the layout crate) because the
//! result is a *diagram*, not a force simulation. External / unresolved targets
//! are dropped so the picture is about the code you own.

use std::collections::HashMap;

use lcw_core::{CodeGraph, NodeKind};

use crate::scene::{EdgeVertex, NodeInstance, SceneData};
use crate::text;

/// Tunables for [`build`]. Defaults produce a readable ~1800-unit-wide diagram.
#[derive(Debug, Clone, Copy)]
pub struct ModuleViewOptions {
    /// Approximate world width before crate boxes wrap to the next row.
    pub target_width: f32,
    /// Draw crate + module text labels.
    pub labels: bool,
}

impl Default for ModuleViewOptions {
    fn default() -> Self {
        ModuleViewOptions {
            target_width: 1800.0,
            labels: true,
        }
    }
}

// Layout constants, in world units.
const CELL: f32 = 140.0; // space allotted to one module (grid pitch)
const PAD: f32 = 22.0; // inner padding inside a crate box
const HEADER: f32 = 54.0; // vertical room for the crate label
const MARGIN: f32 = 80.0; // gap between crate boxes
const CRATE_LABEL_PX: f32 = 6.0;
/// Minimum px a crate header is allowed to shrink to; boxes are widened so the
/// header always fits at this size (no overflow).
const HEADER_FIT_PX: f32 = 3.0;

struct Module {
    path: String,
    crate_name: String,
    label: String,
    functions: u32,
}

/// Build a module-view [`SceneData`] plus a per-node pick label (the module
/// path), aligned with `NodeInstance` order so the window's click-to-pick still
/// reports something meaningful.
pub fn build(graph: &CodeGraph, opts: &ModuleViewOptions) -> (SceneData, Vec<String>) {
    let (modules, node_module) = aggregate_modules(graph);
    let edges = aggregate_edges(graph, &node_module);

    // Group module indices by crate, crates sorted by size (desc, then name).
    let mut by_crate: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        by_crate.entry(m.crate_name.as_str()).or_default().push(i);
    }
    let mut crates: Vec<(&str, Vec<usize>)> = by_crate.into_iter().collect();
    for (_, mods) in &mut crates {
        mods.sort_by(|&a, &b| {
            modules[b]
                .functions
                .cmp(&modules[a].functions)
                .then(modules[a].label.cmp(&modules[b].label))
        });
    }
    crates.sort_by(|a, b| {
        let fa: u32 = a.1.iter().map(|&i| modules[i].functions).sum();
        let fb: u32 = b.1.iter().map(|&i| modules[i].functions).sum();
        fb.cmp(&fa).then(a.0.cmp(b.0))
    });

    // Position every module and remember each crate's box rectangle.
    let mut positions = vec![[0.0f32; 2]; modules.len()];
    let mut boxes: Vec<CrateBox> = Vec::with_capacity(crates.len());

    let mut x_cursor = 0.0f32;
    let mut y_cursor = 0.0f32;
    let mut row_height = 0.0f32;
    for (crate_name, mods) in &crates {
        let m = mods.len().max(1);
        let cols = (m as f32).sqrt().ceil() as usize;
        let rows = m.div_ceil(cols);
        let inner_w = cols as f32 * CELL;
        let inner_h = rows as f32 * CELL;
        // Widen the box if the header (at its minimum size) is wider than the grid.
        let functions: u32 = mods.iter().map(|&i| modules[i].functions).sum();
        let header_w = text::width(&header_text(crate_name, functions), HEADER_FIT_PX) + 2.0 * PAD;
        let box_w = (inner_w + 2.0 * PAD).max(header_w);
        let box_h = inner_h + 2.0 * PAD + HEADER;

        if x_cursor > 0.0 && x_cursor + box_w > opts.target_width {
            x_cursor = 0.0;
            y_cursor -= row_height + MARGIN;
            row_height = 0.0;
        }
        let left = x_cursor;
        let top = y_cursor;

        for (k, &mi) in mods.iter().enumerate() {
            let col = k % cols;
            let row = k / cols;
            let cx = left + PAD + col as f32 * CELL + CELL * 0.5;
            let cy = top - HEADER - PAD - (row as f32 * CELL + CELL * 0.5);
            positions[mi] = [cx, cy];
        }

        boxes.push(CrateBox {
            name: crate_name.to_string(),
            min: [left, top - box_h],
            max: [left + box_w, top],
            functions,
        });

        x_cursor += box_w + MARGIN;
        row_height = row_height.max(box_h);
    }

    // ---- Emit geometry ------------------------------------------------------
    let mut scene = SceneData::default();

    // Module edges (behind boxes would be hidden; draw as graph edges).
    for (&(a, b), &w) in &edges {
        let pa = positions[a];
        let pb = positions[b];
        let alpha = (0.07 + w as f32 * 0.02).min(0.42);
        let color = [0.64, 0.68, 0.82, alpha];
        scene.edges.push(EdgeVertex { pos: pa, color });
        scene.edges.push(EdgeVertex { pos: pb, color });
        scene.edge_nodes.push([a as u32, b as u32]);
    }

    // Crate boxes: translucent fill + crisp border + header label.
    for b in &boxes {
        let rgb = crate_color(&b.name);
        push_rect_fill(
            &mut scene.group_fills,
            b.min,
            b.max,
            [rgb[0], rgb[1], rgb[2], 0.10],
        );
        push_rect_outline(
            &mut scene.group_outlines,
            b.min,
            b.max,
            [rgb[0], rgb[1], rgb[2], 0.85],
        );
        if opts.labels {
            let header = header_text(&b.name, b.functions);
            let avail = (b.max[0] - b.min[0]) - 2.0 * PAD;
            let full = text::width(&header, 1.0).max(1.0);
            let px = (avail / full).clamp(HEADER_FIT_PX, CRATE_LABEL_PX);
            text::emit(
                &mut scene.labels,
                &header,
                [b.min[0] + PAD, b.max[1] - PAD * 0.5],
                px,
                [0.97, 0.98, 1.0, 1.0],
            );
        }
    }

    // Module nodes + labels, in index order so pick ids line up.
    let mut pick_labels = vec![String::new(); modules.len()];
    for (i, m) in modules.iter().enumerate() {
        let pos = positions[i];
        let rgb = crate_color(&m.crate_name);
        let radius = (10.0 + (m.functions as f32).sqrt() * 4.0).clamp(10.0, CELL * 0.42);
        scene.nodes.push(NodeInstance {
            center: pos,
            radius,
            color: [rgb[0], rgb[1], rgb[2], 1.0],
            pick_id: (i as u32) + 1,
        });
        pick_labels[i] = m.path.clone();

        if opts.labels {
            let label = clip_label(&m.label);
            // Shrink the label so it never spills past its grid cell.
            let unit = text::width(&label, 1.0).max(1.0);
            let px = (CELL * 0.9 / unit).clamp(1.6, 2.8);
            let w = text::width(&label, px);
            text::emit(
                &mut scene.labels,
                &label,
                [pos[0] - w * 0.5, pos[1] - radius - 6.0],
                px,
                [0.94, 0.96, 1.0, 1.0],
            );
        }
    }

    let (min, max) = bounds(&boxes);
    scene.min = min;
    scene.max = max;
    (scene, pick_labels)
}

struct CrateBox {
    name: String,
    min: [f32; 2],
    max: [f32; 2],
    functions: u32,
}

/// Collapse nodes into modules. Returns the module table and, indexed by
/// `NodeId.0`, the module each node maps to (`None` = external/unresolved,
/// which is excluded from the view).
fn aggregate_modules(graph: &CodeGraph) -> (Vec<Module>, Vec<Option<usize>>) {
    let mut modules: Vec<Module> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut node_module: Vec<Option<usize>> = vec![None; graph.node_count()];

    for node in graph.nodes() {
        let idx = node.id.0 as usize;
        if node.kind == NodeKind::External || node.module_path.is_empty() {
            continue;
        }
        let mi = *index.entry(node.module_path.clone()).or_insert_with(|| {
            let crate_name = node
                .module_path
                .split("::")
                .next()
                .unwrap_or(&node.module_path)
                .to_string();
            let label = node
                .module_path
                .rsplit("::")
                .next()
                .unwrap_or(&node.module_path)
                .to_string();
            modules.push(Module {
                path: node.module_path.clone(),
                crate_name,
                label,
                functions: 0,
            });
            modules.len() - 1
        });
        modules[mi].functions += 1;
        if idx < node_module.len() {
            node_module[idx] = Some(mi);
        }
    }
    (modules, node_module)
}

/// Aggregate cross-module call counts (self-module calls are omitted: they live
/// *inside* a node, not between them).
fn aggregate_edges(
    graph: &CodeGraph,
    node_module: &[Option<usize>],
) -> HashMap<(usize, usize), u32> {
    let mut edges: HashMap<(usize, usize), u32> = HashMap::new();
    for (from, to, _edge) in graph.edges() {
        let (Some(&Some(a)), Some(&Some(b))) = (
            node_module.get(from.0 as usize),
            node_module.get(to.0 as usize),
        ) else {
            continue;
        };
        if a != b {
            *edges.entry((a, b)).or_insert(0) += 1;
        }
    }
    edges
}

fn header_text(crate_name: &str, functions: u32) -> String {
    format!("{crate_name}  {functions}fn")
}

fn clip_label(s: &str) -> String {
    const MAX: usize = 16;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX - 1).collect();
        out.push('~');
        out
    }
}

fn push_rect_fill(out: &mut Vec<EdgeVertex>, min: [f32; 2], max: [f32; 2], color: [f32; 4]) {
    let v = |x: f32, y: f32| EdgeVertex { pos: [x, y], color };
    out.push(v(min[0], max[1]));
    out.push(v(max[0], max[1]));
    out.push(v(max[0], min[1]));
    out.push(v(min[0], max[1]));
    out.push(v(max[0], min[1]));
    out.push(v(min[0], min[1]));
}

fn push_rect_outline(out: &mut Vec<EdgeVertex>, min: [f32; 2], max: [f32; 2], color: [f32; 4]) {
    let v = |x: f32, y: f32| EdgeVertex { pos: [x, y], color };
    let tl = v(min[0], max[1]);
    let tr = v(max[0], max[1]);
    let br = v(max[0], min[1]);
    let bl = v(min[0], min[1]);
    out.extend_from_slice(&[tl, tr, tr, br, br, bl, bl, tl]);
}

fn bounds(boxes: &[CrateBox]) -> ([f32; 2], [f32; 2]) {
    if boxes.is_empty() {
        return ([0.0, 0.0], [1.0, 1.0]);
    }
    let mut min = [f32::MAX, f32::MAX];
    let mut max = [f32::MIN, f32::MIN];
    for b in boxes {
        min[0] = min[0].min(b.min[0]);
        min[1] = min[1].min(b.min[1]);
        max[0] = max[0].max(b.max[0]);
        max[1] = max[1].max(b.max[1]);
    }
    let pad = 60.0;
    ([min[0] - pad, min[1] - pad], [max[0] + pad, max[1] + pad])
}

/// Deterministic, well-spread color per crate (FNV-1a hash -> HSV hue).
pub(crate) fn crate_color(name: &str) -> [f32; 3] {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let hue = (h % 360) as f32;
    hsv_to_rgb(hue, 0.52, 0.95)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeId, SourceSpan};

    fn func(id: u32, module: &str, name: &str) -> Node {
        Node {
            id: NodeId(id),
            name: name.into(),
            qualified_name: format!("{module}::{name}"),
            module_path: module.into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(lcw_core::FileId(0), 1, 0, 2, 1),
            flags: Default::default(),
            stats: Default::default(),
        }
    }

    #[test]
    fn aggregates_modules_and_cross_module_edges() {
        let mut g = CodeGraph::new();
        g.intern_file("src/lib.rs");
        let a = g.add_node(func(0, "krate::m1", "a"));
        let b = g.add_node(func(1, "krate::m1", "b")); // same module as a
        let c = g.add_node(func(2, "krate::m2", "c"));
        g.add_edge(
            a,
            c,
            Edge::new(
                EdgeKind::DirectCall,
                SourceSpan::new(lcw_core::FileId(0), 1, 0, 1, 1),
            ),
        );
        g.add_edge(
            b,
            c,
            Edge::new(
                EdgeKind::DirectCall,
                SourceSpan::new(lcw_core::FileId(0), 1, 0, 1, 1),
            ),
        );
        // intra-module a->b should NOT create a module edge.
        g.add_edge(
            a,
            b,
            Edge::new(
                EdgeKind::DirectCall,
                SourceSpan::new(lcw_core::FileId(0), 1, 0, 1, 1),
            ),
        );

        let (modules, node_module) = aggregate_modules(&g);
        assert_eq!(modules.len(), 2);
        assert_eq!(node_module[0], node_module[1]); // a,b same module
        assert_ne!(node_module[0], node_module[2]);

        let edges = aggregate_edges(&g, &node_module);
        // m1 -> m2 aggregated from a->c and b->c = weight 2; no self edge.
        assert_eq!(edges.len(), 1);
        assert_eq!(*edges.values().next().unwrap(), 2);
    }

    #[test]
    fn external_nodes_are_excluded() {
        let mut g = CodeGraph::new();
        g.add_node(func(0, "krate::m1", "a"));
        g.add_node(Node::external("std::mem::swap"));
        let (modules, node_module) = aggregate_modules(&g);
        assert_eq!(modules.len(), 1);
        assert_eq!(node_module[1], None);
    }

    #[test]
    fn build_produces_boxes_nodes_and_labels() {
        let mut g = CodeGraph::new();
        g.add_node(func(0, "krate::m1", "a"));
        g.add_node(func(1, "krate::m2", "b"));
        g.add_node(func(2, "other::core", "c"));
        let (scene, picks) = build(&g, &ModuleViewOptions::default());
        assert_eq!(scene.nodes.len(), 3);
        assert_eq!(picks.len(), 3);
        // two crates -> two boxes -> 12 fill verts, 16 outline verts.
        assert_eq!(scene.group_fills.len(), 12);
        assert_eq!(scene.group_outlines.len(), 16);
        assert!(!scene.labels.is_empty());
        assert!(scene.max[0] > scene.min[0] && scene.max[1] > scene.min[1]);
    }

    #[test]
    fn hsv_primary_red() {
        let rgb = hsv_to_rgb(0.0, 1.0, 1.0);
        assert!((rgb[0] - 1.0).abs() < 1e-6 && rgb[1] < 1e-6 && rgb[2] < 1e-6);
    }
}
