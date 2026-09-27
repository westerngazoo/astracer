//! The Leptos shell: a toolbar (repo path + analyze), an **Explorer** sidebar
//! (entry points + the crate ▸ module ▸ type ▸ function outline), the wgpu
//! graph canvas, and a detail pane that answers "what comes in / what goes
//! out" for the selected node, traces a flow between two nodes and expands
//! the call tree below the selection.
//!
//! Reactive UI state lives in Leptos signals; the GPU viewer and the graph it
//! renders live in a plain `Rc<RefCell<_>>` (wgpu handles are `!Send` and don't
//! belong in the reactive graph). Every navigation question is answered by
//! `lcw-query` — the same code the CLI and the native viewer run — so this
//! file is wiring and rendering only (Principle I).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use lcw_core::{CodeGraph, Diagnostic, NodeId, NodeKind, Suggestion, Summary};
use lcw_query::{
    call_tree, edge_kind_str, entry_points, kind_str, node_card, outline, reach_count,
    shortest_path, CallRef, CallTreeNode, CallTreeOptions, Direction, EntryKind, NodeCard, Outline,
    OutlineNode,
};
use lcw_render::web::WebViewer;
use lcw_render::{
    apply_filter, build_scene_with, compute_matches, filter_is_active, highlight, select_labels,
    BundleOptions, FilterStyle, LabelOptions, MinimapView, SceneData, SceneOptions,
};
use leptos::html::Canvas;
use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, MouseEvent, WheelEvent};

use crate::transport::{self, EngineTransport, FixtureTransport, TauriTransport};
use crate::viewer::hit_test;

/// Max hops when tracing a flow from the anchor to the selection.
const FLOW_MAX_DEPTH: u32 = 64;
/// Bounded call tree shown under the selected node.
const TREE_DEPTH: u32 = 3;
const TREE_WIDTH: usize = 8;
/// Zoom (px per world unit) guaranteed when the camera jumps to a node. Enough
/// to tell neighbors apart, low enough to keep the surrounding structure in
/// frame: jumping straight to maximum magnification loses the context that
/// makes the jump useful.
const FOCUS_ZOOM: f32 = 1.5;
/// Rows shown per entry-point kind before "… n more".
const ENTRIES_PER_KIND: usize = 12;
/// Callers/callees listed in the detail pane before "… n more".
const CALLS_SHOWN: usize = 30;
/// Inset (px) used when fitting the graph into the minimap.
const MINIMAP_PADDING: f32 = 8.0;

/// A single projected node label for the DOM overlay.
#[derive(Clone, Debug, PartialEq)]
struct LabelBox {
    text: String,
    /// Canvas-pixel position of the node center.
    x: f64,
    y: f64,
    /// Font size in px, scaled with the node's on-screen size.
    font: f64,
}

/// Non-reactive render state (GPU viewer + data needed for interaction).
#[derive(Default)]
struct RenderState {
    canvas: Option<HtmlCanvasElement>,
    minimap: Option<HtmlCanvasElement>,
    viewer: Option<WebViewer>,
    /// The scene currently on screen (after bundling + filter + highlight).
    scene: Option<SceneData>,
    graph: Option<CodeGraph>,
    /// Raw per-node positions from the transport, kept so the display scene can
    /// be rebuilt when bundling/filter/selection change.
    positions: Vec<[f32; 2]>,
    /// `(short_name, qualified_name)` per node index, for labels + search.
    names: Vec<(String, String)>,
    /// Current toolbar state mirrored here so the pure rebuild is signal-free.
    query: String,
    bundle: bool,
    labels_on: bool,
    /// Navigation state mirrored for the pure scene rebuild (see `Nav`).
    selected: Option<usize>,
    anchor: Option<usize>,
    path: Vec<usize>,
    /// Last cursor position while a drag is in progress.
    drag: Option<(f64, f64)>,
    /// Whether the pointer moved meaningfully since mousedown (drag vs click).
    moved: bool,
    /// Whether a minimap drag (recenter) is in progress.
    mm_drag: bool,
}

impl RenderState {
    /// Build the display scene from the raw graph + positions, applying the
    /// current bundling, search-filter and selection/flow highlighting. All
    /// the heavy lifting lives in `lcw-render` pure functions (unit-tested
    /// there).
    fn compose_scene(&self) -> Option<SceneData> {
        let graph = self.graph.as_ref()?;
        let opts = SceneOptions {
            bundle: self.bundle.then(BundleOptions::default),
        };
        let mut scene = build_scene_with(graph, &self.positions, &opts);
        if filter_is_active(&self.query) {
            let matched = compute_matches(
                &self.query,
                self.names.iter().map(|(n, q)| (n.as_str(), q.as_str())),
            );
            scene = apply_filter(&scene, &matched, FilterStyle::default());
        }
        if self.selected.is_some() || !self.path.is_empty() {
            let (nodes, edges) = highlight(&scene, self.selected, self.anchor, &self.path);
            scene.nodes = nodes;
            scene.edges = edges;
        }
        Some(scene)
    }
}

type Shared = Rc<RefCell<RenderState>>;

// ---------------------------------------------------------------------------
// Explorer data (computed once per analysis from `lcw-query`)
// ---------------------------------------------------------------------------

/// One row of the flattened outline tree (pre-order).
#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: usize,
    depth: u32,
    label: String,
    is_scope: bool,
    /// Graph node index, for functions.
    node: Option<usize>,
    functions: u32,
    entries: u32,
    entry: Option<EntryKind>,
    cyclomatic: u32,
    /// Index of the parent row in the flat list.
    parent: Option<usize>,
}

/// One entry point for the "start here" list.
#[derive(Clone, Debug, PartialEq)]
struct EntryRow {
    node: usize,
    name: String,
    kind: EntryKind,
    location: String,
    /// Functions reachable from a `main` (computed for mains only).
    reach: Option<usize>,
}

/// Everything the Explorer sidebar needs.
#[derive(Clone, Debug, PartialEq, Default)]
struct ExplorerData {
    outline: Outline,
    rows: Vec<Row>,
    entries: Vec<EntryRow>,
}

fn short_location(graph: &CodeGraph, id: NodeId) -> String {
    let n = graph.node(id);
    match graph.file_path(n.span.file()) {
        Some(p) if n.kind != NodeKind::External => {
            let base = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{base}:{}", n.span.start_line)
        }
        _ => "external".to_string(),
    }
}

fn flatten_into(n: &OutlineNode, parent: Option<usize>, rows: &mut Vec<Row>) {
    let idx = rows.len();
    rows.push(Row {
        id: n.id,
        depth: n.depth,
        label: n.label.clone(),
        is_scope: n.is_scope(),
        node: n.node.map(|id| id.0 as usize),
        functions: n.functions,
        entries: n.entries,
        entry: n.entry,
        cyclomatic: n.cyclomatic,
        parent,
    });
    for c in &n.children {
        flatten_into(c, Some(idx), rows);
    }
}

/// Rows whose every ancestor is expanded.
fn visible_rows(rows: &[Row], expanded: &HashSet<usize>) -> Vec<Row> {
    let mut vis = vec![false; rows.len()];
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let v = match r.parent {
            None => true,
            Some(p) => vis[p] && expanded.contains(&rows[p].id),
        };
        vis[i] = v;
        if v {
            out.push(r.clone());
        }
    }
    out
}

/// A path shortened to its last two components (`src-tauri/build.rs:1`), which
/// keeps the directory that disambiguates a file without the absolute prefix.
/// The full path stays available as a tooltip.
fn compact_location(location: &str) -> String {
    let (path, line) = match location.rsplit_once(':') {
        Some((p, l)) if l.chars().all(|c| c.is_ascii_digit()) && !l.is_empty() => (p, Some(l)),
        _ => (location, None),
    };
    let mut parts: Vec<&str> = path.rsplit(['/', '\\']).take(2).collect();
    parts.reverse();
    let short = parts.join("/");
    match line {
        Some(l) => format!("{short}:{l}"),
        None => short,
    }
}

fn build_explorer(graph: &CodeGraph) -> ExplorerData {
    let outline = outline(graph);
    let mut rows = Vec::with_capacity(outline.functions + outline.scopes);
    for root in &outline.roots {
        flatten_into(root, None, &mut rows);
    }
    let mut entries: Vec<EntryRow> = entry_points(graph)
        .into_iter()
        .map(|e| EntryRow {
            node: e.id.0 as usize,
            name: graph.node(e.id).qualified_name.clone(),
            kind: e.kind,
            location: short_location(graph, e.id),
            // Program entries and foreign-ABI exports always get their reach:
            // on a bare-metal or WASM codebase those *are* the starting points.
            reach: matches!(e.kind, EntryKind::Main | EntryKind::Exported)
                .then(|| reach_count(graph, e.id, Direction::Callees)),
        })
        .collect();
    // Rank the `main`s by how much code they drive, so the program's real
    // entry comes first and the "Main" button lands there. A repo usually has
    // several: a build script, a two-line wasm bootstrap, the actual binary.
    // Alphabetical order would offer `build::main` (reach 0) first.
    entries.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| b.reach.cmp(&a.reach))
            .then_with(|| a.name.cmp(&b.name))
    });
    ExplorerData {
        outline,
        rows,
        entries,
    }
}

// ---------------------------------------------------------------------------
// Navigation state (selection, flow, call tree, history)
// ---------------------------------------------------------------------------

/// One hop of a traced flow.
#[derive(Clone, Debug, PartialEq)]
struct Hop {
    node: usize,
    name: String,
}

/// A display-ready call tree (names resolved, so the view needs no graph).
#[derive(Clone, Debug, PartialEq)]
struct TreeItem {
    node: usize,
    name: String,
    location: String,
    external: bool,
    repeat: bool,
    truncated: usize,
    children: Vec<TreeItem>,
}

fn tree_item(graph: &CodeGraph, n: &CallTreeNode) -> TreeItem {
    let node = graph.node(n.id);
    TreeItem {
        node: n.id.0 as usize,
        name: node.name.clone(),
        location: short_location(graph, n.id),
        external: node.kind == NodeKind::External,
        repeat: n.repeat,
        truncated: n.truncated,
        children: n.children.iter().map(|c| tree_item(graph, c)).collect(),
    }
}

/// Reactive navigation state. All `Copy` signals, so it can be handed to
/// every component and closure freely.
#[derive(Clone, Copy)]
struct Nav {
    selected: RwSignal<Option<usize>>,
    card: RwSignal<Option<NodeCard>>,
    /// Flow source ("trace from here").
    anchor: RwSignal<Option<usize>>,
    anchor_name: RwSignal<String>,
    /// Traced path anchor .. selection (empty when none / unreachable).
    chain: RwSignal<Vec<Hop>>,
    tree: RwSignal<Option<TreeItem>>,
    /// Previously selected nodes, for "back".
    history: RwSignal<Vec<usize>>,
}

impl Nav {
    fn new() -> Self {
        Nav {
            selected: RwSignal::new(None),
            card: RwSignal::new(None),
            anchor: RwSignal::new(None),
            anchor_name: RwSignal::new(String::new()),
            chain: RwSignal::new(Vec::new()),
            tree: RwSignal::new(None),
            history: RwSignal::new(Vec::new()),
        }
    }

    fn reset(&self) {
        self.selected.set(None);
        self.card.set(None);
        self.anchor.set(None);
        self.anchor_name.set(String::new());
        self.chain.set(Vec::new());
        self.tree.set(None);
        self.history.set(Vec::new());
    }
}

/// The actions components can trigger. `Rc` closures because they capture the
/// (`!Send`) render state.
#[derive(Clone)]
struct Actions {
    /// Select node `idx`; `true` also moves the camera onto it.
    go: Rc<dyn Fn(usize, bool)>,
    /// Mark the selection as the flow source.
    trace: Rc<dyn Fn()>,
    clear_flow: Rc<dyn Fn()>,
    back: Rc<dyn Fn()>,
    /// Center the camera on the selection.
    locate: Rc<dyn Fn()>,
    clear: Rc<dyn Fn()>,
}

/// A `Copy + Send` handle to the actions. Leptos requires reactive closures
/// (`{move || ...}` blocks) to be `Send`, and `Actions` is not, so views hold
/// this thread-local-storage handle and fetch the closures when they run.
type ActionsHandle = StoredValue<Actions, LocalStorage>;

/// Select a node: query its card, call tree and (if an anchor is set) the
/// flow path, then recolor the scene and optionally move the camera.
fn select_node(
    state: &Shared,
    nav: Nav,
    labels: RwSignal<Vec<LabelBox>>,
    idx: usize,
    center: bool,
    push_history: bool,
) {
    // 1. Query the graph under a short borrow.
    let (card, tree, chain, pos) = {
        let s = state.borrow();
        let Some(graph) = s.graph.as_ref() else {
            return;
        };
        let id = NodeId(idx as u32);
        let Some(card) = node_card(graph, id) else {
            return;
        };
        let opts = CallTreeOptions {
            max_depth: TREE_DEPTH,
            max_children: TREE_WIDTH,
            ..Default::default()
        };
        let tree = tree_item(graph, &call_tree(graph, id, &opts));
        let chain: Vec<Hop> = match nav.anchor.get_untracked() {
            Some(a) if a != idx => shortest_path(graph, NodeId(a as u32), id, FLOW_MAX_DEPTH)
                .unwrap_or_default()
                .into_iter()
                .map(|h| Hop {
                    node: h.0 as usize,
                    name: graph.node(h).name.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        (card, tree, chain, s.positions.get(idx).copied())
    };

    // 2. History + reactive state.
    if push_history {
        if let Some(prev) = nav.selected.get_untracked() {
            if prev != idx {
                nav.history.update(|h| {
                    h.push(prev);
                    if h.len() > 64 {
                        h.remove(0);
                    }
                });
            }
        }
    }
    nav.selected.set(Some(idx));
    nav.card.set(Some(card));
    nav.tree.set(Some(tree));
    nav.chain.set(chain.clone());
    // The detail pane keeps its scroll position, so after clicking something
    // far down (a callee, a call-tree row) the new node's header would be
    // above the fold. Bring it back into view.
    scroll_side_to_top();

    // 3. Recolor the scene, move the camera if asked.
    {
        let mut s = state.borrow_mut();
        s.selected = Some(idx);
        s.path = chain.iter().map(|h| h.node).collect();
        if center {
            if let (Some(p), Some(viewer)) = (pos, s.viewer.as_mut()) {
                viewer.focus_on(p[0], p[1], FOCUS_ZOOM);
            }
        }
    }
    rebuild_and_refresh(state, labels);
}

fn clear_selection(state: &Shared, nav: Nav, labels: RwSignal<Vec<LabelBox>>) {
    nav.selected.set(None);
    nav.card.set(None);
    nav.tree.set(None);
    nav.chain.set(Vec::new());
    {
        let mut s = state.borrow_mut();
        s.selected = None;
        s.path.clear();
    }
    rebuild_and_refresh(state, labels);
}

fn make_actions(state: Shared, nav: Nav, labels: RwSignal<Vec<LabelBox>>) -> Actions {
    let go = {
        let state = state.clone();
        Rc::new(move |idx: usize, center: bool| {
            select_node(&state, nav, labels, idx, center, true);
        }) as Rc<dyn Fn(usize, bool)>
    };
    let trace = {
        let state = state.clone();
        Rc::new(move || {
            let Some(sel) = nav.selected.get_untracked() else {
                return;
            };
            let name = nav
                .card
                .with_untracked(|c| c.as_ref().map(|c| c.name.clone()))
                .unwrap_or_default();
            nav.anchor.set(Some(sel));
            nav.anchor_name.set(name);
            nav.chain.set(Vec::new());
            {
                let mut s = state.borrow_mut();
                s.anchor = Some(sel);
                s.path.clear();
            }
            rebuild_and_refresh(&state, labels);
        }) as Rc<dyn Fn()>
    };
    let clear_flow = {
        let state = state.clone();
        Rc::new(move || {
            nav.anchor.set(None);
            nav.anchor_name.set(String::new());
            nav.chain.set(Vec::new());
            {
                let mut s = state.borrow_mut();
                s.anchor = None;
                s.path.clear();
            }
            rebuild_and_refresh(&state, labels);
        }) as Rc<dyn Fn()>
    };
    let back = {
        let state = state.clone();
        Rc::new(move || {
            let prev = nav.history.try_update(|h| h.pop()).flatten();
            if let Some(idx) = prev {
                select_node(&state, nav, labels, idx, true, false);
            }
        }) as Rc<dyn Fn()>
    };
    let locate = {
        let state = state.clone();
        Rc::new(move || {
            let Some(sel) = nav.selected.get_untracked() else {
                return;
            };
            {
                let mut s = state.borrow_mut();
                let pos = s.positions.get(sel).copied();
                if let (Some(p), Some(viewer)) = (pos, s.viewer.as_mut()) {
                    viewer.focus_on(p[0], p[1], FOCUS_ZOOM);
                }
            }
            refresh_view(&state, labels);
        }) as Rc<dyn Fn()>
    };
    let clear = {
        let state = state.clone();
        Rc::new(move || clear_selection(&state, nav, labels)) as Rc<dyn Fn()>
    };
    Actions {
        go,
        trace,
        clear_flow,
        back,
        locate,
        clear,
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

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

    // Explorer + navigation state.
    let explorer = RwSignal::new(ExplorerData::default());
    let expanded = RwSignal::new(HashSet::<usize>::new());
    let tree_query = RwSignal::new(String::new());
    let nav = Nav::new();

    // Toolbar state for the render/UX features.
    let query = RwSignal::new(String::new());
    let show_labels = RwSignal::new(true);
    let bundle = RwSignal::new(false);
    // Projected labels for the DOM overlay; updated on every camera change.
    let labels = RwSignal::new(Vec::<LabelBox>::new());

    let actions: ActionsHandle = StoredValue::new_local(make_actions(state.clone(), nav, labels));

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
            wire_pointer_events(&canvas, state.clone(), nav, labels);
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
            let hosted = transport::has_tauri();
            if hosted && repo.trim().is_empty() {
                error.set(Some("Please enter a repository path.".into()));
                return;
            }
            let state = state.clone();
            let sem = semantic.get();
            busy.set(true);
            error.set(None);
            nav.reset();
            status.set(
                if hosted {
                    "Starting analysis…"
                } else {
                    "Browser dev mode: loading fixture…"
                }
                .into(),
            );
            spawn_local(async move {
                // Tauri hosts the engine; a plain browser gets a fixture (see
                // `transport::FixtureTransport`). Same trait, same UI code.
                let result = if hosted {
                    TauriTransport.analyze(repo, sem).await
                } else {
                    FixtureTransport.analyze(repo, sem).await
                };
                match result {
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
                        let ex = build_explorer(&graph);
                        // Crates start open so the top-level modules show.
                        let open: HashSet<usize> = ex
                            .rows
                            .iter()
                            .filter(|r| r.is_scope && r.depth == 0)
                            .map(|r| r.id)
                            .collect();
                        expanded.set(open);
                        explorer.set(ex);
                        let scene = {
                            let mut s = state.borrow_mut();
                            s.graph = Some(graph);
                            s.positions = view.positions.clone();
                            s.names = names;
                            s.selected = None;
                            s.anchor = None;
                            s.path.clear();
                            s.compose_scene()
                        };
                        state.borrow_mut().scene = scene;
                        match present_scene(&state).await {
                            Err(e) => error.set(Some(e)),
                            Ok(swapped) => {
                                // The renderer fell back to a replacement canvas
                                // (see `present_scene`): its listeners went with
                                // the old node, so wire the new one.
                                if let Some(fresh) = swapped {
                                    wire_pointer_events(&fresh, state.clone(), nav, labels);
                                }
                                refresh_view(&state, labels);
                                status.set(
                                    "Analysis complete — pick an entry point to start walking."
                                        .into(),
                                );
                            }
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

    // "Entry": jump to the best starting point — of the program entries and
    // foreign-ABI exports, whichever drives the most code (`reach` is computed
    // for exactly those). On a workspace full of build scripts and dev tools
    // that is the real binary; on a kernel it is the export the bootloader
    // jumps to, which is not a `main` at all.
    let on_main = move |_| {
        let main = explorer.with_untracked(|ex| {
            ex.entries
                .iter()
                .filter(|e| e.reach.is_some())
                .max_by_key(|e| e.reach)
                .or_else(|| ex.entries.first())
                .map(|e| e.node)
        });
        if let Some(idx) = main {
            actions.with_value(|a| (a.go)(idx, true));
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

    let has_graph = move || summary.get().is_some();

    view! {
        <div class="app">
            <header class="toolbar">
                <div class="brand">"Live Code Walk"</div>
                <input
                    class="path"
                    type="text"
                    placeholder=if transport::has_tauri() { "/path/to/repo" } else { "fixture.json (browser dev mode)" }
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
                <button on:click=on_main disabled=move || !has_graph() title="Jump to the program entry">"Entry"</button>
                <button on:click=on_fit disabled=move || !has_graph()>"Fit"</button>
                <input
                    class="search"
                    type="search"
                    placeholder="highlight nodes…"
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
                <ExplorerPane
                    explorer=explorer
                    expanded=expanded
                    tree_query=tree_query
                    nav=nav
                    actions=actions
                />
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
                    <DetailPane nav=nav actions=actions />
                    <Overview summary=summary vertical=vertical />
                    <Suggestions suggestions=suggestions />
                    <Diagnostics diagnostics=diagnostics />
                    <Legend />
                </aside>
            </div>

            <footer class="status">{move || status.get()}</footer>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Explorer sidebar
// ---------------------------------------------------------------------------

fn entry_badge(kind: Option<EntryKind>) -> Option<AnyView> {
    kind.map(|k| {
        let (class, text) = match k {
            EntryKind::Main => ("badge main", "main"),
            EntryKind::Exported => ("badge export", "export"),
            EntryKind::PublicRoot => ("badge pub", "pub"),
            EntryKind::Root => ("badge root", "root"),
            EntryKind::Test => ("badge test", "test"),
        };
        view! { <span class=class title=entry_title(k)>{text}</span> }.into_any()
    })
}

fn entry_title(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Main => "program entry: where execution starts",
        EntryKind::Exported => {
            "exported across a foreign ABI and called by nobody here: a bootloader, \
             hardware trap vector, WASM host or FFI caller enters through it"
        }
        EntryKind::PublicRoot => "public item nobody calls internally: an API surface or handler",
        EntryKind::Root => "private item with no callers: dynamic dispatch or dead code",
        EntryKind::Test => "test with no callers",
    }
}

fn entry_group_title(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Main => "main",
        EntryKind::Exported => "exported entries",
        EntryKind::PublicRoot => "public roots",
        EntryKind::Root => "private roots",
        EntryKind::Test => "tests",
    }
}

/// The entry-point list, grouped by kind.
fn entries_view(entries: &[EntryRow], nav: Nav, go: &Rc<dyn Fn(usize, bool)>) -> AnyView {
    if entries.is_empty() {
        return view! { <p class="hint">"No entry points yet — analyze a repository."</p> }
            .into_any();
    }
    let mut groups: Vec<AnyView> = Vec::new();
    for kind in [
        EntryKind::Main,
        EntryKind::Exported,
        EntryKind::PublicRoot,
        EntryKind::Root,
        EntryKind::Test,
    ] {
        let items: Vec<&EntryRow> = entries.iter().filter(|e| e.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        let total = items.len();
        let rows: Vec<AnyView> = items
            .iter()
            .take(ENTRIES_PER_KIND)
            .map(|e| {
                let idx = e.node;
                let go = go.clone();
                let name = e.name.clone();
                let location = e.location.clone();
                let reach = e.reach.map(|r| format!("reaches {r} fn"));
                view! {
                    <div
                        class=move || format!("row fn{}", if nav.selected.get() == Some(idx) { " selected" } else { "" })
                        title=location.clone()
                        on:click=move |_| go(idx, true)
                    >
                        <span class="label">{name}</span>
                        {reach.map(|r| view! { <span class="cc">{r}</span> })}
                    </div>
                }
                .into_any()
            })
            .collect();
        let more = (total > ENTRIES_PER_KIND).then(
            || view! { <div class="more">{format!("… {} more", total - ENTRIES_PER_KIND)}</div> },
        );
        groups.push(
            view! {
                <div class="group">
                    <span>{entry_group_title(kind)}</span>
                    <span class="count">{total}</span>
                </div>
                {rows}
                {more}
            }
            .into_any(),
        );
    }
    groups.into_any()
}

/// One outline row: a collapsible scope or a clickable function.
fn row_view(
    r: Row,
    expanded: RwSignal<HashSet<usize>>,
    nav: Nav,
    go: Rc<dyn Fn(usize, bool)>,
) -> AnyView {
    let indent = format!("padding-left:{}px", 6 + r.depth * 14);
    if r.is_scope {
        let id = r.id;
        let label = r.label;
        let functions = r.functions;
        let entries = r.entries;
        view! {
            <div
                class="row scope"
                style=indent
                on:click=move |_| {
                    expanded.update(|s| {
                        if !s.remove(&id) {
                            s.insert(id);
                        }
                    });
                }
            >
                <span class="twisty">{move || if expanded.with(|s| s.contains(&id)) { "▾" } else { "▸" }}</span>
                <span class="label">{label}</span>
                <span class="count">{functions}</span>
                {(entries > 0).then(|| view! { <span class="badge entry" title="entry points inside">{entries}</span> })}
            </div>
        }
        .into_any()
    } else {
        let idx = r.node.unwrap_or(0);
        let label = r.label;
        let cc = r.cyclomatic;
        let badge = entry_badge(r.entry);
        view! {
            <div
                class=move || format!("row fn{}", if nav.selected.get() == Some(idx) { " selected" } else { "" })
                style=indent
                on:click=move |_| go(idx, true)
            >
                <span class="label">{label}</span>
                {badge}
                <span class="cc" title="cyclomatic complexity">{format!("cc {cc}")}</span>
            </div>
        }
        .into_any()
    }
}

#[component]
fn ExplorerPane(
    explorer: RwSignal<ExplorerData>,
    expanded: RwSignal<HashSet<usize>>,
    tree_query: RwSignal<String>,
    nav: Nav,
    actions: ActionsHandle,
) -> impl IntoView {
    // Visible rows: the pruned tree while searching, else the expanded subset.
    let rows = Memo::new(move |_| {
        let q = tree_query.get();
        explorer.with(|ex| {
            if filter_is_active(&q) {
                let pruned = ex.outline.prune(&q);
                let mut rows = Vec::new();
                for r in &pruned.roots {
                    flatten_into(r, None, &mut rows);
                }
                rows
            } else {
                expanded.with(|open| visible_rows(&ex.rows, open))
            }
        })
    });
    let totals = move || {
        explorer.with(|ex| format!("{} fn · {} scopes", ex.outline.functions, ex.outline.scopes))
    };
    let expand_all = move |_| {
        let all: HashSet<usize> = explorer.with(|ex| {
            ex.rows
                .iter()
                .filter(|r| r.is_scope)
                .map(|r| r.id)
                .collect()
        });
        expanded.set(all);
    };
    let collapse_all = move |_| {
        let roots: HashSet<usize> = explorer.with(|ex| {
            ex.rows
                .iter()
                .filter(|r| r.is_scope && r.depth == 0)
                .map(|r| r.id)
                .collect()
        });
        expanded.set(roots);
    };

    view! {
        <aside class="explorer">
            <div class="card entries">
                <h3>"Start here"</h3>
                {move || {
                    let go = actions.with_value(|a| a.go.clone());
                    explorer.with(|ex| entries_view(&ex.entries, nav, &go))
                }}
            </div>
            <div class="card outline">
                <div class="outline-head">
                    <h3>"Outline"</h3>
                    <span class="hint">{totals}</span>
                </div>
                <div class="outline-tools">
                    <input
                        class="search"
                        type="search"
                        placeholder="filter functions…"
                        prop:value=move || tree_query.get()
                        on:input=move |ev| tree_query.set(target_value(&ev))
                    />
                    <button class="small" on:click=expand_all title="Expand all scopes">"+"</button>
                    <button class="small" on:click=collapse_all title="Collapse to crates">"−"</button>
                </div>
                <div class="tree">
                    {move || {
                        let go = actions.with_value(|a| a.go.clone());
                        rows.get()
                            .into_iter()
                            .map(|r| row_view(r, expanded, nav, go.clone()))
                            .collect::<Vec<_>>()
                    }}
                </div>
            </div>
        </aside>
    }
}

// ---------------------------------------------------------------------------
// Detail pane
// ---------------------------------------------------------------------------

fn call_tag(c: &CallRef) -> String {
    let mut s = edge_kind_str(c.kind).to_string();
    if c.count > 1 {
        s.push_str(&format!(" ×{}", c.count));
    }
    if c.call_site.start_line > 0 {
        s.push_str(&format!(" · L{}", c.call_site.start_line));
    }
    s
}

/// A clickable caller/callee list capped at `CALLS_SHOWN`.
fn calls_view(calls: &[CallRef], arrow: &'static str, go: &Rc<dyn Fn(usize, bool)>) -> AnyView {
    let total = calls.len();
    let rows: Vec<AnyView> = calls
        .iter()
        .take(CALLS_SHOWN)
        .map(|c| {
            let idx = c.id.0 as usize;
            let go = go.clone();
            let name = c.qualified_name.clone();
            let tag = call_tag(c);
            let ext = c.external;
            view! {
                <li class=if ext { "call ext" } else { "call" } on:click=move |_| go(idx, true)>
                    <span class="arrow">{arrow}</span>
                    <span class="name">{name}</span>
                    <span class="tag">{tag}</span>
                </li>
            }
            .into_any()
        })
        .collect();
    let more = (total > CALLS_SHOWN)
        .then(|| view! { <li class="more">{format!("… {} more", total - CALLS_SHOWN)}</li> });
    view! { <ul class="calls">{rows}{more}</ul> }.into_any()
}

/// Recursive call-tree rows.
fn tree_view(item: &TreeItem, go: &Rc<dyn Fn(usize, bool)>) -> AnyView {
    let idx = item.node;
    let go_click = go.clone();
    let name = item.name.clone();
    let location = item.location.clone();
    let class = match (item.external, item.repeat) {
        (true, _) => "call ext",
        (false, true) => "call repeat",
        _ => "call",
    };
    let repeat = item.repeat.then(
        || view! { <span class="tag" title="already expanded above (or a cycle)">"↺"</span> },
    );
    let children: Vec<AnyView> = item.children.iter().map(|c| tree_view(c, go)).collect();
    let more = (item.truncated > 0)
        .then(|| view! { <li class="more">{format!("… {} more", item.truncated)}</li> });
    let sub = (!children.is_empty() || item.truncated > 0)
        .then(|| view! { <ul class="calltree">{children}{more}</ul> });
    view! {
        <li class=class>
            <div class="tree-row" title=location on:click=move |_| go_click(idx, true)>
                <span class="arrow">"→"</span>
                <span class="name">{name}</span>
                {repeat}
            </div>
            {sub}
        </li>
    }
    .into_any()
}

#[component]
fn DetailPane(nav: Nav, actions: ActionsHandle) -> impl IntoView {
    move || {
        let a = actions.get_value();
        nav.card.get().map(|card| {
            let go = a.go.clone();
            let flags = (!card.flags.is_empty())
                .then(|| view! { <div class="flags">{card.flags.join(" · ")}</div> });
            let badge = entry_badge(card.entry);
            let inputs = calls_view(&card.inputs, "←", &go);
            let outputs = calls_view(&card.outputs, "→", &go);
            let m = card.metrics;
            let (fan_in, fan_out) = (card.fan_in, card.fan_out);
            let (n_in, n_out) = (card.inputs.len(), card.outputs.len());
            let full_location = card.location();
            let meta = format!("{} · {}", kind_str(card.kind), compact_location(&full_location));

            let trace = a.trace.clone();
            let back = a.back.clone();
            let locate = a.locate.clone();
            let clear = a.clear.clone();

            view! {
                <div class="card detail">
                    <div class="detail-head">
                        <h3>"Node"</h3>
                        <div class="actions">
                            <button class="small" on:click=move |_| back() disabled=move || nav.history.with(|h| h.is_empty()) title="Back to the previous node">"◀"</button>
                            <button class="small" on:click=move |_| locate() title="Center the view on this node">"⌖"</button>
                            <button class="small" on:click=move |_| clear() title="Clear selection">"✕"</button>
                        </div>
                    </div>
                    <div class="qname">{card.qualified_name.clone()}</div>
                    <div class="meta" title=full_location>{meta}{badge}</div>
                    {flags}
                    <ul class="stats compact">
                        <li><span>"complexity"</span><b>{m.cyclomatic}</b></li>
                        <li><span>"fan-in / out"</span><b>{format!("{fan_in} / {fan_out}")}</b></li>
                        <li><span>"LOC"</span><b>{m.lines_of_code}</b></li>
                        <li><span>"params"</span><b>{m.parameters}</b></li>
                        <li><span>"nesting"</span><b>{m.max_nesting}</b></li>
                        <li><span>"allocs"</span><b>{m.allocations}</b></li>
                    </ul>

                    <div class="flow">
                        <div class="flow-head">
                            <h4>"Flow"</h4>
                            <button class="small" on:click=move |_| trace() title="Use this node as the flow source; then pick a target">"trace from here"</button>
                            {move || nav.anchor.get().map(|_| {
                                let clear_flow = actions.with_value(|x| x.clear_flow.clone());
                                view! { <button class="small" on:click=move |_| clear_flow() title="Forget the flow source">"clear"</button> }
                            })}
                        </div>
                        {move || {
                            let anchor = nav.anchor.get();
                            let chain = nav.chain.get();
                            let go = actions.with_value(|x| x.go.clone());
                            match anchor {
                                None => view! { <p class="hint">"Set a source, then click any node (canvas, outline or a callee) to see the shortest call path between them."</p> }.into_any(),
                                Some(a) if nav.selected.get() == Some(a) => view! {
                                    <p class="hint">{format!("source: {} — now pick a target.", nav.anchor_name.get())}</p>
                                }.into_any(),
                                Some(_) if chain.is_empty() => view! {
                                    <p class="hint warn">{format!("no call path from {} to this node (in fast mode cross-crate method calls may be missing — try semantic).", nav.anchor_name.get())}</p>
                                }.into_any(),
                                Some(_) => {
                                    let hops = chain.len() - 1;
                                    let items: Vec<AnyView> = chain
                                        .into_iter()
                                        .enumerate()
                                        .map(|(i, h)| {
                                            let go = go.clone();
                                            let idx = h.node;
                                            view! {
                                                <span>
                                                    {(i > 0).then(|| view! { <span class="sep">" → "</span> })}
                                                    <span class="hop" on:click=move |_| go(idx, true)>{h.name}</span>
                                                </span>
                                            }.into_any()
                                        })
                                        .collect();
                                    view! {
                                        <div class="chain">
                                            <div class="hint">{format!("{hops} hop(s)")}</div>
                                            {items}
                                        </div>
                                    }.into_any()
                                }
                            }
                        }}
                    </div>

                    <h4>{format!("Inputs — {n_in} caller(s)")}</h4>
                    {inputs}
                    <h4>{format!("Outputs — {n_out} callee(s)")}</h4>
                    {outputs}

                    <h4>"Calls below"</h4>
                    {move || {
                        let go = actions.with_value(|x| x.go.clone());
                        nav.tree.get().map(|t| {
                            if t.children.is_empty() && t.truncated == 0 {
                                view! { <p class="hint">"Leaf: calls nothing in the analyzed code."</p> }.into_any()
                            } else {
                                let rows: Vec<AnyView> = t.children.iter().map(|c| tree_view(c, &go)).collect();
                                let more = (t.truncated > 0)
                                    .then(|| view! { <li class="more">{format!("… {} more", t.truncated)}</li> });
                                view! { <ul class="calltree root">{rows}{more}</ul> }.into_any()
                            }
                        })
                    }}
                </div>
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Report cards
// ---------------------------------------------------------------------------

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
fn Legend() -> impl IntoView {
    view! {
        <div class="card legend">
            <h3>"Legend"</h3>
            <ul>
                <li><i class="dot fn"></i>"function"</li>
                <li><i class="dot method"></i>"method"</li>
                <li><i class="dot closure"></i>"closure"</li>
                <li><i class="dot external"></i>"external / unresolved"</li>
                <li><i class="dot sel"></i>"selected · "<i class="dot anchor"></i>"flow source · "<i class="dot path"></i>"on path"</li>
            </ul>
            <p class="hint">"Size ∝ fan-in · warmer ∝ complexity"</p>
            <p class="hint">"Drag to pan · scroll to zoom · click a node"</p>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Canvas plumbing
// ---------------------------------------------------------------------------

/// Create the viewer on first use, otherwise swap the scene; then draw. Care is
/// taken never to hold the `RefCell` borrow across the async device request.
async fn present_scene(state: &Shared) -> Result<Option<HtmlCanvasElement>, String> {
    let (need_viewer, canvas, scene) = {
        let s = state.borrow();
        (s.viewer.is_none(), s.canvas.clone(), s.scene.clone())
    };
    let Some(scene) = scene else {
        return Ok(None);
    };

    let mut swapped = None;
    if need_viewer {
        let canvas = canvas.ok_or_else(|| "canvas is not ready yet".to_string())?;
        resize_canvas(&canvas);
        let viewer = WebViewer::new(canvas.clone(), &scene).await?;
        // A WebGPU attempt that fails leaves its canvas unusable for the WebGL2
        // retry, so the viewer may have swapped in a replacement element. The
        // old node (and its listeners) is gone from the document; hand the new
        // one back so the caller can re-wire it.
        let used = viewer.canvas();
        if !canvas.is_same_node(Some(&used)) {
            resize_canvas(&used);
            swapped = Some(used.clone());
            state.borrow_mut().canvas = Some(used);
        }
        state.borrow_mut().viewer = Some(viewer);
    } else if let Some(viewer) = state.borrow_mut().viewer.as_mut() {
        viewer.set_scene(&scene);
    }

    if let Some(viewer) = state.borrow_mut().viewer.as_mut() {
        viewer.render();
    }
    Ok(swapped)
}

/// Scroll the right-hand pane back to the top (see `select_node`).
fn scroll_side_to_top() {
    if let Some(side) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(".side").ok().flatten())
    {
        side.set_scroll_top(0);
    }
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
    nav: Nav,
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

    // mouseup: a click (no drag) selects the node under the cursor, or clears
    // the selection when the background is clicked.
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: MouseEvent| {
            let x = ev.offset_x() as f32;
            let y = ev.offset_y() as f32;
            let hit = {
                let mut s = state.borrow_mut();
                let is_click = s.drag.is_some() && !s.moved;
                s.drag = None;
                if !is_click {
                    return;
                }
                match (s.viewer.as_ref(), s.scene.as_ref()) {
                    (Some(viewer), Some(scene)) => {
                        let world = viewer.screen_to_world(x, y);
                        hit_test(scene, world)
                    }
                    _ => None,
                }
            };
            match hit {
                Some(idx) => select_node(&state, nav, labels, idx, false, true),
                None => clear_selection(&state, nav, labels),
            }
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

/// Rebuild the display scene from the toggles (bundling / search / selection),
/// swap it into the viewer *without* moving the camera, then refresh overlays.
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
