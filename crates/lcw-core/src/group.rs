//! **Group boxes**: the rectangles of a grouped layout, where each crate (or
//! top-level module) is drawn as a box around its own functions.
//!
//! The layout (`lcw-layout`) produces them and the renderers (`lcw-render`,
//! native and wasm) draw them; they cross the engine/UI boundary inside the
//! graph view, which is why the type lives here rather than in either side.

use serde::{Deserialize, Serialize};

/// Height of the label band across the top of every group box, in world units.
/// The layout keeps nodes out of it and the renderer writes the group's name
/// in it, so both sides must agree on the number.
pub const GROUP_HEADER: f32 = 72.0;

/// One labelled rectangle of a grouped layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupBox {
    /// Display name: the crate, or `crate::module` when the repository has a
    /// single crate and is split by its top-level modules instead.
    pub name: String,
    /// Bottom-left corner, world units (y grows upward).
    pub min: [f32; 2],
    /// Top-right corner. The top [`GROUP_HEADER`] units are the label band.
    pub max: [f32; 2],
    /// Number of graph nodes inside.
    pub members: u32,
    /// Whether this box holds the code outside the repository (std, other
    /// dependencies) rather than one of its own parts.
    pub external: bool,
}

impl GroupBox {
    /// Whether `p` lies inside the box (edges included).
    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }
}
