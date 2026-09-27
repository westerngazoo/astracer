//! Pointer interaction shared by the native viewer and the web UI: telling a
//! click from a drag, finding the node a click meant, and the selection and
//! flow-source state that a click updates.
//!
//! It lives here, GPU-free, for two reasons. Both front ends need the same
//! answers — a click that selects in the browser must select in the window —
//! and these rules are exactly where "flow from here" broke without any test
//! noticing:
//!
//! * a click on a Retina trackpad read as a drag, because the threshold was
//!   half a *physical* pixel and a drag never selects;
//! * a near miss on the target cleared the selection *and the flow source*,
//!   so one slightly-off click threw the whole flow away;
//! * a target *upstream* of the source traced to nothing and said nothing.
//!
//! Pure functions over plain data can be tested; an event loop cannot.

use lcw_core::{CodeGraph, NodeId};
use lcw_query::Connection;

use crate::scene::SceneData;

/// How far, in logical pixels, a press may travel and still count as a click.
/// Desktop toolkits use four or five (Windows' `SM_CXDRAG` is 4); anything
/// tighter turns ordinary trackpad jitter into a drag.
pub const CLICK_SLOP_PX: f32 = 4.0;

/// How far outside a node's disc, in logical pixels, a click still hits it.
/// Measured on screen rather than in world units, so it means the same thing
/// at every zoom: small nodes stay clickable zoomed out, and big ones do not
/// swallow their neighbours zoomed in.
pub const PICK_SLOP_PX: f32 = 6.0;

/// Maximum hops a flow trace searches, in either direction.
pub const FLOW_MAX_DEPTH: u32 = 64;

/// Whether a press at `press` released at `release` was a click. Both points
/// are in the same pixel space; `scale` is how many of those pixels make a
/// logical one — the window scale factor for physical pixels, 1 for CSS pixels.
///
/// Travel is measured from the press, not per movement event: a slow drag made
/// of many one-pixel moves is still a drag.
pub fn is_click(press: [f32; 2], release: [f32; 2], scale: f32) -> bool {
    let dx = release[0] - press[0];
    let dy = release[1] - press[1];
    (dx * dx + dy * dy).sqrt() <= CLICK_SLOP_PX * scale.max(1.0)
}

/// [`PICK_SLOP_PX`] in world units, for a camera drawing `zoom` screen pixels
/// per world unit whose pixels are `scale` times a logical one.
pub fn pick_slop_world(zoom: f32, scale: f32) -> f32 {
    PICK_SLOP_PX * scale.max(1.0) / zoom.max(1e-4)
}

/// The node a click at `world` meant: of the nodes whose disc, grown by
/// `slop_world`, contains the point, the one whose centre is nearest. Nearest
/// centre rather than deepest overlap, so a small node drawn over a large one
/// is still the one you get when you click on it.
pub fn pick_node(scene: &SceneData, world: [f32; 2], slop_world: f32) -> Option<usize> {
    let mut best = None;
    let mut best_dist = f32::INFINITY;
    for (i, node) in scene.nodes.iter().enumerate() {
        let dx = node.center[0] - world[0];
        let dy = node.center[1] - world[1];
        let dist = (dx * dx + dy * dy).sqrt();
        if dist <= node.radius + slop_world && dist < best_dist {
            best_dist = dist;
            best = Some(i);
        }
    }
    best
}

/// What is selected, the flow source if one is set, and how the two connect.
///
/// Scene indices and graph node ids coincide — [`crate::scene::build`] emits
/// one node per graph node, in id order — so the conversion is a cast; a test
/// below holds the scene builder to that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub selected: Option<usize>,
    pub anchor: Option<usize>,
    /// How the anchor and the selection connect. `None` without an anchor, or
    /// while the anchor itself is selected.
    pub connection: Option<Connection>,
}

impl Selection {
    /// Select node `idx`. With a flow source set, also trace how the two
    /// connect, in both directions (see [`lcw_query::connection`]).
    pub fn select(&mut self, graph: Option<&CodeGraph>, idx: usize) {
        self.selected = Some(idx);
        self.connection = match (self.anchor, graph) {
            (Some(a), Some(g)) if a != idx => Some(lcw_query::connection(
                g,
                node_id(a),
                node_id(idx),
                FLOW_MAX_DEPTH,
            )),
            _ => None,
        };
    }

    /// Make the current selection the flow source. `false` when nothing is
    /// selected, so the caller can say so rather than silently doing nothing.
    pub fn anchor_here(&mut self) -> bool {
        match self.selected {
            Some(sel) => {
                self.anchor = Some(sel);
                self.connection = None;
                true
            }
            None => false,
        }
    }

    /// A click that hit no node. With a flow source set that is nearly always
    /// a near miss on the intended target, so nothing changes and the next
    /// click gets another chance — discarding the source here is what made
    /// the feature look broken. Without one it clears the selection. Returns
    /// whether anything changed.
    pub fn miss(&mut self) -> bool {
        if self.anchor.is_some() || self.selected.is_none() {
            return false;
        }
        self.selected = None;
        self.connection = None;
        true
    }

    /// Forget the selection and the flow source (Esc, or a clear button).
    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    /// The traced path as scene indices, in call order; empty when there is
    /// none.
    pub fn path(&self) -> Vec<usize> {
        self.connection
            .as_ref()
            .map(|c| c.path().iter().map(|id| id.0 as usize).collect())
            .unwrap_or_default()
    }

    /// One line describing the flow for a status bar or HUD, or `None` when no
    /// flow source is set. `name` turns a scene index into a display name.
    pub fn flow_status(&self, name: impl Fn(usize) -> String) -> Option<String> {
        let anchor = self.anchor?;
        let chain = |p: &[NodeId]| {
            p.iter()
                .map(|id| name(id.0 as usize))
                .collect::<Vec<_>>()
                .join(" -> ")
        };
        Some(match &self.connection {
            None | Some(Connection::Same) => {
                format!("flow source: {} (now click a target)", name(anchor))
            }
            Some(Connection::Downstream(p)) => format!("flow: {}", chain(p)),
            Some(Connection::Upstream(p)) => {
                format!("flow, reaching the source: {}", chain(p))
            }
            Some(Connection::Unconnected) => format!(
                "no call path between {} and {} in either direction \
                 (dynamic dispatch or an unresolved call?)",
                name(anchor),
                self.selected.map(&name).unwrap_or_default()
            ),
        })
    }
}

fn node_id(idx: usize) -> NodeId {
    NodeId(idx as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene;
    use crate::Camera2D;
    use glam::Vec2;
    use lcw_core::{Edge, EdgeKind, Node, NodeKind, SourceSpan};

    /// `main -> run -> parse`, and `orphan` on its own.
    fn graph() -> (CodeGraph, [usize; 4]) {
        let mut g = CodeGraph::new();
        let file = g.intern_file("src/main.rs");
        let add = |g: &mut CodeGraph, name: &str, line: u32| {
            g.add_node(Node {
                id: NodeId(0),
                name: name.into(),
                qualified_name: format!("app::{name}"),
                module_path: "app".into(),
                kind: NodeKind::Function,
                span: SourceSpan::new(file, line, 0, line + 2, 1),
                flags: Default::default(),
                stats: Default::default(),
            })
        };
        let main = add(&mut g, "main", 1);
        let run = add(&mut g, "run", 10);
        let parse = add(&mut g, "parse", 20);
        let orphan = add(&mut g, "orphan", 30);
        let edge = |g: &mut CodeGraph, a, b, line| {
            g.add_edge(
                a,
                b,
                Edge::new(
                    EdgeKind::DirectCall,
                    SourceSpan::new(file, line, 4, line, 9),
                ),
            );
        };
        edge(&mut g, main, run, 2);
        edge(&mut g, run, parse, 11);
        let ids = [main, run, parse, orphan].map(|id| id.0 as usize);
        (g, ids)
    }

    fn name(g: &CodeGraph) -> impl Fn(usize) -> String + '_ {
        |i| g.node(NodeId(i as u32)).name.clone()
    }

    #[test]
    fn trackpad_jitter_is_still_a_click() {
        // The old rule: any movement over 0.5 physical pixels was a drag. On a
        // 2x display that is a quarter of a logical pixel.
        assert!(is_click([100.0, 100.0], [100.6, 100.0], 2.0));
        assert!(is_click([100.0, 100.0], [102.0, 101.0], 1.0));
        // Three logical pixels at 2x is six physical pixels: still a click.
        assert!(is_click([100.0, 100.0], [106.0, 100.0], 2.0));
        // A real drag is not.
        assert!(!is_click([100.0, 100.0], [100.0, 105.0], 1.0));
        assert!(!is_click([100.0, 100.0], [100.0, 110.0], 2.0));
    }

    #[test]
    fn pick_slop_is_constant_on_screen_at_any_zoom() {
        assert_eq!(pick_slop_world(1.0, 1.0), PICK_SLOP_PX);
        assert_eq!(pick_slop_world(4.0, 1.0), PICK_SLOP_PX / 4.0);
        assert_eq!(pick_slop_world(2.0, 2.0), PICK_SLOP_PX);
    }

    #[test]
    fn a_near_miss_still_picks_and_overlaps_go_to_the_nearest_centre() {
        let (g, [main, run, parse, orphan]) = graph();
        let mut positions = vec![[0.0, 0.0]; g.node_count()];
        positions[main] = [0.0, 0.0];
        positions[run] = [8.0, 0.0];
        positions[parse] = [900.0, 900.0];
        positions[orphan] = [-900.0, 900.0];
        let s = scene::build(&g, &positions);
        let r = s.nodes[main].radius;

        assert_eq!(pick_node(&s, [0.0, 0.0], 0.0), Some(main));
        // Just outside the disc: only the slop catches it.
        assert_eq!(pick_node(&s, [0.0, -(r + 1.0)], 0.0), None);
        assert_eq!(pick_node(&s, [0.0, -(r + 1.0)], 2.0), Some(main));
        // Between two overlapping discs, the nearer centre wins.
        assert_eq!(pick_node(&s, [5.0, 0.0], 2.0), Some(run));
        assert_eq!(pick_node(&s, [3.0, 0.0], 2.0), Some(main));
        assert_eq!(pick_node(&s, [500.0, 500.0], 2.0), None);
    }

    #[test]
    fn scene_index_is_graph_node_id() {
        let (g, _) = graph();
        let s = scene::build(&g, &vec![[0.0, 0.0]; g.node_count()]);
        for (i, id) in g.node_ids().enumerate() {
            assert_eq!(id.0 as usize, i, "graph ids are dense and ordered");
            assert_eq!(s.nodes[i].pick_id, i as u32 + 1, "pick id is index + 1");
        }
    }

    #[test]
    fn anchor_then_downstream_target_traces_in_call_order() {
        let (g, [main, run, parse, _]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), main);
        assert!(sel.anchor_here());
        sel.select(Some(&g), parse);
        assert_eq!(sel.path(), vec![main, run, parse]);
        assert!(matches!(sel.connection, Some(Connection::Downstream(_))));
        assert_eq!(
            sel.flow_status(name(&g)).unwrap(),
            "flow: main -> run -> parse"
        );
    }

    #[test]
    fn anchor_then_upstream_target_still_traces() {
        // The case that used to show nothing at all.
        let (g, [main, run, parse, _]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), parse);
        sel.anchor_here();
        sel.select(Some(&g), main);
        assert_eq!(sel.path(), vec![main, run, parse], "call order");
        assert!(matches!(sel.connection, Some(Connection::Upstream(_))));
        assert!(sel
            .flow_status(name(&g))
            .unwrap()
            .contains("reaching the source"));
    }

    #[test]
    fn no_path_is_said_out_loud() {
        let (g, [main, _, _, orphan]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), main);
        sel.anchor_here();
        sel.select(Some(&g), orphan);
        assert!(sel.path().is_empty());
        assert_eq!(sel.connection, Some(Connection::Unconnected));
        let status = sel.flow_status(name(&g)).unwrap();
        assert!(
            status.contains("no call path between main and orphan"),
            "{status}"
        );
    }

    #[test]
    fn a_miss_with_a_source_set_keeps_everything() {
        let (g, [main, _, parse, _]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), main);
        sel.anchor_here();
        let before = sel.clone();
        assert!(!sel.miss(), "nothing changes");
        assert_eq!(sel, before);
        // ...and the next, accurate click completes the flow.
        sel.select(Some(&g), parse);
        assert_eq!(sel.path().len(), 3);
    }

    #[test]
    fn a_miss_without_a_source_clears_the_selection() {
        let (g, [main, ..]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), main);
        assert!(sel.miss());
        assert_eq!(sel.selected, None);
        assert!(!sel.miss(), "already clear");
    }

    #[test]
    fn anchoring_needs_a_selection_and_clear_forgets_the_source() {
        let (g, [main, ..]) = graph();
        let mut sel = Selection::default();
        assert!(!sel.anchor_here());
        sel.select(Some(&g), main);
        sel.anchor_here();
        assert_eq!(
            sel.flow_status(name(&g)).unwrap(),
            "flow source: main (now click a target)"
        );
        sel.clear();
        assert_eq!(sel, Selection::default());
        assert_eq!(sel.flow_status(name(&g)), None);
    }

    #[test]
    fn selecting_the_source_itself_traces_nothing() {
        let (g, [main, ..]) = graph();
        let mut sel = Selection::default();
        sel.select(Some(&g), main);
        sel.anchor_here();
        sel.select(Some(&g), main);
        assert_eq!(sel.connection, None);
        assert!(sel.path().is_empty());
    }

    #[test]
    fn a_view_without_a_call_graph_selects_but_never_traces() {
        // The module view has no call graph behind it.
        let mut sel = Selection::default();
        sel.select(None, 0);
        sel.anchor_here();
        sel.select(None, 1);
        assert_eq!(sel.selected, Some(1));
        assert_eq!(sel.connection, None);
    }

    /// The whole click path the viewer runs, minus the GPU: select main by
    /// clicking it, press `f`, then click near — not on — the target with a
    /// trackpad's jitter between press and release, on a 2x display.
    #[test]
    fn flow_from_here_end_to_end_on_a_retina_trackpad() {
        let (g, [main, run, parse, orphan]) = graph();
        let mut positions = vec![[0.0, 0.0]; g.node_count()];
        positions[main] = [-100.0, 0.0];
        positions[run] = [0.0, 0.0];
        positions[parse] = [100.0, 0.0];
        positions[orphan] = [0.0, -300.0];
        let s = scene::build(&g, &positions);

        let scale = 2.0;
        let mut cam = Camera2D {
            viewport: Vec2::new(1600.0, 1000.0),
            ..Default::default()
        };
        cam.fit(Vec2::from(s.min), Vec2::from(s.max));

        let click = |sel: &mut Selection, world: [f32; 2], jitter: [f32; 2]| {
            let press = cam.world_to_screen(Vec2::from(world));
            let release = press + Vec2::from(jitter);
            if !is_click(press.into(), release.into(), scale) {
                return false;
            }
            let at = cam.screen_to_world(release).into();
            match pick_node(&s, at, pick_slop_world(cam.zoom, scale)) {
                Some(i) => sel.select(Some(&g), i),
                None => {
                    sel.miss();
                }
            }
            true
        };

        let mut sel = Selection::default();
        assert!(click(&mut sel, positions[main], [0.0, 0.0]));
        assert_eq!(sel.selected, Some(main));
        assert!(sel.anchor_here());

        // Three pixels off the target's edge, with 1.5 physical pixels of
        // travel between press and release: formerly a "drag", and a miss.
        let r = s.nodes[parse].radius;
        let off = r + 3.0 * scale / cam.zoom;
        assert!(click(&mut sel, [100.0, off], [1.5, 0.5]));
        assert_eq!(sel.path(), vec![main, run, parse]);

        // A click on empty canvas keeps the flow.
        assert!(click(&mut sel, [0.0, 400.0], [0.0, 0.0]));
        assert_eq!(sel.path(), vec![main, run, parse]);
    }
}
