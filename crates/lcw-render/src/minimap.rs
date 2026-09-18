//! Minimap geometry (Feature 2).
//!
//! A small corner overview of the whole graph with a rectangle marking the
//! current viewport. The frontend draws it on a 2D `<canvas>`; this module owns
//! the (pure, testable) coordinate math: world → minimap px, the inverse
//! (for click-to-recenter), and the viewport rectangle to outline.
//!
//! Minimap pixels use the DOM convention (origin top-left, +Y down) and the
//! world Y axis is flipped so the overview matches the main view's orientation.

use crate::camera::Camera2D;

/// An axis-aligned rectangle in minimap pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A fitted mapping between world space and a `size`-pixel minimap, with
/// `padding` px of inset, preserving aspect ratio and centering the content.
#[derive(Debug, Clone, Copy)]
pub struct MinimapView {
    world_min: [f32; 2],
    world_max: [f32; 2],
    scale: f32,
    offset: [f32; 2],
    size: [f32; 2],
}

impl MinimapView {
    /// Fit world `bounds` into a `size = [w, h]` px minimap with `padding` inset.
    pub fn new(bounds_min: [f32; 2], bounds_max: [f32; 2], size: [f32; 2], padding: f32) -> Self {
        let world_w = (bounds_max[0] - bounds_min[0]).max(1e-3);
        let world_h = (bounds_max[1] - bounds_min[1]).max(1e-3);
        let avail_w = (size[0] - 2.0 * padding).max(1.0);
        let avail_h = (size[1] - 2.0 * padding).max(1.0);
        let scale = (avail_w / world_w).min(avail_h / world_h);
        let content_w = world_w * scale;
        let content_h = world_h * scale;
        let offset = [(size[0] - content_w) * 0.5, (size[1] - content_h) * 0.5];
        MinimapView {
            world_min: bounds_min,
            world_max: bounds_max,
            scale,
            offset,
            size,
        }
    }

    pub fn size(&self) -> [f32; 2] {
        self.size
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Project a world point to minimap pixels (Y flipped so up stays up).
    pub fn world_to_minimap(&self, world: [f32; 2]) -> [f32; 2] {
        [
            self.offset[0] + (world[0] - self.world_min[0]) * self.scale,
            self.offset[1] + (self.world_max[1] - world[1]) * self.scale,
        ]
    }

    /// Inverse of [`world_to_minimap`]: a minimap click → the world point to
    /// recenter the camera on.
    ///
    /// [`world_to_minimap`]: MinimapView::world_to_minimap
    pub fn minimap_to_world(&self, px: [f32; 2]) -> [f32; 2] {
        [
            self.world_min[0] + (px[0] - self.offset[0]) / self.scale,
            self.world_max[1] - (px[1] - self.offset[1]) / self.scale,
        ]
    }

    /// The rectangle (in minimap px) covering what `camera` currently shows.
    pub fn viewport_rect(&self, camera: &Camera2D) -> Rect {
        let hx = (camera.viewport.x * 0.5) / camera.zoom.max(1e-4);
        let hy = (camera.viewport.y * 0.5) / camera.zoom.max(1e-4);
        let cx = camera.center.x;
        let cy = camera.center.y;
        // Top-left in world has max Y; bottom-right has min Y (Y is flipped).
        let tl = self.world_to_minimap([cx - hx, cy + hy]);
        let br = self.world_to_minimap([cx + hx, cy - hy]);
        Rect {
            x: tl[0],
            y: tl[1],
            w: br[0] - tl[0],
            h: br[1] - tl[1],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    fn view() -> MinimapView {
        MinimapView::new([0.0, 0.0], [100.0, 50.0], [200.0, 100.0], 10.0)
    }

    #[test]
    fn corners_map_into_the_padded_box_with_y_flip() {
        let v = view();
        // scale = min(180/100, 80/50) = 1.6; offset = [20, 10].
        let bl = v.world_to_minimap([0.0, 0.0]); // world bottom-left -> minimap bottom-left
        let tl = v.world_to_minimap([0.0, 50.0]); // world top-left -> minimap top-left
        let tr = v.world_to_minimap([100.0, 50.0]);
        assert!((bl[0] - 20.0).abs() < 1e-3 && (bl[1] - 90.0).abs() < 1e-3);
        assert!((tl[0] - 20.0).abs() < 1e-3 && (tl[1] - 10.0).abs() < 1e-3);
        assert!((tr[0] - 180.0).abs() < 1e-3 && (tr[1] - 10.0).abs() < 1e-3);
    }

    #[test]
    fn world_minimap_round_trips() {
        let v = view();
        for &p in &[[0.0, 0.0], [100.0, 50.0], [37.0, 12.0], [80.0, 45.0]] {
            let back = v.minimap_to_world(v.world_to_minimap(p));
            assert!((back[0] - p[0]).abs() < 1e-2 && (back[1] - p[1]).abs() < 1e-2);
        }
    }

    #[test]
    fn viewport_rect_covers_content_when_camera_frames_bounds() {
        let v = view();
        // Camera framing exactly [0,0]..[100,50]: center (50,25), half (50,25).
        let cam = Camera2D {
            center: Vec2::new(50.0, 25.0),
            zoom: 1.0,
            viewport: Vec2::new(100.0, 50.0),
        };
        let r = v.viewport_rect(&cam);
        assert!((r.x - 20.0).abs() < 1e-3, "x={}", r.x);
        assert!((r.y - 10.0).abs() < 1e-3, "y={}", r.y);
        assert!((r.w - 160.0).abs() < 1e-3, "w={}", r.w);
        assert!((r.h - 80.0).abs() < 1e-3, "h={}", r.h);
    }

    #[test]
    fn click_center_recenters_to_bounds_center() {
        let v = view();
        // Minimap center pixel -> world center of bounds.
        let w = v.minimap_to_world([100.0, 50.0]);
        assert!((w[0] - 50.0).abs() < 1e-2 && (w[1] - 25.0).abs() < 1e-2);
    }
}
