//! Turn a [`CodeGraph`] + [`Layout`] into flat GPU-ready buffers
//! (Principle II: contiguous, `Pod` arrays uploaded straight to the GPU).

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
#[derive(Debug, Clone, Default)]
pub struct SceneData {
    pub nodes: Vec<NodeInstance>,
    pub edges: Vec<EdgeVertex>,
    pub min: [f32; 2],
    pub max: [f32; 2],
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

/// Build GPU scene data from a graph and one 2D position per node (indexed by
/// `NodeId.0`). Keeping layout *out* of the renderer means the renderer has no
/// `rayon` dependency and so compiles cleanly to wasm (Principle I).
///
/// Node radius grows with fan-in; color shifts toward red as cyclomatic
/// complexity rises (a visual "hotspot" cue).
pub fn build(graph: &CodeGraph, positions: &[[f32; 2]]) -> SceneData {
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
    for (from, to, edge) in graph.edges() {
        let a = positions.get(from.0 as usize).copied();
        let b = positions.get(to.0 as usize).copied();
        if let (Some(a), Some(b)) = (a, b) {
            let c = edge_color(edge.kind);
            edges.push(EdgeVertex { pos: a, color: c });
            edges.push(EdgeVertex { pos: b, color: c });
        }
    }

    let (min, max) = bounds(positions);
    SceneData {
        nodes,
        edges,
        min,
        max,
    }
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
