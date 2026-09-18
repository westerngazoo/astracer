//! # lcw-layout
//!
//! Force-directed 2D layout for the call graph (Fruchterman-Reingold with
//! cooling). Node repulsion is embarrassingly parallel and runs across cores
//! with `rayon`; edge attraction is a cheap sequential scatter.
//!
//! The layout is **deterministic**: initial positions come from a hash of the
//! node index, so the same graph always lays out the same way (reproducible
//! renders and tests). Data is kept in flat `Vec`s (Principle II).
//!
//! For very large graphs the O(n^2) repulsion is the bottleneck; the hardening
//! phase swaps in a Barnes-Hut / GPU-compute variant behind the same API.

use lcw_core::CodeGraph;
use rayon::prelude::*;

#[cfg(feature = "gpu")]
pub mod gpu;

#[cfg(feature = "gpu")]
pub use gpu::{layout_gpu, layout_gpu_or_cpu, GpuLayoutError};

/// A 2D point.
pub type Vec2 = [f32; 2];

/// Tunable layout parameters.
#[derive(Debug, Clone, Copy)]
pub struct LayoutParams {
    pub iterations: usize,
    /// Ideal edge length `k`.
    pub ideal_length: f32,
    /// Repulsion strength multiplier.
    pub repulsion: f32,
    /// Pull toward the origin so disconnected parts don't drift away.
    pub gravity: f32,
    /// Initial temperature (max node movement per step); cools linearly to 0.
    pub temperature: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            iterations: 300,
            ideal_length: 40.0,
            repulsion: 1.0,
            gravity: 0.02,
            temperature: 200.0,
        }
    }
}

/// Result of a layout run: one position per node, indexed by `NodeId.0`, plus
/// the axis-aligned bounding box `(min, max)`.
#[derive(Debug, Clone)]
pub struct Layout {
    pub positions: Vec<Vec2>,
    pub min: Vec2,
    pub max: Vec2,
}

impl Layout {
    /// Width/height of the laid-out graph.
    pub fn extent(&self) -> Vec2 {
        [self.max[0] - self.min[0], self.max[1] - self.min[1]]
    }

    pub fn center(&self) -> Vec2 {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }
}

/// Compute a force-directed layout for `graph`.
pub fn layout(graph: &CodeGraph, params: &LayoutParams) -> Layout {
    let n = graph.node_count();
    if n == 0 {
        return Layout {
            positions: Vec::new(),
            min: [0.0, 0.0],
            max: [0.0, 0.0],
        };
    }

    let edges: Vec<(usize, usize)> = graph
        .edges()
        .map(|(a, b, _)| (a.0 as usize, b.0 as usize))
        .filter(|(a, b)| a != b && *a < n && *b < n)
        .collect();

    let k = params.ideal_length.max(1.0);
    let mut pos = seed_positions(n, k);

    let mut temp = params.temperature;
    let cooling = params.temperature / (params.iterations.max(1) as f32);

    for _ in 0..params.iterations {
        // Repulsion: parallel over nodes, O(n^2) reads of shared positions.
        let mut disp: Vec<Vec2> = (0..n)
            .into_par_iter()
            .map(|i| {
                let pi = pos[i];
                let mut dx = 0.0f32;
                let mut dy = 0.0f32;
                for (j, pj) in pos.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    let mut ex = pi[0] - pj[0];
                    let mut ey = pi[1] - pj[1];
                    let mut d2 = ex * ex + ey * ey;
                    if d2 < 1e-4 {
                        // Deterministic nudge for coincident nodes.
                        ex = ((i * 31 + j) % 7) as f32 - 3.0;
                        ey = ((i * 17 + j) % 5) as f32 - 2.0;
                        d2 = ex * ex + ey * ey + 1e-3;
                    }
                    let dist = d2.sqrt();
                    let force = params.repulsion * (k * k) / dist;
                    dx += ex / dist * force;
                    dy += ey / dist * force;
                }
                [dx, dy]
            })
            .collect();

        // Attraction along edges (sequential scatter).
        for &(a, b) in &edges {
            let ex = pos[a][0] - pos[b][0];
            let ey = pos[a][1] - pos[b][1];
            let dist = (ex * ex + ey * ey).sqrt().max(1e-3);
            let force = (dist * dist) / k;
            let fx = ex / dist * force;
            let fy = ey / dist * force;
            disp[a][0] -= fx;
            disp[a][1] -= fy;
            disp[b][0] += fx;
            disp[b][1] += fy;
        }

        // Gravity + integrate with a temperature cap.
        for i in 0..n {
            disp[i][0] -= pos[i][0] * params.gravity * k;
            disp[i][1] -= pos[i][1] * params.gravity * k;

            let len = (disp[i][0] * disp[i][0] + disp[i][1] * disp[i][1])
                .sqrt()
                .max(1e-6);
            let capped = len.min(temp);
            pos[i][0] += disp[i][0] / len * capped;
            pos[i][1] += disp[i][1] / len * capped;
        }

        temp = (temp - cooling).max(0.0);
    }

    let (min, max) = bounds(&pos);
    Layout {
        positions: pos,
        min,
        max,
    }
}

/// Deterministic initial placement on a phyllotaxis (sunflower) spiral so
/// nodes start well spread out. Shared with the GPU backend so both seed
/// identically.
pub(crate) fn seed_positions(n: usize, k: f32) -> Vec<Vec2> {
    const GOLDEN_ANGLE: f32 = 2.399963; // radians
    let radius_step = k * 0.75;
    (0..n)
        .map(|i| {
            let r = radius_step * (i as f32).sqrt();
            let theta = i as f32 * GOLDEN_ANGLE;
            [r * theta.cos(), r * theta.sin()]
        })
        .collect()
}

pub(crate) fn bounds(pos: &[Vec2]) -> (Vec2, Vec2) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, SourceSpan};

    fn line_graph(n: usize) -> CodeGraph {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let mut ids = Vec::new();
        for i in 0..n {
            ids.push(g.add_node(Node::external(format!("f{i}"))));
        }
        for w in ids.windows(2) {
            g.add_edge(
                w[0],
                w[1],
                Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
            );
        }
        g
    }

    #[test]
    fn produces_one_position_per_node() {
        let g = line_graph(20);
        let l = layout(
            &g,
            &LayoutParams {
                iterations: 50,
                ..Default::default()
            },
        );
        assert_eq!(l.positions.len(), 20);
        assert!(l
            .positions
            .iter()
            .all(|p| p[0].is_finite() && p[1].is_finite()));
    }

    #[test]
    fn is_deterministic() {
        let g = line_graph(30);
        let p = LayoutParams {
            iterations: 40,
            ..Default::default()
        };
        let a = layout(&g, &p);
        let b = layout(&g, &p);
        assert_eq!(a.positions, b.positions);
    }

    #[test]
    fn empty_graph_is_ok() {
        let g = CodeGraph::new();
        let l = layout(&g, &LayoutParams::default());
        assert!(l.positions.is_empty());
    }
}
