//! Minimal bitmap text: turn short ASCII strings into world-space triangles so
//! the renderer can draw labels without a font atlas, a texture, or any extra
//! bind group. Each glyph is an 8x8 cell from [`font8x8`]; every lit pixel
//! becomes a small quad (two triangles) in the shared [`EdgeVertex`] format, so
//! labels ride the same solid-fill pipeline as the grouping rectangles.
//!
//! Text is placed in *world* space (so it pans/zooms with the graph, exactly
//! like the module boxes it annotates). Anchoring is top-left, +X right and
//! +Y up, matching the camera.

use font8x8::{UnicodeFonts, BASIC_FONTS};

use crate::scene::EdgeVertex;

/// Cell advance in "pixels" (8 wide glyph + 1 px inter-glyph gap).
pub const ADVANCE_CELLS: f32 = 9.0;
/// Full line height in "pixels" (8 tall glyph + 2 px leading).
pub const LINE_CELLS: f32 = 10.0;

/// Width, in world units, that [`emit`] will occupy for `text` at pixel size
/// `px` (one 8x8 cell spans `8 * px`; glyphs advance `ADVANCE_CELLS * px`).
pub fn width(text: &str, px: f32) -> f32 {
    let count = text.chars().count();
    if count == 0 {
        0.0
    } else {
        // n glyphs -> (n-1) advances + one glyph body.
        (count as f32 - 1.0) * ADVANCE_CELLS * px + 8.0 * px
    }
}

/// Emit filled-pixel triangles for `text`, top-left anchored at world `origin`,
/// with each glyph pixel `px` world units on a side and painted `rgba`.
pub fn emit(out: &mut Vec<EdgeVertex>, text: &str, origin: [f32; 2], px: f32, rgba: [f32; 4]) {
    let mut cx = origin[0];
    let top = origin[1];
    for ch in text.chars() {
        if ch != ' ' {
            if let Some(glyph) = BASIC_FONTS.get(ch) {
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..8u32 {
                        if bits & (1 << col) != 0 {
                            let x0 = cx + col as f32 * px;
                            // row 0 is the glyph's top; +Y is up, so descend.
                            let y0 = top - row as f32 * px;
                            push_pixel(out, x0, y0, px, rgba);
                        }
                    }
                }
            }
        }
        cx += ADVANCE_CELLS * px;
    }
}

/// A single filled pixel: the square `[x0, x0+px] x [y0-px, y0]` as two tris.
fn push_pixel(out: &mut Vec<EdgeVertex>, x0: f32, y0: f32, px: f32, color: [f32; 4]) {
    let x1 = x0 + px;
    let y1 = y0 - px;
    let v = |x: f32, y: f32| EdgeVertex { pos: [x, y], color };
    out.push(v(x0, y0));
    out.push(v(x1, y0));
    out.push(v(x1, y1));
    out.push(v(x0, y0));
    out.push(v(x1, y1));
    out.push(v(x0, y1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_emits_nothing() {
        let mut out = Vec::new();
        emit(&mut out, "", [0.0, 0.0], 1.0, [1.0; 4]);
        assert!(out.is_empty());
        assert_eq!(width("", 3.0), 0.0);
    }

    #[test]
    fn width_scales_with_length_and_px() {
        // 8*px body for one glyph; +ADVANCE per extra glyph.
        assert_eq!(width("A", 2.0), 16.0);
        assert_eq!(width("AB", 2.0), (ADVANCE_CELLS + 8.0) * 2.0);
    }

    #[test]
    fn glyph_emits_triangles_in_multiples_of_six() {
        let mut out = Vec::new();
        emit(&mut out, "A", [0.0, 0.0], 1.0, [1.0, 1.0, 1.0, 1.0]);
        assert!(!out.is_empty());
        assert_eq!(out.len() % 6, 0, "each lit pixel is two triangles");
    }

    #[test]
    fn space_is_blank_but_advances() {
        let mut out = Vec::new();
        emit(&mut out, " ", [0.0, 0.0], 1.0, [1.0; 4]);
        assert!(out.is_empty());
    }
}
