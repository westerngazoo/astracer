//! Rendering-adjacent helpers that don't belong in the UI components: CPU-side
//! hit testing (the webview can't reliably read back the GPU pick buffer).
//! Everything *about* a picked node (its card, callers/callees, call tree)
//! comes from `lcw-query`, so this file stays tiny.

use lcw_render::SceneData;

/// Return the index of the node whose disc contains `world`, nearest first.
///
/// Scene node centers/radii are in world units, so the caller converts the
/// cursor to world space (via `WebViewer::screen_to_world`) before calling.
pub fn hit_test(scene: &SceneData, world: [f32; 2]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_dist = f32::MAX;
    for (i, node) in scene.nodes.iter().enumerate() {
        let dx = node.center[0] - world[0];
        let dy = node.center[1] - world[1];
        let dist = (dx * dx + dy * dy).sqrt();
        // A few px of slop so small nodes stay clickable.
        if dist <= node.radius + 3.0 && dist < best_dist {
            best_dist = dist;
            best = Some(i);
        }
    }
    best
}
