//! The Leptos shell: a toolbar (repo path + analyze), the wgpu graph canvas,
//! and a side panel with the overview, the selected-node detail and a legend.
//!
//! Reactive UI state lives in Leptos signals; the GPU viewer and the graph it
//! renders live in a plain `Rc<RefCell<_>>` (wgpu handles are `!Send` and don't
//! belong in the reactive graph).

use std::cell::RefCell;
use std::rc::Rc;

use lcw_core::{CodeGraph, Diagnostic, Suggestion, Summary};
use lcw_render::web::WebViewer;
use lcw_render::{
    apply_filter, build_scene_with, compute_matches, filter_is_active, select_labels,
    BundleOptions, FilterStyle, LabelOptions, MinimapView, SceneData, SceneOptions,
};
use leptos::html::Canvas;
use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, MouseEvent, WheelEvent};

use crate::transport::{self, EngineTransport, TauriTransport};
use crate::viewer::{detail_for, hit_test, NodeDetail};

/// A single projected node label for the DOM overlay (Feature 1).
#[derive(Clone, Debug, PartialEq)]
struct LabelBox {
    text: String,
    /// Canvas-pixel position of the node center.
    x: f64,
    y: f64,
    /// Font size in px, scaled with the node's on-screen size.
    font: f64,
}

/// Inset (px) used when fitting the graph into the minimap.
const MINIMAP_PADDING: f32 = 8.0;

/// Non-reactive render state (GPU viewer + data needed for interaction).
#[derive(Default)]
struct RenderState {
    canvas: Option<HtmlCanvasElement>,
    minimap: Option<HtmlCanvasElement>,
    viewer: Option<WebViewer>,
    /// The scene currently on screen (after bundling + filter styling).
    scene: Option<SceneData>,
    graph: Option<CodeGraph>,
    /// Raw per-node positions from the transport, kept so the display scene can
    /// be rebuilt when bundling/filter toggles change.
    positions: Vec<[f32; 2]>,
    /// `(short_name, qualified_name)` per node index, for labels + search.
    names: Vec<(String, String)>,
    /// Current toolbar state mirrored here so the pure rebuild is signal-free.
    query: String,
    bundle: bool,
    labels_on: bool,
    /// Last cursor position while a drag is in progress.
    drag: Option<(f64, f64)>,
    /// Whether the pointer moved meaningfully since mousedown (drag vs click).
    moved: bool,
    /// Whether a minimap drag (recenter) is in progress.
    mm_drag: bool,
}

impl RenderState {
    /// Build the display scene from the raw graph + positions, applying the
    /// current bundling and search-filter toggles. All the heavy lifting lives
    /// in `lcw-render` pure functions (unit-tested there).
    fn compose_scene(&self) -> Option<SceneData> {
        let graph = self.graph.as_ref()?;
        let opts = SceneOptions {
            bundle: self.bundle.then(BundleOptions::default),
        };
        let base = build_scene_with(graph, &self.positions, &opts);
        if filter_is_active(&self.query) {
            let matched = compute_matches(
                &self.query,
                self.names.iter().map(|(n, q)| (n.as_str(), q.as_str())),
            );
            Some(apply_filter(&base, &matched, FilterStyle::default()))
        } else {
            Some(base)
        }
    }
}

type Shared = Rc<RefCell<RenderState>>;

#[component]
pub fn App() -> impl IntoView {
    let state: Shared = Rc::new(RefCell::new(RenderState {
        labels_on: true,
        ..RenderState::default()
    }));

    let path = RwSignal::new(String::new());
    let semantic = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let status = RwSignal::new(String::from("Idle — enter a repository path and analyze."));
    let error = RwSignal::new(Option::<String>::None);
    let summary = RwSignal::new(Option::<Summary>::None);
    let vertical = RwSignal::new(String::new());
    let suggestions = RwSignal::new(Vec::<Suggestion>::new());
    let diagnostics = RwSignal::new(Vec::<Diagnostic>::new());
    let selected = RwSignal::new(Option::<NodeDetail>::None);

    // Toolbar state for the new render/UX features.
    let query = RwSignal::new(String::new());
    let show_labels = RwSignal::new(true);
    let bundle = RwSignal::new(false);
    // Projected labels for the DOM overlay; updated on every camera change.
    let labels = RwSignal::new(Vec::<LabelBox>::new());

    // Stream backend progress into the status line.
    transport::on_progress(move |p| status.set(format!("[{}] {}", p.phase, p.message)));

    let canvas_ref = NodeRef::<Canvas>::new();
    let minimap_ref = NodeRef::<Canvas>::new();

    // Once the canvas is connected to the DOM, remember it and wire interaction.
    {
        let state = state.clone();
        canvas_ref.on_load(move |canvas: HtmlCanvasElement| {
            resize_canvas(&canvas);
            state.borrow_mut().canvas = Some(canvas.clone());
            wire_pointer_events(&canvas, state.clone(), selected, labels);
            wire_resize(state.clone(), labels);
        });
    }

    // The minimap canvas: remember it and wire click/drag-to-recenter.
    {
        let state = state.clone();
        minimap_ref.on_load(move |canvas: HtmlCanvasElement| {
            state.borrow_mut().minimap = Some(canvas.clone());
            wire_minimap(&canvas, state.clone(), labels);
            redraw_minimap(&state);
        });
    }

    let on_analyze = {
        let state = state.clone();
        move |_| {
            let repo = path.get();
            if repo.trim().is_empty() {
                error.set(Some("Please enter a repository path.".into()));
                return;
            }
            let state = state.clone();
            let sem = semantic.get();
            busy.set(true);
            error.set(None);
            selected.set(None);
            status.set("Starting analysis…".into());
            spawn_local(async move {
                match TauriTransport.analyze(repo, sem).await {
                    Ok(view) => {
                        let report = view.report;
                        summary.set(Some(report.summary));
                        vertical.set(report.vertical.to_string());
                        suggestions.set(report.suggestions.clone());
                        diagnostics.set(report.diagnostics.clone());
                        let graph = CodeGraph::from_snapshot(report.graph);
                        let names: Vec<(String, String)> = graph
                            .nodes()
                            .map(|n| (n.name.clone(), n.qualified_name.clone()))
                            .collect();
                        let scene = {
                            let mut s = state.borrow_mut();
                            s.graph = Some(graph);
                            s.positions = view.positions.clone();
                            s.names = names;
                            s.compose_scene()
                        };
                        state.borrow_mut().scene = scene;
                        if let Err(e) = present_scene(&state).await {
                            error.set(Some(e));
                        } else {
                            refresh_view(&state, labels);
                            status.set("Analysis complete.".into());
                        }
                    }
                    Err(e) => {
                        status.set("Analysis failed.".into());
                        error.set(Some(e));
                    }
                }
                busy.set(false);
            });
        }
    };

    let on_fit = {
        let state = state.clone();
        move |_| {
            {
                let mut s = state.borrow_mut();
                if let (Some(scene), Some(viewer)) = (s.scene.clone(), s.viewer.as_mut()) {
                    viewer.fit(scene.min, scene.max);
                }
            }
            refresh_view(&state, labels);
        }
    };

    // Search box: recompute matches + re-color the scene without moving the view.
    let on_search = {
        let state = state.clone();
        move |ev: web_sys::Event| {
            let q = target_value(&ev);
            query.set(q.clone());
            state.borrow_mut().query = q;
            rebuild_and_refresh(&state, labels);
        }
    };

    // Toggle curved edge bundling (rebuilds edge geometry, keeps the camera).
    let on_bundle = {
        let state = state.clone();
        move |ev: web_sys::Event| {
            let on = target_checked(&ev);
            bundle.set(on);
            state.borrow_mut().bundle = on;
            rebuild_and_refresh(&state, labels);
        }
    };

    // Toggle the label overlay (cheap: just recompute the projected set).
    let on_labels = {
        let state = state.clone();
        move |ev: web_sys::Event| {
            let on = target_checked(&ev);
            show_labels.set(on);
            state.borrow_mut().labels_on = on;
            refresh_view(&state, labels);
        }
    };

    view! {
        <div class="app">
            <header class="toolbar">
                <div class="brand">"Live Code Walk"</div>
                <input
                    class="path"
                    type="text"
                    placeholder="/path/to/rust/repo"
                    prop:value=move || path.get()
                    on:input=move |ev| path.set(target_value(&ev))
                />
                <label class="mode" title="Use the rust-analyzer backend when available">
                    <input
                        type="checkbox"
                        prop:checked=move || semantic.get()
                        on:change=move |ev| semantic.set(target_checked(&ev))
                    />
                    "semantic"
                </label>
                <button class="primary" on:click=on_analyze disabled=move || busy.get()>
                    {move || if busy.get() { "Analyzing…" } else { "Analyze" }}
                </button>
                <button on:click=on_fit>"Fit"</button>
                <input
                    class="search"
                    type="search"
                    placeholder="search nodes…"
                    prop:value=move || query.get()
                    on:input=on_search
                />
                <label class="mode" title="Show labels for the most important nodes">
                    <input
                        type="checkbox"
                        prop:checked=move || show_labels.get()
                        on:change=on_labels
                    />
                    "labels"
                </label>
                <label class="mode" title="Bundle edges into curved lanes to reduce clutter">
                    <input
                        type="checkbox"
                        prop:checked=move || bundle.get()
                        on:change=on_bundle
                    />
                    "bundle"
                </label>
            </header>

            <div class="body">
                <div class="stage">
                    <canvas node_ref=canvas_ref class="graph" width="1000" height="700"></canvas>
                    <div class="labels">
                        {move || {
                            labels
                                .get()
                                .into_iter()
                                .map(|l| {
                                    let style = format!(
                                        "left:{:.1}px;top:{:.1}px;font-size:{:.0}px;",
                                        l.x,
                                        l.y,
                                        l.font,
                                    );
                                    view! { <span class="node-label" style=style>{l.text}</span> }
                                })
                                .collect::<Vec<_>>()
                        }}
                    </div>
                    <canvas
                        node_ref=minimap_ref
                        class="minimap"
                        width="200"
                        height="136"
                        title="Overview — click or drag to recenter"
                    ></canvas>
                </div>
                <aside class="side">
                    {move || error.get().map(|e| view! { <div class="error">{e}</div> })}
                    <Overview summary=summary vertical=vertical />
                    <Detail selected=selected />
                    <Suggestions suggestions=suggestions />
                    <Diagnostics diagnostics=diagnostics />
                    <Legend />
                </aside>
            </div>

            <footer class="status">{move || status.get()}</footer>
        </div>
    }
}

#[component]
fn Overview(summary: RwSignal<Option<Summary>>, vertical: RwSignal<String>) -> impl IntoView {
    move || {
        summary.get().map(|s| {
            let vertical = vertical.get();
            let vertical_row = (!vertical.is_empty())
                .then(|| view! { <li><span>"Vertical"</span><b>{vertical}</b></li> });
            view! {
                <div class="card">
                    <h3>"Overview"</h3>
                    <ul class="stats">
                        {vertical_row}
                        <li><span>"Files"</span><b>{s.files}</b></li>
                        <li><span>"Functions"</span><b>{s.nodes}</b></li>
                        <li><span>"Calls"</span><b>{s.edges}</b></li>
                        <li><span>"External"</span><b>{s.external_nodes}</b></li>
                        <li><span>"Diagnostics"</span><b>{s.diagnostics}</b></li>
                        <li><span>"Suggestions"</span><b>{s.suggestions}</b></li>
                    </ul>
                </div>
            }
        })
    }
}

#[component]
fn Suggestions(suggestions: RwSignal<Vec<Suggestion>>) -> impl IntoView {
    move || {
        let items = suggestions.get();
        (!items.is_empty()).then(|| {
            let rows = items
                .into_iter()
                .map(|s| {
                    let targets = s
                        .targets
                        .iter()
                        .map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    view! {
                        <li class="sugg">
                            <div class="sugg-head">
                                <span class="prio">{s.priority}</span>
                                <b>{s.title.clone()}</b>
                            </div>
                            <div class="sugg-why">{s.rationale.clone()}</div>
                            <div class="sugg-tgt">{targets}</div>
                        </li>
                    }
                })
                .collect::<Vec<_>>();
            view! {
                <div class="card">
                    <h3>"Suggestions"</h3>
                    <ul class="list">{rows}</ul>
                </div>
            }
        })
    }
}

#[component]
fn Diagnostics(diagnostics: RwSignal<Vec<Diagnostic>>) -> impl IntoView {
    move || {
        let items = diagnostics.get();
        (!items.is_empty()).then(|| {
            let total = items.len();
            let rows = items
                .iter()
                .take(14)
                .map(|d| {
                    let sev = d.severity.to_string();
                    view! {
                        <li class=format!("diag sev-{sev}")>
                            <span class="sev">{sev.clone()}</span>
                            {d.message.clone()}
                        </li>
                    }
                })
                .collect::<Vec<_>>();
            view! {
                <div class="card">
                    <h3>{format!("Diagnostics ({total})")}</h3>
                    <ul class="list">{rows}</ul>
                </div>
            }
        })
    }
}

#[component]
fn Detail(selected: RwSignal<Option<NodeDetail>>) -> impl IntoView {
    move || {
        selected.get().map(|d| {
            let flags = (!d.flags.is_empty())
                .then(|| view! { <div class="flags">{d.flags.join(" · ")}</div> });
            view! {
                <div class="card">
                    <h3>"Node"</h3>
                    <div class="qname">{d.qualified_name.clone()}</div>
                    <ul class="stats">
                        <li><span>"kind"</span><b>{d.kind.clone()}</b></li>
                        <li><span>"location"</span><b>{d.location.clone()}</b></li>
                        <li><span>"complexity"</span><b>{d.complexity}</b></li>
                        <li><span>"fan-in"</span><b>{d.fan_in}</b></li>
                        <li><span>"fan-out"</span><b>{d.fan_out}</b></li>
                        <li><span>"LOC"</span><b>{d.lines_of_code}</b></li>
                        <li><span>"params"</span><b>{d.parameters}</b></li>
                        <li><span>"allocs"</span><b>{d.allocations}</b></li>
                    </ul>
                    {flags}
                </div>
            }
        })
    }
}

#[component]
fn Legend() -> impl IntoView {
    view! {
        <div class="card legend">
            <h3>"Legend"</h3>
            <ul>
                <li><i class="dot fn"></i>"function"</li>
                <li><i class="dot method"></i>"method"</li>
                <li><i class="dot closure"></i>"closure"</li>
                <li><i class="dot external"></i>"external / unresolved"</li>
            </ul>
            <p class="hint">"Size ∝ fan-in · warmer ∝ complexity"</p>
            <p class="hint">"Drag to pan · scroll to zoom · click a node"</p>
        </div>
    }
}

/// Create the viewer on first use, otherwise swap the scene; then draw. Care is
/// taken never to hold the `RefCell` borrow across the async device request.
async fn present_scene(state: &Shared) -> Result<(), String> {
    let (need_viewer, canvas, scene) = {
        let s = state.borrow();
        (s.viewer.is_none(), s.canvas.clone(), s.scene.clone())
    };
    let Some(scene) = scene else {
        return Ok(());
    };

    if need_viewer {
        let canvas = canvas.ok_or_else(|| "canvas is not ready yet".to_string())?;
        resize_canvas(&canvas);
        let viewer = WebViewer::new(canvas, &scene).await?;
        state.borrow_mut().viewer = Some(viewer);
    } else if let Some(viewer) = state.borrow_mut().viewer.as_mut() {
        viewer.set_scene(&scene);
    }

    if let Some(viewer) = state.borrow_mut().viewer.as_mut() {
        viewer.render();
    }
    Ok(())
}

/// Match the canvas backing store to its CSS pixel size so mouse coordinates
/// and world coordinates stay 1:1 (keeps hit testing honest).
fn resize_canvas(canvas: &HtmlCanvasElement) {
    let w = canvas.client_width();
    let h = canvas.client_height();
    if w > 0 {
        canvas.set_width(w as u32);
    }
    if h > 0 {
        canvas.set_height(h as u32);
    }
}

fn wire_pointer_events(
    canvas: &HtmlCanvasElement,
    state: Shared,
    selected: RwSignal<Option<NodeDetail>>,
    labels: RwSignal<Vec<LabelBox>>,
) {
    // mousedown: begin a potential drag.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            let mut s = state.borrow_mut();
            s.drag = Some((ev.offset_x() as f64, ev.offset_y() as f64));
            s.moved = false;
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // mousemove: pan while dragging.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            let panned = {
                let mut s = state.borrow_mut();
                if let Some((lx, ly)) = s.drag {
                    let x = ev.offset_x() as f64;
                    let y = ev.offset_y() as f64;
                    let dx = (x - lx) as f32;
                    let dy = (y - ly) as f32;
                    if dx.abs() + dy.abs() > 2.0 {
                        s.moved = true;
                    }
                    s.drag = Some((x, y));
                    if let Some(viewer) = s.viewer.as_mut() {
                        viewer.pan(dx, dy);
                    }
                    true
                } else {
                    false
                }
            };
            if panned {
                refresh_view(&state, labels);
            }
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // mouseup: a click (no drag) selects the node under the cursor.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            let x = ev.offset_x() as f32;
            let y = ev.offset_y() as f32;
            let mut s = state.borrow_mut();
            let is_click = s.drag.is_some() && !s.moved;
            s.drag = None;
            if !is_click {
                return;
            }
            let detail = match (s.viewer.as_ref(), s.scene.as_ref(), s.graph.as_ref()) {
                (Some(viewer), Some(scene), Some(graph)) => {
                    let world = viewer.screen_to_world(x, y);
                    hit_test(scene, world).and_then(|i| detail_for(graph, i))
                }
                _ => None,
            };
            drop(s);
            selected.set(detail);
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // wheel: zoom around the cursor.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: WheelEvent| {
            ev.prevent_default();
            let x = ev.offset_x() as f32;
            let y = ev.offset_y() as f32;
            let factor = if ev.delta_y() < 0.0 { 1.1 } else { 1.0 / 1.1 };
            {
                let mut s = state.borrow_mut();
                if let Some(viewer) = s.viewer.as_mut() {
                    viewer.zoom_at(x, y, factor);
                }
            }
            refresh_view(&state, labels);
        }) as Box<dyn FnMut(WheelEvent)>);
        let _ = canvas.add_event_listener_with_callback("wheel", cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

/// Keep the canvas + camera in sync with window resizes.
fn wire_resize(state: Shared, labels: RwSignal<Vec<LabelBox>>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::wrap(Box::new(move |_ev: web_sys::Event| {
        {
            let mut s = state.borrow_mut();
            if let Some(canvas) = s.canvas.clone() {
                resize_canvas(&canvas);
                let (w, h) = (canvas.width(), canvas.height());
                if let Some(viewer) = s.viewer.as_mut() {
                    viewer.resize(w, h);
                }
            }
        }
        refresh_view(&state, labels);
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = window.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// Render the viewer, then refresh the DOM label overlay and the minimap. This
/// is the single "the camera or scene changed" entry point, so labels and the
/// minimap viewport rectangle always track the main view.
fn refresh_view(state: &Shared, labels: RwSignal<Vec<LabelBox>>) {
    {
        let mut s = state.borrow_mut();
        if let Some(viewer) = s.viewer.as_mut() {
            viewer.render();
        }
    }
    let boxes = {
        let s = state.borrow();
        compute_label_boxes(&s)
    };
    labels.set(boxes);
    redraw_minimap(state);
}

/// Rebuild the display scene from the toggles (bundling / search), swap it into
/// the viewer *without* moving the camera, then refresh overlays.
fn rebuild_and_refresh(state: &Shared, labels: RwSignal<Vec<LabelBox>>) {
    let scene = {
        let s = state.borrow();
        s.compose_scene()
    };
    if let Some(scene) = scene {
        {
            let mut s = state.borrow_mut();
            if let Some(viewer) = s.viewer.as_mut() {
                viewer.replace_scene(&scene);
            }
            s.scene = Some(scene);
        }
        refresh_view(state, labels);
    }
}

/// Project the important nodes to screen space for the DOM label overlay.
fn compute_label_boxes(s: &RenderState) -> Vec<LabelBox> {
    if !s.labels_on {
        return Vec::new();
    }
    let (Some(viewer), Some(scene)) = (s.viewer.as_ref(), s.scene.as_ref()) else {
        return Vec::new();
    };
    let camera = viewer.camera();
    select_labels(&scene.nodes, &camera, &LabelOptions::default())
        .into_iter()
        .filter_map(|p| {
            let (short, _qualified) = s.names.get(p.index)?;
            let font = (9.0 + ((p.radius_px - 7.0) * 0.5) as f64).clamp(9.0, 15.0);
            Some(LabelBox {
                text: short.clone(),
                x: p.screen[0] as f64,
                y: p.screen[1] as f64,
                font,
            })
        })
        .collect()
}

/// Draw the whole-graph overview (node dots + current viewport rectangle) on
/// the 2D minimap canvas.
fn redraw_minimap(state: &Shared) {
    let s = state.borrow();
    let (Some(canvas), Some(scene), Some(viewer)) =
        (s.minimap.as_ref(), s.scene.as_ref(), s.viewer.as_ref())
    else {
        return;
    };
    let ctx = match canvas.get_context("2d") {
        Ok(Some(obj)) => match obj.dyn_into::<CanvasRenderingContext2d>() {
            Ok(c) => c,
            Err(_) => return,
        },
        _ => return,
    };
    let w = canvas.width() as f64;
    let h = canvas.height() as f64;

    ctx.set_fill_style_str("rgba(9,12,18,0.92)");
    ctx.fill_rect(0.0, 0.0, w, h);
    ctx.set_stroke_style_str("#2a323d");
    ctx.set_line_width(1.0);
    ctx.stroke_rect(0.5, 0.5, w - 1.0, h - 1.0);

    let view = MinimapView::new(scene.min, scene.max, [w as f32, h as f32], MINIMAP_PADDING);
    for node in &scene.nodes {
        let p = view.world_to_minimap(node.center);
        let a = node.color[3].clamp(0.0, 1.0);
        let col = format!(
            "rgba({},{},{},{:.3})",
            (node.color[0] * 255.0) as u8,
            (node.color[1] * 255.0) as u8,
            (node.color[2] * 255.0) as u8,
            a
        );
        ctx.set_fill_style_str(&col);
        let r = (node.radius * view.scale()).clamp(0.6, 3.0) as f64;
        ctx.begin_path();
        let _ = ctx.arc(p[0] as f64, p[1] as f64, r, 0.0, std::f64::consts::TAU);
        ctx.fill();
    }

    let rect = view.viewport_rect(&viewer.camera());
    ctx.set_stroke_style_str("#4c8cf2");
    ctx.set_line_width(1.5);
    ctx.stroke_rect(rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
}

/// Wire minimap click/drag to recenter the camera on the clicked world point.
fn wire_minimap(canvas: &HtmlCanvasElement, state: Shared, labels: RwSignal<Vec<LabelBox>>) {
    fn recenter(state: &Shared, labels: RwSignal<Vec<LabelBox>>, x: f32, y: f32) {
        let world = {
            let s = state.borrow();
            let (Some(canvas), Some(scene)) = (s.minimap.as_ref(), s.scene.as_ref()) else {
                return;
            };
            let w = canvas.width() as f32;
            let h = canvas.height() as f32;
            let view = MinimapView::new(scene.min, scene.max, [w, h], MINIMAP_PADDING);
            view.minimap_to_world([x, y])
        };
        {
            let mut s = state.borrow_mut();
            if let Some(viewer) = s.viewer.as_mut() {
                viewer.center_on(world[0], world[1]);
            }
        }
        refresh_view(state, labels);
    }

    // mousedown: start recenter drag and jump immediately.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            state.borrow_mut().mm_drag = true;
            recenter(&state, labels, ev.offset_x() as f32, ev.offset_y() as f32);
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // mousemove: keep recentering while the button is held.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            let dragging = state.borrow().mm_drag;
            if dragging {
                recenter(&state, labels, ev.offset_x() as f32, ev.offset_y() as f32);
            }
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref());
        cb.forget();
    }

    // mouseup / mouseleave: end the drag.
    for evname in ["mouseup", "mouseleave"] {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |_ev: MouseEvent| {
            state.borrow_mut().mm_drag = false;
        }) as Box<dyn FnMut(MouseEvent)>);
        let _ = canvas.add_event_listener_with_callback(evname, cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

fn target_value(ev: &web_sys::Event) -> String {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|el| el.value())
        .unwrap_or_default()
}

fn target_checked(ev: &web_sys::Event) -> bool {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|el| el.checked())
        .unwrap_or(false)
}
