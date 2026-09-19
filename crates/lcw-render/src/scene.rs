//! Turn a [`CodeGraph`] + [`Layout`] into flat GPU-ready buffers
//! (Principle II: contiguous, `Pod` arrays uploaded straight to the GPU).

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use lcw_core::{CodeGraph, EdgeKind, NodeKind};

/// Per-node instance data for the node pipeline (also carries the pick id).
/// Tightly packed (no padding) so `vertex_attr_array!` offsets line up.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct NodeInstance {
    pub center: [f32; 2],
    pub radius: f32,
    pub color: [f32; 4],
    /// Pick id = node index + 1 (0 is reserved for "background").
    pub pick_id: u32,
}

/// A single vertex of an edge (rendered as a line-list).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct EdgeVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

/// CPU-side scene data ready to upload.
///
/// `edges` is a flat line-list (two [`EdgeVertex`]es per segment). `edge_nodes`
/// records the `[from, to]` node indices that *own* each segment, one entry per
/// segment, so downstream passes (search dimming, hover) can reason about which
/// nodes an edge connects even after it has been tessellated into a curved
/// bundle. Invariant: `edge_nodes.len() * 2 == edges.len()`.
///
/// `group_fills`, `group_outlines`, and `labels` back the *module view* overlay:
/// translucent grouping rectangles (a triangle-list drawn behind the graph),
/// their crisp borders (a line-list), and bitmap text glyphs (a triangle-list
/// drawn on top). All three are empty for the classic function-level scene, so
/// existing callers are byte-for-byte unaffected (Principle I: opt-in).
#[derive(Debug, Clone, Default)]
pub struct SceneData {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<EdgeVertex>,
    pub edge_nodes: Vec<[u32; 2]>,
    /// Filled grouping rectangles, as a triangle-list (6 verts per rect).
    pub group_fills: Vec<EdgeVertex>,
    /// Grouping-rectangle borders, as a line-list (8 verts per rect).
    pub group_outlines: Vec<EdgeVertex>,
    /// Text glyph pixels, as a triangle-list (6 verts per lit pixel).
    pub labels: Vec<EdgeVertex>,
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// Opt-in curved edge bundling: route each edge through a control point pulled
/// toward the centroid(s) of the module(s) it connects, then tessellate the
/// resulting quadratic Bézier into `segments` straight pieces.
#[derive(Debug, Clone, Copy)]
pub struct BundleOptions {
    /// How strongly to pull an edge toward its module centroid, in `0.0..=1.0`
    /// (0 = straight, 1 = through the centroid midpoint).
    pub strength: f32,
    /// Number of straight pieces each edge is tessellated into (`>= 1`). Higher
    /// is smoother but heavier.
    pub segments: u32,
}

impl Default for BundleOptions {
    fn default() -> Self {
        BundleOptions {
            strength: 0.85,
            segments: 12,
        }
    }
}

/// Knobs for [`build_with`]. Defaults reproduce the classic straight-edge scene
/// so existing callers and tests are unaffected.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneOptions {
    /// When `Some`, edges are bundled into curves; when `None`, straight lines.
    pub bundle: Option<BundleOptions>,
}

fn node_base_color(kind: NodeKind) -> [f32; 3] {
    match kind {
        NodeKind::Function => [0.30, 0.55, 0.95],
        NodeKind::Method => [0.25, 0.75, 0.72],
        NodeKind::Closure => [0.62, 0.45, 0.90],
        NodeKind::External => [0.45, 0.45, 0.50],
    }
}

fn edge_color(kind: EdgeKind) -> [f32; 4] {
    match kind {
        EdgeKind::DirectCall => [0.70, 0.72, 0.78, 0.55],
        EdgeKind::MethodCall => [0.25, 0.75, 0.72, 0.55],
        EdgeKind::AssociatedCall => [0.35, 0.60, 0.95, 0.55],
        EdgeKind::MacroCall => [0.95, 0.65, 0.25, 0.55],
        EdgeKind::TraitDispatch => [0.85, 0.45, 0.85, 0.55],
        EdgeKind::Unresolved => [0.40, 0.40, 0.45, 0.30],
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Build GPU scene data from a graph and one 2D position per node with the
/// default (straight-edge) options. Keeping layout *out* of the renderer means
/// the renderer has no `rayon` dependency and so compiles cleanly to wasm
/// (Principle I).
///
/// Node radius grows with fan-in; color shifts toward red as cyclomatic
/// complexity rises (a visual "hotspot" cue).
pub fn build(graph: &CodeGraph, positions: &[[f32; 2]]) -> SceneData {
    build_with(graph, positions, &SceneOptions::default())
}

/// Build GPU scene data with explicit [`SceneOptions`] (e.g. edge bundling).
///
/// With `SceneOptions::default()` the output is byte-for-byte the same scene as
/// [`build`] produced historically (straight edges, two vertices each), so the
/// bundling path is strictly opt-in.
pub fn build_with(graph: &CodeGraph, positions: &[[f32; 2]], opts: &SceneOptions) -> SceneData {
    let n = graph.node_count();
    let mut nodes = Vec::with_capacity(n);

    for (i, node) in graph.nodes().enumerate() {
        let pos = positions.get(i).copied().unwrap_or([0.0, 0.0]);
        let fan_in = graph.in_degree(node.id) as f32;
        let radius = 4.0 + fan_in.sqrt() * 2.0;

        let hot = (node.cyclomatic_complexity() as f32 / 20.0).clamp(0.0, 1.0);
        let base = node_base_color(node.kind);
        let rgb = lerp3(base, [0.95, 0.25, 0.25], hot);

        nodes.push(NodeInstance {
            center: pos,
            radius,
            color: [rgb[0], rgb[1], rgb[2], 1.0],
            pick_id: (i as u32) + 1,
        });
    }

    let mut edges = Vec::with_capacity(graph.edge_count() * 2);
    let mut edge_nodes = Vec::with_capacity(graph.edge_count());

    let centroids = opts.bundle.map(|_| module_centroids(graph, positions));

    for (from, to, edge) in graph.edges() {
        let a = positions.get(from.0 as usize).copied();
        let b = positions.get(to.0 as usize).copied();
        let (Some(a), Some(b)) = (a, b) else { continue };
        let color = edge_color(edge.kind);
        let pair = [from.0, to.0];

        match (&opts.bundle, &centroids) {
            (Some(bundle), Some(centroids)) => {
                let control = bundle_control(graph, &from, &to, a, b, centroids, bundle.strength);
                let seg = bundle.segments.max(1);
                let mut prev = a;
                for s in 1..=seg {
                    let t = s as f32 / seg as f32;
                    let next = quad_bezier(a, control, b, t);
                    edges.push(EdgeVertex { pos: prev, color });
                    edges.push(EdgeVertex { pos: next, color });
                    edge_nodes.push(pair);
                    prev = next;
                }
            }
            _ => {
                edges.push(EdgeVertex { pos: a, color });
                edges.push(EdgeVertex { pos: b, color });
                edge_nodes.push(pair);
            }
        }
    }

    let (min, max) = bounds(positions);
    SceneData {
        nodes,
        edges,
        edge_nodes,
        min,
        max,
        ..Default::default()
    }
}

/// Average node position per `module_path`. External nodes (empty module) fall
/// into a single shared bucket, which is fine: they are pulled toward a common
/// "external" gravity well, further separating them from real code.
fn module_centroids(graph: &CodeGraph, positions: &[[f32; 2]]) -> HashMap<String, [f32; 2]> {
    let mut acc: HashMap<String, ([f32; 2], u32)> = HashMap::new();
    for (i, node) in graph.nodes().enumerate() {
        let Some(p) = positions.get(i).copied() else {
            continue;
        };
        let entry = acc
            .entry(node.module_path.clone())
            .or_insert(([0.0, 0.0], 0));
        entry.0[0] += p[0];
        entry.0[1] += p[1];
        entry.1 += 1;
    }
    acc.into_iter()
        .map(|(k, (sum, count))| {
            let c = count.max(1) as f32;
            (k, [sum[0] / c, sum[1] / c])
        })
        .collect()
}

/// Control point for the quadratic Bézier of one edge: the straight midpoint
/// pulled `strength` of the way toward the midpoint of the two endpoints'
/// module centroids. This is what makes edges sharing modules fan into a
/// common bundle.
fn bundle_control(
    graph: &CodeGraph,
    from: &lcw_core::NodeId,
    to: &lcw_core::NodeId,
    a: [f32; 2],
    b: [f32; 2],
    centroids: &HashMap<String, [f32; 2]>,
    strength: f32,
) -> [f32; 2] {
    let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let ca = module_centroid_of(graph, *from, centroids).unwrap_or(mid);
    let cb = module_centroid_of(graph, *to, centroids).unwrap_or(mid);
    let meta = [(ca[0] + cb[0]) * 0.5, (ca[1] + cb[1]) * 0.5];
    let s = strength.clamp(0.0, 1.0);
    [
        mid[0] + (meta[0] - mid[0]) * s,
        mid[1] + (meta[1] - mid[1]) * s,
    ]
}

fn module_centroid_of(
    graph: &CodeGraph,
    id: lcw_core::NodeId,
    centroids: &HashMap<String, [f32; 2]>,
) -> Option<[f32; 2]> {
    let node = graph.try_node(id)?;
    centroids.get(&node.module_path).copied()
}

/// Quadratic Bézier point at parameter `t` in `[0, 1]`.
fn quad_bezier(a: [f32; 2], c: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let w0 = u * u;
    let w1 = 2.0 * u * t;
    let w2 = t * t;
    [
        w0 * a[0] + w1 * c[0] + w2 * b[0],
        w0 * a[1] + w1 * c[1] + w2 * b[1],
    ]
}

fn bounds(positions: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    if positions.is_empty() {
        return ([0.0, 0.0], [0.0, 0.0]);
    }
    let mut min = [f32::MAX, f32::MAX];
    let mut max = [f32::MIN, f32::MIN];
    for p in positions {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, NodeId, SourceSpan};

    /// Two real nodes in different modules plus one edge between them.
    fn two_module_graph() -> (CodeGraph, Vec<[f32; 2]>) {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let a = g.add_node(Node {
            id: NodeId(0),
            name: "a".into(),
            qualified_name: "crate::m1::a".into(),
            module_path: "crate::m1".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, 1, 0, 2, 1),
            flags: Default::default(),
            stats: Default::default(),
        });
        let b = g.add_node(Node {
            id: NodeId(1),
            name: "b".into(),
            qualified_name: "crate::m2::b".into(),
            module_path: "crate::m2".into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, 3, 0, 4, 1),
            flags: Default::default(),
            stats: Default::default(),
        });
        g.add_edge(
            a,
            b,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
        );
        (g, vec![[-10.0, 0.0], [10.0, 20.0]])
    }

    /// Module `m1` has two nodes (so its centroid is offset from either one),
    /// module `m2` has one; a single edge `m1::a1 -> m2::b` should bend toward
    /// the midpoint of the two module centroids.
    fn bundling_graph() -> (CodeGraph, Vec<[f32; 2]>) {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let mk = |name: &str, module: &str| Node {
            id: NodeId(0),
            name: name.into(),
            qualified_name: format!("{module}::{name}"),
            module_path: module.into(),
            kind: NodeKind::Function,
            span: SourceSpan::new(f, 1, 0, 2, 1),
            flags: Default::default(),
            stats: Default::default(),
        };
        let a1 = g.add_node(mk("a1", "crate::m1"));
        let _a2 = g.add_node(mk("a2", "crate::m1"));
        let b = g.add_node(mk("b", "crate::m2"));
        g.add_edge(
            a1,
            b,
            Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
        );
        // Node indices: a1=0, a2=1, b=2. centroid(m1)=[-10,-5], centroid(m2)=[10,20].
        (g, vec![[-10.0, 0.0], [-10.0, -10.0], [10.0, 20.0]])
    }

    #[test]
    fn default_build_is_straight_two_vertices_per_edge() {
        let (g, pos) = two_module_graph();
        let scene = build(&g, &pos);
        assert_eq!(scene.nodes.len(), 2);
        // One edge -> exactly two vertices, endpoints equal to node positions.
        assert_eq!(scene.edges.len(), 2);
        assert_eq!(scene.edge_nodes.len(), 1);
        assert_eq!(scene.edge_nodes[0], [0, 1]);
        assert_eq!(scene.edges[0].pos, pos[0]);
        assert_eq!(scene.edges[1].pos, pos[1]);
    }

    #[test]
    fn edge_nodes_invariant_holds() {
        let (g, pos) = two_module_graph();
        let straight = build(&g, &pos);
        assert_eq!(straight.edge_nodes.len() * 2, straight.edges.len());

        let bundled = build_with(
            &g,
            &pos,
            &SceneOptions {
                bundle: Some(BundleOptions {
                    strength: 0.9,
                    segments: 10,
                }),
            },
        );
        assert_eq!(bundled.edge_nodes.len() * 2, bundled.edges.len());
    }

    #[test]
    fn bundled_edge_tessellates_and_bends() {
        let (g, pos) = bundling_graph();
        let (a, b) = (pos[0], pos[2]); // edge a1(0) -> b(2)
        let seg = 8u32;
        let bundled = build_with(
            &g,
            &pos,
            &SceneOptions {
                bundle: Some(BundleOptions {
                    strength: 1.0,
                    segments: seg,
                }),
            },
        );
        // `seg` segments -> 2*seg vertices, `seg` ownership entries.
        assert_eq!(bundled.edges.len(), (seg * 2) as usize);
        assert_eq!(bundled.edge_nodes.len(), seg as usize);
        for pair in &bundled.edge_nodes {
            assert_eq!(*pair, [0, 2]);
        }
        // Bézier endpoints are exact.
        assert!(dist(bundled.edges.first().unwrap().pos, a) < 1e-4);
        assert!(dist(bundled.edges.last().unwrap().pos, b) < 1e-4);

        // The curve's own midpoint is pulled away from the straight midpoint.
        let straight_mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let curve_mid = quad_bezier(
            a,
            bundle_control(
                &g,
                &NodeId(0),
                &NodeId(2),
                a,
                b,
                &module_centroids(&g, &pos),
                1.0,
            ),
            b,
            0.5,
        );
        assert!(dist(curve_mid, straight_mid) > 1.0);
    }

    #[test]
    fn zero_strength_bundle_stays_collinear() {
        let (g, pos) = two_module_graph();
        let (a, b) = (pos[0], pos[1]);
        let bundled = build_with(
            &g,
            &pos,
            &SceneOptions {
                bundle: Some(BundleOptions {
                    strength: 0.0,
                    segments: 6,
                }),
            },
        );
        // strength 0 => control == straight midpoint => every tessellated vertex
        // stays exactly on the a→b line (zero cross product).
        assert_eq!(bundled.edges.len(), 12);
        assert!(dist(bundled.edges.first().unwrap().pos, a) < 1e-4);
        assert!(dist(bundled.edges.last().unwrap().pos, b) < 1e-4);
        for v in &bundled.edges {
            let cross = (b[0] - a[0]) * (v.pos[1] - a[1]) - (b[1] - a[1]) * (v.pos[0] - a[0]);
            assert!(cross.abs() < 1e-3, "vertex off-line: {:?}", v.pos);
        }
    }

    fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
    }
}
