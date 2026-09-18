//! Label selection for the DOM overlay (Feature 1).
//!
//! Drawing a `<span>` for every node would be illegible and slow, so this
//! module picks a small, important subset and projects each to screen space via
//! [`Camera2D`]. The frontend maps the returned indices to node names and
//! positions the spans; keeping the *policy* here (pure, no DOM) means it is
//! covered by `cargo test -p lcw-render`.

use glam::Vec2;

use crate::camera::Camera2D;
use crate::scene::NodeInstance;

/// Tunables for [`select_labels`].
#[derive(Debug, Clone, Copy)]
pub struct LabelOptions {
    /// Hard cap on how many labels to show (legibility + DOM cost).
    pub max_labels: usize,
    /// Only label nodes at least this big *on screen* (px). Because node radius
    /// grows with fan-in, this doubles as an importance gate and a natural
    /// "labels appear as you zoom in" behavior.
    pub min_radius_px: f32,
    /// Cull nodes whose center is more than this many px outside the viewport.
    pub margin_px: f32,
}

impl Default for LabelOptions {
    fn default() -> Self {
        LabelOptions {
            max_labels: 48,
            min_radius_px: 7.0,
            margin_px: 96.0,
        }
    }
}

/// A label to draw: which node, where (screen px, at the node center), and how
/// big the node is on screen (callers can scale font size with it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelPlacement {
    pub index: usize,
    pub screen: [f32; 2],
    pub radius_px: f32,
}

/// Choose which nodes deserve a label for the current camera.
///
/// Candidates are nodes that (a) render at least `min_radius_px` across and
/// (b) fall inside the viewport (plus `margin_px`). They are ranked by on-screen
/// radius (≈ fan-in, the most "important" hubs first) and truncated to
/// `max_labels`. Ties break by index so the output is deterministic (and
/// testable).
pub fn select_labels(
    nodes: &[NodeInstance],
    camera: &Camera2D,
    opts: &LabelOptions,
) -> Vec<LabelPlacement> {
    let vp = camera.viewport;
    let mut candidates: Vec<LabelPlacement> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        let radius_px = node.radius * camera.zoom;
        if radius_px < opts.min_radius_px {
            continue;
        }
        let s = camera.world_to_screen(Vec2::from(node.center));
        let out = s.x < -opts.margin_px
            || s.y < -opts.margin_px
            || s.x > vp.x + opts.margin_px
            || s.y > vp.y + opts.margin_px;
        if out {
            continue;
        }
        candidates.push(LabelPlacement {
            index: i,
            screen: [s.x, s.y],
            radius_px,
        });
    }

    candidates.sort_by(|a, b| {
        b.radius_px
            .partial_cmp(&a.radius_px)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.index.cmp(&b.index))
    });
    candidates.truncate(opts.max_labels);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(center: [f32; 2], radius: f32, pick: u32) -> NodeInstance {
        NodeInstance {
            center,
            radius,
            color: [1.0, 1.0, 1.0, 1.0],
            pick_id: pick,
        }
    }

    fn cam() -> Camera2D {
        Camera2D {
            center: Vec2::ZERO,
            zoom: 1.0,
            viewport: Vec2::new(200.0, 200.0),
        }
    }

    #[test]
    fn filters_small_and_offscreen_then_ranks_by_size() {
        let nodes = vec![
            node([0.0, 0.0], 10.0, 1),      // big, centered -> kept, rank 1
            node([50.0, 0.0], 8.0, 2),      // on-screen -> kept, rank 2
            node([0.0, 50.0], 5.0, 3),      // too small -> dropped
            node([10_000.0, 0.0], 20.0, 4), // off-screen -> culled
        ];
        let opts = LabelOptions {
            max_labels: 10,
            min_radius_px: 7.0,
            margin_px: 96.0,
        };
        let out = select_labels(&nodes, &cam(), &opts);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].index, 0);
        assert_eq!(out[1].index, 1);
        // Center node projects to viewport center.
        assert!((out[0].screen[0] - 100.0).abs() < 1e-3);
        assert!((out[0].screen[1] - 100.0).abs() < 1e-3);
    }

    #[test]
    fn respects_max_labels_cap() {
        let nodes = vec![
            node([0.0, 0.0], 10.0, 1),
            node([20.0, 0.0], 9.0, 2),
            node([-20.0, 0.0], 8.0, 3),
        ];
        let opts = LabelOptions {
            max_labels: 1,
            ..Default::default()
        };
        let out = select_labels(&nodes, &cam(), &opts);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].index, 0); // the largest
    }

    #[test]
    fn zooming_in_reveals_more_labels() {
        // A node of world-radius 4 is below the 7px gate at zoom 1...
        let nodes = vec![node([0.0, 0.0], 4.0, 1)];
        let opts = LabelOptions::default();
        assert!(select_labels(&nodes, &cam(), &opts).is_empty());
        // ...but crosses it once we zoom in.
        let zoomed = Camera2D { zoom: 3.0, ..cam() };
        assert_eq!(select_labels(&nodes, &zoomed, &opts).len(), 1);
    }
}
