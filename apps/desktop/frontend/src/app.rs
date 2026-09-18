//! The Leptos shell: a toolbar (repo path + analyze), the wgpu graph canvas,
//! and a side panel with the overview, the selected-node detail and a legend.
//!
//! Reactive UI state lives in Leptos signals; the GPU viewer and the graph it
//! renders live in a plain `Rc<RefCell<_>>` (wgpu handles are `!Send` and don't
//! belong in the reactive graph).

use std::cell::RefCell;
use std::rc::Rc;

use leptos::html::Canvas;
use leptos::prelude::*;
use lcw_core::{CodeGraph, Diagnostic, Suggestion, Summary};
use lcw_render::web::WebViewer;
use lcw_render::{build_scene, SceneData};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlCanvasElement, MouseEvent, WheelEvent};

use crate::transport::{self, EngineTransport, TauriTransport};
use crate::viewer::{detail_for, hit_test, NodeDetail};

/// Non-reactive render state (GPU viewer + data needed for interaction).
#[derive(Default)]
struct RenderState {
    canvas: Option<HtmlCanvasElement>,
    viewer: Option<WebViewer>,
    scene: Option<SceneData>,
    graph: Option<CodeGraph>,
    /// Last cursor position while a drag is in progress.
    drag: Option<(f64, f64)>,
    /// Whether the pointer moved meaningfully since mousedown (drag vs click).
    moved: bool,
}

type Shared = Rc<RefCell<RenderState>>;

#[component]
pub fn App() -> impl IntoView {
    let state: Shared = Rc::new(RefCell::new(RenderState::default()));

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

    // Stream backend progress into the status line.
    transport::on_progress(move |p| status.set(format!("[{}] {}", p.phase, p.message)));

    let canvas_ref = NodeRef::<Canvas>::new();

    // Once the canvas is connected to the DOM, remember it and wire interaction.
    {
        let state = state.clone();
        canvas_ref.on_load(move |canvas: HtmlCanvasElement| {
            resize_canvas(&canvas);
            state.borrow_mut().canvas = Some(canvas.clone());
            wire_pointer_events(&canvas, state.clone(), selected);
            wire_resize(state.clone());
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
                        let scene = build_scene(&graph, &view.positions);
                        {
                            let mut s = state.borrow_mut();
                            s.graph = Some(graph);
                            s.scene = Some(scene);
                        }
                        if let Err(e) = present_scene(&state).await {
                            error.set(Some(e));
                        } else {
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
            let mut s = state.borrow_mut();
            if let (Some(scene), Some(viewer)) = (s.scene.clone(), s.viewer.as_mut()) {
                viewer.fit(scene.min, scene.max);
                viewer.render();
            }
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
            </header>

            <div class="body">
                <canvas node_ref=canvas_ref class="graph" width="1000" height="700"></canvas>
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

fn wire_pointer_events(canvas: &HtmlCanvasElement, state: Shared, selected: RwSignal<Option<NodeDetail>>) {
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
                    viewer.render();
                }
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
            let mut s = state.borrow_mut();
            if let Some(viewer) = s.viewer.as_mut() {
                viewer.zoom_at(x, y, factor);
                viewer.render();
            }
        }) as Box<dyn FnMut(WheelEvent)>);
        let _ = canvas.add_event_listener_with_callback("wheel", cb.as_ref().unchecked_ref());
        cb.forget();
    }
}

/// Keep the canvas + camera in sync with window resizes.
fn wire_resize(state: Shared) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::wrap(Box::new(move |_ev: web_sys::Event| {
        let mut s = state.borrow_mut();
        if let Some(canvas) = s.canvas.clone() {
            resize_canvas(&canvas);
            let (w, h) = (canvas.width(), canvas.height());
            if let Some(viewer) = s.viewer.as_mut() {
                viewer.resize(w, h);
                viewer.render();
            }
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = window.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
    cb.forget();
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
