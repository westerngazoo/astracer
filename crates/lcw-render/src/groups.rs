//! Drawing **group boxes** ([`GroupBox`]): the crate rectangles of a grouped
//! layout. Each box gets a translucent fill, a tinted label band across its
//! top, a border and the crate's name, all appended to a [`SceneData`] so the
//! native viewer and the wasm webview draw them identically.
//!
//! The geometry comes from the layout; nothing here decides where a box goes.
//! [`group_labels`] places the names for a screen-space overlay instead (the
//! web UI draws them as HTML text, crisp at any zoom).

use glam::Vec2;
use lcw_core::{GroupBox, GROUP_HEADER};

use crate::camera::Camera2D;
use crate::module_view::{crate_color, push_rect_fill, push_rect_outline};
use crate::scene::SceneData;
use crate::text;

/// Largest glyph pixel for a name: 8 cells tall, so 40 of the band's 72 units,
/// leaving even space above and below.
const NAME_PX: f32 = 5.0;
/// Smallest glyph pixel a name shrinks to before it is clipped instead.
const NAME_MIN_PX: f32 = 2.0;
/// Space between the border and the name.
const NAME_INSET: f32 = 24.0;
/// Color for the box of external code: neutral, so it reads as "not ours".
const EXTERNAL_RGB: [f32; 3] = [0.55, 0.57, 0.62];
/// Opacity kept by a call into the external box. Every crate reaches for std,
/// so these calls run the full height of the diagram and, drawn like the rest,
/// bury the crates under one gray funnel. A selected node's calls are
/// re-brightened by [`crate::highlight`], so nothing is lost by fading them.
const EXTERNAL_EDGE_ALPHA: f32 = 0.2;

/// The color a box is drawn in: its crate's hue, or neutral gray for external
/// code. Overlays use it to tie a name to its box.
pub fn group_color(b: &GroupBox) -> [f32; 3] {
    if b.external {
        EXTERNAL_RGB
    } else {
        crate_color(&b.name)
    }
}

/// Append `boxes` to `scene`, with world-space names when `labels` is set,
/// fade the calls that end in an external box, and grow the scene bounds to
/// include the boxes (so "fit" frames the boxes, not just the nodes inside).
pub fn draw_groups(scene: &mut SceneData, boxes: &[GroupBox], labels: bool) {
    for b in boxes {
        let rgb = group_color(b);
        let (fill, band, border) = if b.external {
            (0.04, 0.10, 0.45)
        } else {
            (0.07, 0.16, 0.80)
        };
        let tint = |a: f32| [rgb[0], rgb[1], rgb[2], a];
        push_rect_fill(&mut scene.group_fills, b.min, b.max, tint(fill));
        let band_bottom = (b.max[1] - GROUP_HEADER).max(b.min[1]);
        push_rect_fill(
            &mut scene.group_fills,
            [b.min[0], band_bottom],
            b.max,
            tint(band),
        );
        push_rect_outline(&mut scene.group_outlines, b.min, b.max, tint(border));
        if labels {
            draw_name(scene, b);
        }
    }
    fade_external_calls(scene, boxes);
    grow_bounds(scene, boxes);
}

/// Scale down the opacity of every edge segment whose target node sits in an
/// external box.
fn fade_external_calls(scene: &mut SceneData, boxes: &[GroupBox]) {
    let external: Vec<&GroupBox> = boxes.iter().filter(|b| b.external).collect();
    if external.is_empty() {
        return;
    }
    for (i, &[_, to]) in scene.edge_nodes.iter().enumerate() {
        let Some(target) = scene.nodes.get(to as usize) else {
            continue;
        };
        if !external.iter().any(|b| b.contains(target.center)) {
            continue;
        }
        for v in scene.edges.iter_mut().skip(i * 2).take(2) {
            v.color[3] *= EXTERNAL_EDGE_ALPHA;
        }
    }
}

/// Where one box's name goes in a screen-space overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupLabel {
    /// Index into the boxes passed to [`group_labels`].
    pub index: usize,
    /// Top-left corner for the name, in screen pixels: the box's top-left,
    /// pinned inside the viewport while the box is on screen, so a large box
    /// keeps its name in view as you pan across it.
    pub screen: [f32; 2],
    /// Width the name may take before it must be clipped: what is visible of
    /// the box from `screen` to its right edge.
    pub width_px: f32,
}

/// The names worth showing: boxes at least `min_width_px` wide on screen and
/// at least partly in view.
pub fn group_labels(boxes: &[GroupBox], camera: &Camera2D, min_width_px: f32) -> Vec<GroupLabel> {
    let vp = camera.viewport;
    boxes
        .iter()
        .enumerate()
        .filter_map(|(index, b)| {
            let tl = camera.world_to_screen(Vec2::new(b.min[0], b.max[1]));
            let br = camera.world_to_screen(Vec2::new(b.max[0], b.min[1]));
            let width = br.x - tl.x;
            let off = br.x < 0.0 || br.y < 0.0 || tl.x > vp.x || tl.y > vp.y;
            if off || width < min_width_px {
                return None;
            }
            let x = tl.x.max(0.0);
            // Stay inside the box: never below its bottom edge.
            let y = tl.y.max(0.0).min(br.y - 16.0).max(tl.y);
            Some(GroupLabel {
                index,
                screen: [x, y],
                width_px: br.x.min(vp.x) - x,
            })
        })
        .collect()
}

/// The box's name and size, as large as fits the band, vertically centered.
fn draw_name(scene: &mut SceneData, b: &GroupBox) {
    let avail = (b.max[0] - b.min[0]) - 2.0 * NAME_INSET;
    if avail <= 0.0 {
        return;
    }
    let full = if b.external {
        format!("{}  {}", b.name, b.members)
    } else {
        format!("{}  {} fn", b.name, b.members)
    };
    let name = fit(&full, avail);
    let px = (avail / text::width(&name, 1.0).max(1.0)).clamp(NAME_MIN_PX, NAME_PX);
    let top = b.max[1] - (GROUP_HEADER - 8.0 * px) * 0.5;
    text::emit(
        &mut scene.labels,
        &name,
        [b.min[0] + NAME_INSET, top],
        px,
        [0.97, 0.98, 1.0, 1.0],
    );
}

/// `s`, clipped with a trailing `~` if even the smallest size overflows `avail`.
fn fit(s: &str, avail: f32) -> String {
    if text::width(s, NAME_MIN_PX) <= avail {
        return s.to_string();
    }
    let mut out: String = s.chars().collect();
    while !out.is_empty() && text::width(&format!("{out}~"), NAME_MIN_PX) > avail {
        out.pop();
    }
    out.push('~');
    out
}

fn grow_bounds(scene: &mut SceneData, boxes: &[GroupBox]) {
    if boxes.is_empty() {
        return;
    }
    let empty = scene.nodes.is_empty() && scene.min == scene.max;
    let (mut min, mut max) = if empty {
        ([f32::MAX; 2], [f32::MIN; 2])
    } else {
        (scene.min, scene.max)
    };
    for b in boxes {
        min = [min[0].min(b.min[0]), min[1].min(b.min[1])];
        max = [max[0].max(b.max[0]), max[1].max(b.max[1])];
    }
    scene.min = min;
    scene.max = max;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(name: &str, min: [f32; 2], max: [f32; 2], external: bool) -> GroupBox {
        GroupBox {
            name: name.into(),
            min,
            max,
            members: 12,
            external,
        }
    }

    #[test]
    fn each_box_is_a_fill_a_band_and_a_border() {
        let mut scene = SceneData::default();
        let boxes = [
            boxed("engine", [0.0, 0.0], [600.0, 400.0], false),
            boxed("external", [0.0, -700.0], [900.0, -200.0], true),
        ];
        draw_groups(&mut scene, &boxes, true);
        assert_eq!(scene.group_fills.len(), 2 * 12);
        assert_eq!(scene.group_outlines.len(), 2 * 8);
        assert!(!scene.labels.is_empty());
        // The band covers exactly the header strip of the first box.
        let band: Vec<f32> = scene.group_fills[6..12].iter().map(|v| v.pos[1]).collect();
        let lowest = band.iter().cloned().fold(f32::MAX, f32::min);
        assert_eq!(lowest, 400.0 - GROUP_HEADER);
    }

    #[test]
    fn names_stay_inside_their_band_even_when_clipped() {
        let long = "a_crate_name_far_too_long_for_a_tiny_box_to_hold";
        for w in [240.0f32, 1200.0] {
            let mut scene = SceneData::default();
            let b = boxed(long, [0.0, 0.0], [w, 300.0], false);
            draw_groups(&mut scene, std::slice::from_ref(&b), true);
            assert!(!scene.labels.is_empty());
            for v in &scene.labels {
                assert!(
                    v.pos[0] >= b.min[0] && v.pos[0] <= b.max[0],
                    "x {} in {w}",
                    v.pos[0]
                );
                assert!(v.pos[1] <= b.max[1] && v.pos[1] >= b.max[1] - GROUP_HEADER);
            }
        }
        assert!(fit(long, 200.0).ends_with('~'));
        assert_eq!(fit("core", 200.0), "core");
    }

    #[test]
    fn labels_off_draws_no_text() {
        let mut scene = SceneData::default();
        draw_groups(
            &mut scene,
            &[boxed("core", [0.0, 0.0], [300.0, 300.0], false)],
            false,
        );
        assert!(scene.labels.is_empty());
        assert_eq!(scene.group_fills.len(), 12);
    }

    #[test]
    fn calls_into_the_external_box_are_faded_and_others_are_not() {
        use crate::scene::{EdgeVertex, NodeInstance};
        let node = |x: f32, y: f32| NodeInstance {
            center: [x, y],
            radius: 4.0,
            color: [1.0; 4],
            pick_id: 1,
        };
        let v = |x: f32, y: f32| EdgeVertex {
            pos: [x, y],
            color: [0.5, 0.5, 0.5, 0.6],
        };
        // 0: in `core`, 1: in `core`, 2: external.
        let mut scene = SceneData {
            nodes: vec![node(10.0, 10.0), node(50.0, 10.0), node(10.0, -300.0)],
            edges: vec![v(10.0, 10.0), v(50.0, 10.0), v(10.0, 10.0), v(10.0, -300.0)],
            edge_nodes: vec![[0, 1], [0, 2]],
            ..Default::default()
        };
        let boxes = [
            boxed("core", [0.0, 0.0], [300.0, 200.0], false),
            boxed("external", [0.0, -400.0], [300.0, -100.0], true),
        ];
        draw_groups(&mut scene, &boxes, false);
        assert_eq!(scene.edges[0].color[3], 0.6);
        assert_eq!(scene.edges[1].color[3], 0.6);
        assert!((scene.edges[2].color[3] - 0.6 * EXTERNAL_EDGE_ALPHA).abs() < 1e-6);
        assert!((scene.edges[3].color[3] - 0.6 * EXTERNAL_EDGE_ALPHA).abs() < 1e-6);
    }

    #[test]
    fn names_are_placed_for_boxes_in_view_and_pinned_while_panning() {
        let camera = Camera2D {
            center: Vec2::new(500.0, 0.0),
            zoom: 1.0,
            viewport: Vec2::new(1000.0, 600.0),
        };
        let boxes = [
            // Fully in view: the name sits at its top-left.
            boxed("core", [100.0, -100.0], [400.0, 200.0], false),
            // Starts left of the viewport and runs past its top: pinned.
            boxed("big", [-300.0, -250.0], [600.0, 900.0], false),
            // Too narrow on screen to label.
            boxed("tiny", [700.0, 0.0], [720.0, 50.0], false),
            // Entirely off to the right.
            boxed("away", [2000.0, 0.0], [2400.0, 100.0], false),
        ];
        let labels = group_labels(&boxes, &camera, 40.0);
        let got: Vec<usize> = labels.iter().map(|l| l.index).collect();
        assert_eq!(got, vec![0, 1]);
        let near =
            |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3;
        // World (100, 200) -> screen (100, 100); visible width 300.
        assert!(near(labels[0].screen, [100.0, 100.0]), "{:?}", labels[0]);
        assert!((labels[0].width_px - 300.0).abs() < 1e-3);
        // Pinned to the viewport's top-left corner, as wide as what shows.
        assert!(near(labels[1].screen, [0.0, 0.0]), "{:?}", labels[1]);
        assert!((labels[1].width_px - 600.0).abs() < 1e-3);
    }

    #[test]
    fn external_code_is_drawn_neutral() {
        let ext = boxed("external", [0.0, 0.0], [1.0, 1.0], true);
        let own = boxed("external", [0.0, 0.0], [1.0, 1.0], false);
        assert_eq!(group_color(&ext), EXTERNAL_RGB);
        assert_ne!(group_color(&own), EXTERNAL_RGB);
    }

    #[test]
    fn bounds_grow_to_frame_the_boxes() {
        let mut scene = SceneData {
            min: [10.0, 10.0],
            max: [20.0, 20.0],
            ..Default::default()
        };
        scene.nodes.push(crate::scene::NodeInstance {
            center: [15.0, 15.0],
            radius: 4.0,
            color: [1.0; 4],
            pick_id: 1,
        });
        draw_groups(
            &mut scene,
            &[boxed("core", [-50.0, 0.0], [300.0, 90.0], false)],
            false,
        );
        assert_eq!(scene.min, [-50.0, 0.0]);
        assert_eq!(scene.max, [300.0, 90.0]);

        // An empty scene takes the boxes' bounds rather than keeping (0,0).
        let mut empty = SceneData::default();
        draw_groups(
            &mut empty,
            &[boxed("core", [5.0, 5.0], [300.0, 90.0], false)],
            false,
        );
        assert_eq!(empty.min, [5.0, 5.0]);
    }
}
