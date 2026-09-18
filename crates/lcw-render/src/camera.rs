//! A 2D pan/zoom camera producing a view-projection matrix for the graph.

use glam::{Mat4, Vec2, Vec4};

/// Orthographic 2D camera. `center` is the world point at the middle of the
/// viewport; `zoom` is pixels-per-world-unit.
#[derive(Debug, Clone, Copy)]
pub struct Camera2D {
    pub center: Vec2,
    pub zoom: f32,
    pub viewport: Vec2,
}

impl Default for Camera2D {
    fn default() -> Self {
        Camera2D {
            center: Vec2::ZERO,
            zoom: 1.0,
            viewport: Vec2::new(1280.0, 800.0),
        }
    }
}

impl Camera2D {
    /// The world-space rectangle currently visible: half-extents.
    fn half_extent(&self) -> Vec2 {
        (self.viewport * 0.5) / self.zoom.max(1e-4)
    }

    /// View-projection matrix mapping world coordinates into clip space.
    /// Y points up on screen. Built directly (2D ortho, z passthrough) to stay
    /// independent of glam's clip-space depth convention.
    pub fn view_proj(&self) -> Mat4 {
        let h = self.half_extent();
        let left = self.center.x - h.x;
        let right = self.center.x + h.x;
        let bottom = self.center.y - h.y;
        let top = self.center.y + h.y;
        let sx = 2.0 / (right - left);
        let sy = 2.0 / (top - bottom);
        let tx = -(right + left) / (right - left);
        let ty = -(top + bottom) / (top - bottom);
        // Column-major columns.
        Mat4::from_cols(
            Vec4::new(sx, 0.0, 0.0, 0.0),
            Vec4::new(0.0, sy, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(tx, ty, 0.0, 1.0),
        )
    }

    /// Convert a screen-pixel position (origin top-left, +Y down) to world.
    pub fn screen_to_world(&self, screen: Vec2) -> Vec2 {
        let h = self.half_extent();
        // Normalized [-1, 1], with Y flipped to match `view_proj`.
        let nx = (screen.x / self.viewport.x) * 2.0 - 1.0;
        let ny = 1.0 - (screen.y / self.viewport.y) * 2.0;
        Vec2::new(self.center.x + nx * h.x, self.center.y + ny * h.y)
    }

    /// Pan by a screen-pixel delta (e.g. from a mouse drag).
    pub fn pan_pixels(&mut self, delta: Vec2) {
        // Dragging right should move the world right (content follows cursor).
        self.center.x -= delta.x / self.zoom.max(1e-4);
        self.center.y += delta.y / self.zoom.max(1e-4);
    }

    /// Zoom by `factor` (e.g. 1.1 in, 0.9 out) keeping the world point under
    /// `anchor` (screen pixels) fixed.
    pub fn zoom_at(&mut self, anchor: Vec2, factor: f32) {
        let before = self.screen_to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(1e-3, 1e5);
        let after = self.screen_to_world(anchor);
        self.center += before - after;
    }

    /// Frame a bounding box `(min, max)` into the viewport with padding.
    pub fn fit(&mut self, min: Vec2, max: Vec2) {
        let size = (max - min).max(Vec2::splat(1.0));
        self.center = (min + max) * 0.5;
        let zx = self.viewport.x / (size.x * 1.15);
        let zy = self.viewport.y / (size.y * 1.15);
        self.zoom = zx.min(zy).clamp(1e-3, 1e5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_center_maps_to_camera_center() {
        let cam = Camera2D {
            center: Vec2::new(10.0, 20.0),
            zoom: 2.0,
            viewport: Vec2::new(800.0, 600.0),
        };
        let w = cam.screen_to_world(Vec2::new(400.0, 300.0));
        assert!((w - cam.center).length() < 1e-3);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut cam = Camera2D {
            center: Vec2::ZERO,
            zoom: 1.0,
            viewport: Vec2::new(800.0, 600.0),
        };
        let anchor = Vec2::new(600.0, 200.0);
        let before = cam.screen_to_world(anchor);
        cam.zoom_at(anchor, 1.5);
        let after = cam.screen_to_world(anchor);
        assert!((before - after).length() < 1e-2);
    }
}
