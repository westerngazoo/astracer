//! The interactive **detail panel** (HUD): a screen-space side panel that the
//! native viewer draws over the graph when a node is selected. It reuses the
//! same solid-fill pipeline and [`crate::text`] emitter as the world-space
//! labels, but is authored in pixel space (see
//! [`crate::renderer::Renderer::update_hud_projection`]) so it stays pinned
//! while the graph pans and zooms.
//!
//! Pure presentation: the caller fills in a [`Panel`] (title, metrics, the
//! inputs/outputs lists, key hints); geometry and clipping live here.

use crate::scene::EdgeVertex;
use crate::text;

/// One labelled list within the panel (e.g. inputs, outputs, flow).
#[derive(Debug, Clone)]
pub struct Section {
    pub heading: String,
    pub items: Vec<String>,
    pub accent: [f32; 4],
}

/// The full panel model for one selected node.
#[derive(Debug, Clone, Default)]
pub struct Panel {
    pub title: String,
    pub subtitle: String,
    pub sections: Vec<Section>,
    /// Bottom-pinned key hints, drawn dimly.
    pub footer: Vec<String>,
}

const TITLE_PX: f32 = 2.6;
const SUB_PX: f32 = 1.7;
const HEAD_PX: f32 = 2.0;
const ITEM_PX: f32 = 1.7;
const FOOT_PX: f32 = 1.6;
const PAD: f32 = 18.0;

/// Build the panel geometry for a `width`x`height` framebuffer (pixels).
/// Returns screen-space triangles in the shared [`EdgeVertex`] format.
pub fn build(width: u32, height: u32, panel: &Panel) -> Vec<EdgeVertex> {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    let panel_w = (w * 0.42).clamp(360.0, 560.0).min(w);
    let x0 = w - panel_w;

    let inner_x = x0 + PAD + 8.0;
    let inner_w = panel_w - PAD * 2.0 - 8.0;
    let floor = PAD + 8.0 * FOOT_PX * panel.footer.len().max(1) as f32 + 24.0;

    let mut out = Vec::new();
    push_rect(&mut out, x0, 0.0, w, h, [0.09, 0.10, 0.13, 0.94]);
    push_rect(&mut out, x0, 0.0, x0 + 4.0, h, [0.30, 0.62, 0.98, 1.0]);

    let mut top = h - PAD - 8.0 * TITLE_PX;
    top = line(
        &mut out,
        &panel.title,
        inner_x,
        top,
        TITLE_PX,
        [0.98, 0.99, 1.0, 1.0],
        inner_w,
    );
    if !panel.subtitle.is_empty() {
        top -= 3.0;
        top = line(
            &mut out,
            &panel.subtitle,
            inner_x,
            top,
            SUB_PX,
            [0.64, 0.71, 0.82, 1.0],
            inner_w,
        );
    }
    // Divider under the header.
    top -= 8.0;
    push_rect(
        &mut out,
        inner_x,
        top,
        x0 + panel_w - PAD,
        top + 1.5,
        [0.24, 0.28, 0.36, 1.0],
    );
    top -= 12.0;

    for section in &panel.sections {
        if top < floor {
            break;
        }
        top = line(
            &mut out,
            &section.heading,
            inner_x,
            top,
            HEAD_PX,
            section.accent,
            inner_w,
        );
        top -= 3.0;
        for item in &section.items {
            if top < floor {
                line(
                    &mut out,
                    "...",
                    inner_x + 10.0,
                    top,
                    ITEM_PX,
                    [0.6, 0.6, 0.7, 1.0],
                    inner_w,
                );
                break;
            }
            top = line(
                &mut out,
                item,
                inner_x + 10.0,
                top,
                ITEM_PX,
                [0.85, 0.88, 0.94, 1.0],
                inner_w - 10.0,
            );
        }
        top -= 12.0;
    }

    // Footer pinned to the bottom, drawn bottom-up.
    let mut fy = PAD + 8.0 * FOOT_PX;
    for hint in panel.footer.iter().rev() {
        line(
            &mut out,
            hint,
            inner_x,
            fy,
            FOOT_PX,
            [0.55, 0.60, 0.70, 1.0],
            inner_w,
        );
        fy += 8.0 * FOOT_PX + 5.0;
    }

    out
}

/// Build a top-left **breadcrumb bar** (e.g. the traced flow chain) as a pinned
/// pill with amber accent, independent of the side panel. Concatenate its
/// vertices with a [`build`] result and upload them together.
pub fn breadcrumb(width: u32, height: u32, text: &str) -> Vec<EdgeVertex> {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    let px = 2.2;
    let pad = 12.0;
    let x0 = 16.0;
    let bar_w = (text::width(text, px) + pad * 2.0).min(w - 32.0);
    let y1 = h - 14.0;
    let y0 = y1 - (8.0 * px + pad);

    let mut out = Vec::new();
    push_rect(&mut out, x0, y0, x0 + bar_w, y1, [0.09, 0.10, 0.13, 0.93]);
    push_rect(&mut out, x0, y0, x0 + 4.0, y1, [0.96, 0.80, 0.30, 1.0]);
    text::emit(
        &mut out,
        &clip(text, px, bar_w - pad * 2.0),
        [x0 + pad, y1 - pad * 0.5],
        px,
        [0.97, 0.91, 0.72, 1.0],
    );
    out
}

/// Emit one clipped line anchored top-left at `(x, top)`; return the next
/// line's top.
fn line(
    out: &mut Vec<EdgeVertex>,
    text: &str,
    x: f32,
    top: f32,
    px: f32,
    color: [f32; 4],
    max_w: f32,
) -> f32 {
    text::emit(out, &clip(text, px, max_w), [x, top], px, color);
    top - (8.0 * px + 5.0)
}

/// Trim `text` (appending `..`) so it fits within `max_w` world units at `px`.
fn clip(text: &str, px: f32, max_w: f32) -> String {
    if text::width(text, px) <= max_w {
        return text.to_string();
    }
    const ELL: &str = "..";
    let mut kept = String::new();
    for ch in text.chars() {
        let mut trial = kept.clone();
        trial.push(ch);
        trial.push_str(ELL);
        if text::width(&trial, px) > max_w {
            break;
        }
        kept.push(ch);
    }
    kept.push_str(ELL);
    kept
}

fn push_rect(out: &mut Vec<EdgeVertex>, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4]) {
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
    fn empty_panel_still_draws_background() {
        let out = build(1200, 800, &Panel::default());
        // Background + accent border = 12 vertices minimum.
        assert!(out.len() >= 12);
    }

    #[test]
    fn panel_with_content_emits_more_than_background() {
        let bg = build(1200, 800, &Panel::default()).len();
        let panel = Panel {
            title: "lcw_engine::Engine::analyze".into(),
            subtitle: "lib.rs:151  method  cc 1".into(),
            sections: vec![Section {
                heading: "outputs (1)".into(),
                items: vec!["analyze_with_progress".into()],
                accent: [0.4, 0.8, 0.5, 1.0],
            }],
            footer: vec!["f flow  o open  esc clear".into()],
        };
        assert!(build(1200, 800, &panel).len() > bg);
    }

    #[test]
    fn clip_shortens_long_text() {
        let s = clip("a_very_long_symbol_name_that_will_not_fit", 2.0, 60.0);
        assert!(s.ends_with(".."));
        assert!(text::width(&s, 2.0) <= 60.0);
    }
}
