//! Barnes-Hut approximation of the all-pairs repulsion force.
//!
//! The exact repulsion in [`crate::layout`] is `O(n^2)`: every node reads every
//! other node each iteration. That is fine for small graphs but quadratic blowup
//! on the "repos gigantes" the Manifesto targets. Barnes-Hut builds a quadtree
//! over the current positions and, for a body far from a cell, treats the whole
//! cell as a single pseudo-body at its centre of mass — turning the per-node cost
//! into `O(log n)` and the whole pass into `O(n log n)`.
//!
//! It is **deterministic**: the tree is built by inserting bodies in index order
//! into a fixed spatial subdivision, and forces are summed by walking children in
//! a fixed order, so the same positions always yield the same forces (matching the
//! reproducibility guarantee of the exact path).
//!
//! Accuracy is controlled by [`THETA`]; the force law and softening match the
//! exact path so the two produce visually equivalent layouts.

use crate::Vec2;
use rayon::prelude::*;

/// Opening angle. A cell of width `s` seen from distance `d` is treated as a
/// single body when `s / d < THETA`. Smaller = more accurate + slower; `0.9` is
/// a good speed/quality trade-off for graph layout (which needs no physical
/// accuracy).
const THETA: f32 = 0.9;

/// Squared-distance floor, matching the exact path's coincidence guard, so two
/// bodies at (nearly) the same spot don't produce an infinite force / NaN.
const EPS2: f32 = 1e-4;

/// Maximum subdivision depth. Bounds recursion and, more importantly, terminates
/// insertion for clusters of coincident points (which subdivision can never
/// separate); such a cluster collapses into one weighted leaf.
const MAX_DEPTH: u32 = 48;

/// A quadtree cell in the arena, one of:
/// - **empty**: `mass == 0`,
/// - **leaf**: no children (`leaf_body >= 0` for a single body at `leaf_pos`, or
///   `leaf_body == -1` for a collapsed coincident cluster summarised by
///   `mass`/`com`),
/// - **internal**: at least one child `>= 0`.
#[derive(Clone, Copy)]
struct Cell {
    cx: f32,
    cy: f32,
    half: f32,
    /// Number of bodies under this cell (unit mass each, matching the exact
    /// pairwise force).
    mass: f32,
    /// Centre of mass of everything under the cell.
    com: Vec2,
    children: [i32; 4],
    /// The single body a leaf holds (so it can be pushed down on a split), or
    /// `-1` when the cell is empty, internal, or a collapsed cluster.
    leaf_body: i32,
    /// Original position of `leaf_body` (only meaningful when `leaf_body >= 0`).
    leaf_pos: Vec2,
}

impl Cell {
    fn new(cx: f32, cy: f32, half: f32) -> Self {
        Cell {
            cx,
            cy,
            half,
            mass: 0.0,
            com: [0.0, 0.0],
            children: [-1; 4],
            leaf_body: -1,
            leaf_pos: [0.0, 0.0],
        }
    }

    /// A cell is internal once it has at least one real child.
    #[inline]
    fn is_internal(&self) -> bool {
        self.children.iter().any(|&c| c >= 0)
    }
}

/// A built quadtree ready for force queries.
pub(crate) struct QuadTree {
    cells: Vec<Cell>,
}

impl QuadTree {
    /// Build a tree over `pos`. Returns `None` for fewer than two bodies (there
    /// is nothing to repel).
    fn build(pos: &[Vec2]) -> Option<QuadTree> {
        if pos.len() < 2 {
            return None;
        }
        let (mut minx, mut miny) = (f32::MAX, f32::MAX);
        let (mut maxx, mut maxy) = (f32::MIN, f32::MIN);
        for p in pos {
            minx = minx.min(p[0]);
            miny = miny.min(p[1]);
            maxx = maxx.max(p[0]);
            maxy = maxy.max(p[1]);
        }
        let cx = (minx + maxx) * 0.5;
        let cy = (miny + maxy) * 0.5;
        // Square that covers every body, padded so points on the edge sort cleanly.
        let half = ((maxx - minx).max(maxy - miny) * 0.5).max(1.0) + 1.0;

        let mut tree = QuadTree {
            cells: Vec::with_capacity(pos.len() * 2),
        };
        tree.cells.push(Cell::new(cx, cy, half));
        for (i, p) in pos.iter().enumerate() {
            tree.insert(0, i as i32, *p, 0);
        }
        Some(tree)
    }

    /// Insert body `body` at position `p` under cell `ci`.
    fn insert(&mut self, ci: usize, body: i32, p: Vec2, depth: u32) {
        // Fold this body into the cell's centre of mass, then read the fields we
        // need and release the borrow before any recursive `descend`.
        let (cx, cy, mass_before, is_internal, leaf_body, leaf_pos);
        {
            let cell = &mut self.cells[ci];
            mass_before = cell.mass;
            let new_mass = mass_before + 1.0;
            cell.com[0] = (cell.com[0] * mass_before + p[0]) / new_mass;
            cell.com[1] = (cell.com[1] * mass_before + p[1]) / new_mass;
            cell.mass = new_mass;
            cx = cell.cx;
            cy = cell.cy;
            is_internal = cell.is_internal();
            leaf_body = cell.leaf_body;
            leaf_pos = cell.leaf_pos;
        }

        // Empty -> becomes a leaf holding just this body.
        if mass_before == 0.0 {
            let cell = &mut self.cells[ci];
            cell.leaf_body = body;
            cell.leaf_pos = p;
            return;
        }

        // Internal -> descend into the matching quadrant.
        if is_internal {
            let q = quadrant(cx, cy, p);
            self.descend(ci, q, body, p, depth);
            return;
        }

        // Occupied leaf. A collapsed cluster (`leaf_body < 0`) keeps collapsing;
        // its extra mass/com were already folded in above.
        if leaf_body < 0 {
            return;
        }
        let dx = leaf_pos[0] - p[0];
        let dy = leaf_pos[1] - p[1];
        if depth >= MAX_DEPTH || dx * dx + dy * dy < EPS2 {
            // Cannot separate the two: collapse into a weighted leaf.
            self.cells[ci].leaf_body = -1;
            return;
        }
        // Split: push the existing body down, then the new one.
        self.cells[ci].leaf_body = -1;
        self.descend(ci, quadrant(cx, cy, leaf_pos), leaf_body, leaf_pos, depth);
        self.descend(ci, quadrant(cx, cy, p), body, p, depth);
    }

    /// Insert `body` into child quadrant `q` of internal cell `ci`, creating the
    /// child cell if it does not exist yet.
    fn descend(&mut self, ci: usize, q: usize, body: i32, p: Vec2, depth: u32) {
        let child = self.cells[ci].children[q];
        let child_idx = if child >= 0 {
            child as usize
        } else {
            let (cx, cy, half) = child_bounds(&self.cells[ci], q);
            let idx = self.cells.len();
            self.cells.push(Cell::new(cx, cy, half));
            self.cells[ci].children[q] = idx as i32;
            idx
        };
        self.insert(child_idx, body, p, depth + 1);
    }

    /// Repulsion force on a body at `p`, summed over the tree.
    #[inline]
    fn force(&self, p: Vec2, k: f32, repulsion: f32) -> Vec2 {
        let mut acc = [0.0f32, 0.0f32];
        self.force_rec(0, p, k, repulsion, &mut acc);
        acc
    }

    fn force_rec(&self, ci: usize, p: Vec2, k: f32, repulsion: f32, acc: &mut Vec2) {
        let cell = &self.cells[ci];
        if cell.mass == 0.0 {
            return;
        }
        let ex = p[0] - cell.com[0];
        let ey = p[1] - cell.com[1];
        let d2 = ex * ex + ey * ey;

        if cell.is_internal() {
            // Multipole acceptance: `s/d < THETA` (squared to avoid the sqrt).
            let s = cell.half * 2.0;
            if s * s < THETA * THETA * d2 {
                apply(acc, ex, ey, d2, cell.mass, k, repulsion);
            } else {
                for &c in &cell.children {
                    if c >= 0 {
                        self.force_rec(c as usize, p, k, repulsion, acc);
                    }
                }
            }
            return;
        }

        // Leaf: skip self / coincident (below the softening floor), else apply.
        if d2 >= EPS2 {
            apply(acc, ex, ey, d2, cell.mass, k, repulsion);
        }
    }
}

/// Fill `disp` with the Barnes-Hut repulsion force for each body in `pos`.
/// Parallel over bodies; the tree is read-only, so this is race-free.
pub(crate) fn repulsion(pos: &[Vec2], disp: &mut [Vec2], k: f32, repulsion: f32) {
    let Some(tree) = QuadTree::build(pos) else {
        disp.iter_mut().for_each(|d| *d = [0.0, 0.0]);
        return;
    };
    disp.par_iter_mut()
        .zip(pos.par_iter())
        .for_each(|(d, &p)| *d = tree.force(p, k, repulsion));
}

/// Accumulate the repulsion from a pseudo-body of `mass` at offset `(ex, ey)`
/// with squared distance `d2`. Identical force law to the exact path.
#[inline]
fn apply(acc: &mut Vec2, ex: f32, ey: f32, d2: f32, mass: f32, k: f32, repulsion: f32) {
    let dist = d2.max(1e-3).sqrt();
    let force = repulsion * (k * k) / dist * mass;
    acc[0] += ex / dist * force;
    acc[1] += ey / dist * force;
}

/// Which child quadrant of the square centred at `(cx, cy)` contains `p`
/// (0=NW, 1=NE, 2=SW, 3=SE).
#[inline]
fn quadrant(cx: f32, cy: f32, p: Vec2) -> usize {
    let east = (p[0] >= cx) as usize;
    let south = (p[1] >= cy) as usize;
    south * 2 + east
}

/// Centre + half-extent of child quadrant `q` of `cell`.
#[inline]
fn child_bounds(cell: &Cell, q: usize) -> (f32, f32, f32) {
    let h = cell.half * 0.5;
    let east = q & 1 == 1;
    let south = q & 2 == 2;
    let cx = if east { cell.cx + h } else { cell.cx - h };
    let cy = if south { cell.cy + h } else { cell.cy - h };
    (cx, cy, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force exact repulsion, mirroring `crate::layout`'s inner loop but
    /// without the index-based coincidence nudge (BH can't reproduce that), so
    /// the two are comparable on well-separated points.
    fn exact(pos: &[Vec2], k: f32, repulsion: f32) -> Vec<Vec2> {
        let n = pos.len();
        (0..n)
            .map(|i| {
                let pi = pos[i];
                let mut acc = [0.0f32, 0.0f32];
                for (j, pj) in pos.iter().enumerate() {
                    if i == j {
                        continue;
                    }
                    let ex = pi[0] - pj[0];
                    let ey = pi[1] - pj[1];
                    let d2 = ex * ex + ey * ey;
                    if d2 >= EPS2 {
                        apply(&mut acc, ex, ey, d2, 1.0, k, repulsion);
                    }
                }
                acc
            })
            .collect()
    }

    fn grid(cols: usize, rows: usize, spacing: f32) -> Vec<Vec2> {
        let mut v = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                v.push([c as f32 * spacing, r as f32 * spacing]);
            }
        }
        v
    }

    #[test]
    fn is_deterministic() {
        let pos = grid(20, 20, 5.0);
        let mut a = vec![[0.0; 2]; pos.len()];
        let mut b = vec![[0.0; 2]; pos.len()];
        repulsion(&pos, &mut a, 40.0, 1.0);
        repulsion(&pos, &mut b, 40.0, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn all_forces_finite() {
        let pos = grid(32, 32, 3.0);
        let mut disp = vec![[0.0; 2]; pos.len()];
        repulsion(&pos, &mut disp, 40.0, 1.0);
        assert!(disp.iter().all(|d| d[0].is_finite() && d[1].is_finite()));
    }

    #[test]
    fn approximates_exact_within_tolerance() {
        // On well-separated points the multipole sum should track the exact
        // all-pairs force closely (worst relative per-node error under ~25%).
        let pos = grid(16, 16, 7.0);
        let (k, rep) = (40.0, 1.0);
        let approx = {
            let mut d = vec![[0.0; 2]; pos.len()];
            repulsion(&pos, &mut d, k, rep);
            d
        };
        let truth = exact(&pos, k, rep);

        let mut worst = 0.0f32;
        for (a, t) in approx.iter().zip(&truth) {
            let tmag = (t[0] * t[0] + t[1] * t[1]).sqrt();
            if tmag < 1.0 {
                continue; // near-zero net force (interior symmetry): skip
            }
            let ex = a[0] - t[0];
            let ey = a[1] - t[1];
            let err = (ex * ex + ey * ey).sqrt() / tmag;
            worst = worst.max(err);
        }
        assert!(worst < 0.25, "worst relative error {worst} too high");
    }

    #[test]
    fn coincident_points_do_not_explode() {
        // A pile of identical points must terminate (depth cap) and stay finite.
        let pos = vec![[3.0, 3.0]; 500];
        let mut disp = vec![[0.0; 2]; pos.len()];
        repulsion(&pos, &mut disp, 40.0, 1.0);
        assert!(disp.iter().all(|d| d[0].is_finite() && d[1].is_finite()));
    }

    #[test]
    fn single_and_empty_are_zero() {
        let mut one = vec![[0.0; 2]; 1];
        repulsion(&[[1.0, 2.0]], &mut one, 40.0, 1.0);
        assert_eq!(one, vec![[0.0, 0.0]]);

        let mut none: Vec<Vec2> = Vec::new();
        repulsion(&[], &mut none, 40.0, 1.0);
        assert!(none.is_empty());
    }
}
