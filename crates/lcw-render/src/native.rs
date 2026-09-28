//! Native winit + wgpu driver: opens a window and renders the graph with
//! pan (drag), zoom (wheel), and click-to-pick. This is the desktop
//! validation path / "power mode"; the same [`Renderer`] later runs in the
//! Tauri webview via wasm.

use std::sync::Arc;

use glam::Vec2;
use lcw_core::{CodeGraph, NodeKind};
use lcw_query::{node_card, CallRef, Connection, NodeCard};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::camera::Camera2D;
use crate::highlight::highlight;
use crate::hud;
use crate::interact::{self, Selection};
use crate::scene::{self, SceneData};
use crate::{RenderError, Renderer};

/// Per-node detail for the interactive panel, indexed by pick id (which, for
/// the function-level call view, equals `NodeId.0`). A thin display
/// projection of [`lcw_query::NodeCard`], built once by [`build_node_infos`];
/// traversal (flow tracing, entry points) goes through `lcw-query` directly.
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    /// Fully qualified name (also the pick label).
    pub title: String,
    /// One-line summary: `file:line  kind  cc N  params N  [entry]`.
    pub subtitle: String,
    /// Source file for jump-to-code (empty for external nodes).
    pub file: String,
    pub line: u32,
    /// Caller lines (fan-in), each with its edge kind / multiplicity.
    pub inputs: Vec<String>,
    /// Callee lines (fan-out; externals tagged).
    pub outputs: Vec<String>,
}

fn kind_short(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "fn",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

fn short_name(qualified: &str) -> &str {
    qualified.rsplit("::").next().unwrap_or(qualified)
}

/// One panel line for a caller/callee: `name  (ext)  [kind x2]`.
fn call_line(c: &CallRef) -> String {
    let mut s = c.qualified_name.clone();
    if c.external {
        s.push_str("  (ext)");
    }
    s.push_str("  [");
    s.push_str(lcw_query::edge_kind_str(c.kind));
    if c.count > 1 {
        s.push_str(&format!(" x{}", c.count));
    }
    s.push(']');
    s
}

fn info_from_card(card: &NodeCard) -> NodeInfo {
    let mut subtitle = format!(
        "{}  {}  cc {}  params {}",
        if card.file.is_empty() {
            "<external>".to_string()
        } else {
            card.location_short()
        },
        kind_short(card.kind),
        card.metrics.cyclomatic,
        card.metrics.parameters
    );
    if let Some(entry) = card.entry {
        subtitle.push_str(&format!("  [{entry}]"));
    }
    NodeInfo {
        title: card.qualified_name.clone(),
        subtitle,
        file: card.file.clone(),
        line: card.line,
        inputs: card.inputs.iter().map(call_line).collect(),
        outputs: card.outputs.iter().map(call_line).collect(),
    }
}

/// Build per-node [`NodeInfo`] in pick-id order (== `NodeId.0` for the
/// function-level call view built by [`scene::build`]).
pub fn build_node_infos(graph: &CodeGraph) -> Vec<NodeInfo> {
    graph
        .node_ids()
        .filter_map(|id| node_card(graph, id))
        .map(|card| info_from_card(&card))
        .collect()
}

/// Build the detail panel for node `idx`, optionally annotating an active flow
/// `path` (pick ids) that ends at it.
pub fn panel_for(infos: &[NodeInfo], idx: usize, path: &[usize]) -> hud::Panel {
    let Some(info) = infos.get(idx) else {
        return hud::Panel {
            title: format!("node #{idx}"),
            ..Default::default()
        };
    };
    let mut sections = Vec::new();
    if path.len() > 1 {
        let chain: Vec<&str> = path
            .iter()
            .map(|&i| infos.get(i).map(|x| short_name(&x.title)).unwrap_or("?"))
            .collect();
        sections.push(hud::Section {
            heading: format!("flow — {} hop(s)", path.len() - 1),
            items: vec![chain.join(" -> ")],
            accent: [0.96, 0.86, 0.36, 1.0],
        });
    }
    sections.push(hud::Section {
        heading: format!("inputs — {} caller(s)", info.inputs.len()),
        items: capped(&info.inputs, 12),
        accent: [0.46, 0.74, 1.0, 1.0],
    });
    sections.push(hud::Section {
        heading: format!("outputs — {} callee(s)", info.outputs.len()),
        items: capped(&info.outputs, 12),
        accent: [0.46, 0.90, 0.56, 1.0],
    });
    hud::Panel {
        title: info.title.clone(),
        subtitle: info.subtitle.clone(),
        sections,
        footer: vec![
            "f: flow from here   o: open code   m: jump to main".into(),
            "click: inspect   esc: clear".into(),
        ],
    }
}

fn capped(items: &[String], max: usize) -> Vec<String> {
    if items.len() <= max {
        return items.to_vec();
    }
    let mut out: Vec<String> = items[..max].to_vec();
    out.push(format!("... {} more", items.len() - max));
    out
}

/// Open a window and render `graph` using the given per-node `positions`
/// (indexed by `NodeId.0`), with `groups` drawn as boxes behind the nodes
/// (empty for a free layout). Blocks until the window is closed.
pub fn run(
    graph: &CodeGraph,
    positions: &[[f32; 2]],
    groups: &[lcw_core::GroupBox],
) -> Result<(), RenderError> {
    let mut scene = scene::build(graph, positions);
    crate::groups::draw_groups(&mut scene, groups, true);
    let infos = build_node_infos(graph);
    let labels: Vec<String> = infos.iter().map(|i| i.title.clone()).collect();
    run_app("Live Code Walk", scene, labels, infos, Some(graph.clone()))
}

/// Open a window on a prebuilt [`SceneData`] with per-node pick `labels`
/// (indexed by pick id - 1). Used by the module view, which supplies its own
/// aggregated scene rather than one function node per graph node. Blocks until
/// the window is closed.
pub fn run_scene(scene: SceneData, labels: Vec<String>) -> Result<(), RenderError> {
    run_app("Live Code Walk", scene, labels, Vec::new(), None)
}

fn run_app(
    title: &str,
    scene: SceneData,
    labels: Vec<String>,
    infos: Vec<NodeInfo>,
    graph: Option<CodeGraph>,
) -> Result<(), RenderError> {
    let event_loop = EventLoop::new().map_err(|e| RenderError::Window(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(title, scene, labels, infos, graph);
    event_loop
        .run_app(&mut app)
        .map_err(|e| RenderError::Window(e.to_string()))
}

/// Render `graph` (with the given per-node `positions`) to an RGBA PNG at
/// `path`, headless — no window, no event loop. The whole graph is framed to
/// fit `width`x`height`. Useful for thumbnails, docs, CI artifacts, and sharing
/// a static view of the same `wgpu` scene the interactive window shows.
pub fn render_to_png(
    graph: &CodeGraph,
    positions: &[[f32; 2]],
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> Result<(), RenderError> {
    let scene = scene::build(graph, positions);
    render_to_png_scene(&scene, width, height, path)
}

/// Render a prebuilt [`SceneData`] (e.g. the module view) to a PNG, headless.
pub fn render_to_png_scene(
    scene: &SceneData,
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> Result<(), RenderError> {
    render_to_png_scene_with_hud(scene, &[], width, height, path)
}

/// Like [`render_to_png_scene`], but also draws a screen-space HUD overlay
/// (e.g. a node detail panel from [`hud::build`]). Lets tools produce an
/// annotated screenshot of the same panel the live window shows.
pub fn render_to_png_scene_with_hud(
    scene: &SceneData,
    hud_verts: &[crate::scene::EdgeVertex],
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> Result<(), RenderError> {
    let width = width.max(1);
    let height = height.max(1);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|_| RenderError::NoAdapter)?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| RenderError::Screenshot(e.to_string()))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(&device, format, scene);
    let mut camera = Camera2D {
        viewport: Vec2::new(width as f32, height as f32),
        ..Default::default()
    };
    camera.fit(scene.min.into(), scene.max.into());
    renderer.update_camera(&queue, &camera);
    if !hud_verts.is_empty() {
        renderer.set_hud(&device, hud_verts);
        renderer.update_hud_projection(&queue, width, height);
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lcw screenshot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    // `copy_texture_to_buffer` requires each row to be a multiple of 256 bytes.
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lcw screenshot readback"),
        size: (padded_bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lcw screenshot"),
    });
    renderer.render(&mut encoder, &view);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;

    // Drop the row padding so the PNG rows are tightly packed.
    let mut rgba = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height {
        let start = (row * padded_bytes_per_row) as usize;
        rgba.extend_from_slice(&data[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(data);
    readback.unmap();

    let file = std::fs::File::create(path).map_err(|e| RenderError::Screenshot(e.to_string()))?;
    let writer = std::io::BufWriter::new(file);
    let mut png = png::Encoder::new(writer, width, height);
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    let mut png = png
        .write_header()
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    png.write_image_data(&rgba)
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    Ok(())
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

struct App {
    title: String,
    scene: SceneData,
    labels: Vec<String>,
    infos: Vec<NodeInfo>,
    /// The call graph behind the scene (function-level view only); pick ids
    /// equal `NodeId.0`, so `lcw-query` traversals apply directly.
    graph: Option<CodeGraph>,
    camera: Camera2D,
    gfx: Option<Gfx>,
    cursor: Vec2,
    press_origin: Option<Vec2>,
    dragging: bool,
    /// Whether the current press has travelled far enough to be a pan. Until
    /// it has, the camera stays put and a release is a click.
    panning: bool,
    /// Selection, flow source and how they connect: the same state machine the
    /// web UI runs (see [`crate::interact`]), which is what the tests cover.
    sel: Selection,
}

impl App {
    fn new(
        title: &str,
        scene: SceneData,
        labels: Vec<String>,
        infos: Vec<NodeInfo>,
        graph: Option<CodeGraph>,
    ) -> Self {
        App {
            title: title.to_string(),
            scene,
            labels,
            infos,
            graph,
            camera: Camera2D::default(),
            gfx: None,
            cursor: Vec2::ZERO,
            press_origin: None,
            dragging: false,
            panning: false,
            sel: Selection::default(),
        }
    }

    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RenderError> {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title(&self.title))
                .map_err(|e| RenderError::Window(e.to_string()))?,
        );
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // The GL backend needs the display connection to present (notably on
        // Wayland); the others ignore it.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| RenderError::Window(e.to_string()))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|_| RenderError::NoAdapter)?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| RenderError::Window(e.to_string()))?;

        let mut config = surface
            .get_default_config(&adapter, w, h)
            .ok_or(RenderError::NoAdapter)?;
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        surface.configure(&device, &config);

        let renderer = Renderer::new(&device, config.format, &self.scene);

        self.camera.viewport = Vec2::new(w as f32, h as f32);
        self.camera
            .fit(self.scene.min.into(), self.scene.max.into());

        window.request_redraw();
        self.gfx = Some(Gfx {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
        });
        Ok(())
    }

    fn resize(&mut self, w: u32, h: u32) {
        if let Some(gfx) = &mut self.gfx {
            if w > 0 && h > 0 {
                gfx.config.width = w;
                gfx.config.height = h;
                gfx.surface.configure(&gfx.device, &gfx.config);
                self.camera.viewport = Vec2::new(w as f32, h as f32);
                gfx.window.request_redraw();
            }
        }
    }

    fn redraw(&mut self) {
        let Some(gfx) = &mut self.gfx else { return };
        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                gfx.surface.configure(&gfx.device, &gfx.config);
                return;
            }
            // Timed out, occluded or invalid: skip this frame.
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        gfx.renderer.update_camera(&gfx.queue, &self.camera);
        gfx.renderer
            .update_hud_projection(&gfx.queue, gfx.config.width, gfx.config.height);
        let mut encoder = gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lcw frame"),
            });
        gfx.renderer.render(&mut encoder, &view);
        gfx.queue.submit(std::iter::once(encoder.finish()));
        gfx.queue.present(frame);
    }

    /// Window pixels per logical pixel: 2 on a Retina display. Cursor
    /// positions and the camera viewport are physical, so every threshold
    /// expressed in logical pixels is scaled by this.
    fn scale_factor(&self) -> f32 {
        self.gfx
            .as_ref()
            .map_or(1.0, |g| g.window.scale_factor() as f32)
    }

    /// Resolve a click at `screen` to a node on the CPU, with a slop measured
    /// in screen pixels (see [`interact::pick_node`]). The GPU pick buffer
    /// only answered for the exact pixel under the cursor, so any click that
    /// grazed a small node was a miss.
    fn pick_at(&mut self, screen: Vec2) {
        let world = self.camera.screen_to_world(screen);
        let slop = interact::pick_slop_world(self.camera.zoom, self.scale_factor());
        match interact::pick_node(&self.scene, world.into(), slop) {
            Some(idx) => self.select(idx),
            None => {
                if self.sel.miss() {
                    self.refresh_selection();
                }
            }
        }
    }

    /// Select a node — tracing a flow to it when a source is set — then
    /// redraw the panel and highlight.
    fn select(&mut self, idx: usize) {
        self.sel.select(self.graph.as_ref(), idx);
        let label = self
            .labels
            .get(idx)
            .map(String::as_str)
            .unwrap_or("<unknown>");
        match &self.sel.connection {
            Some(Connection::Unconnected) => {
                lcw_telemetry::info!(target: "lcw::render", node = idx, %label, "no call path to the flow source")
            }
            Some(c) if !c.path().is_empty() => {
                lcw_telemetry::info!(target: "lcw::render", node = idx, %label, hops = c.path().len() - 1, "flow traced")
            }
            _ => lcw_telemetry::info!(target: "lcw::render", node = idx, %label, "picked node"),
        }
        self.refresh_selection();
    }

    fn refresh_selection(&mut self) {
        self.rebuild_hud();
        self.recolor();
        self.request_redraw();
    }

    fn clear_selection(&mut self) {
        self.sel.clear();
        self.refresh_selection();
    }

    /// `f`: make the selection the flow source; the next pick traces how the
    /// two connect, in whichever direction a call path exists.
    fn mark_flow_anchor(&mut self) {
        if self.sel.anchor_here() {
            lcw_telemetry::info!(target: "lcw::render", node = ?self.sel.anchor, "flow source set");
            self.refresh_selection();
        } else {
            lcw_telemetry::info!(target: "lcw::render", "select a node first, then press f");
        }
    }

    /// Open the selected node's source location in an editor.
    fn open_selected(&self) {
        let Some(sel) = self.sel.selected else { return };
        let Some(info) = self.infos.get(sel) else {
            return;
        };
        if info.file.is_empty() {
            lcw_telemetry::info!(target: "lcw::render", "no source location for selection");
            return;
        }
        open_in_editor(&info.file, info.line);
    }

    /// Select the best place to start reading and center the camera on it: of
    /// the program entries and foreign-ABI exports, the one that drives the
    /// most code (see [`lcw_query::primary_entry`]).
    fn jump_to_main(&mut self) {
        let Some(graph) = &self.graph else {
            lcw_telemetry::info!(target: "lcw::render", "no call graph in this view; cannot jump to the entry point");
            return;
        };
        let Some(main) = lcw_query::primary_entry(graph) else {
            lcw_telemetry::info!(target: "lcw::render", "no entry point found in the graph");
            return;
        };
        let idx = main.0 as usize;
        self.focus(idx);
        self.select(idx);
    }

    /// Center the camera on a node, zooming in if the view is too far out to
    /// tell nodes apart.
    fn focus(&mut self, idx: usize) {
        if let Some(inst) = self.scene.nodes.get(idx) {
            self.camera.center = Vec2::from(inst.center);
            if self.camera.zoom < 3.0 {
                self.camera.zoom = 3.0;
            }
        }
    }

    fn rebuild_hud(&mut self) {
        let Some(sel) = self.sel.selected else {
            if let Some(gfx) = &mut self.gfx {
                gfx.renderer.set_hud(&gfx.device, &[]);
            }
            return;
        };
        let path = self.sel.path();
        let panel = panel_for(&self.infos, sel, &path);
        // Every flow state gets a line, including the two that used to show
        // nothing at all: "source set, pick a target" and "no path".
        let infos = &self.infos;
        let crumb = self.sel.flow_status(|i| {
            infos
                .get(i)
                .map_or("?", |x| short_name(&x.title))
                .to_string()
        });
        let Some(gfx) = &mut self.gfx else { return };
        let (w, h) = (gfx.config.width, gfx.config.height);
        let mut verts = hud::build(w, h, &panel);
        if let Some(text) = crumb {
            verts.extend(hud::breadcrumb(w, h, &text));
        }
        gfx.renderer.set_hud(&gfx.device, &verts);
    }

    /// Recolor nodes and edges to focus the current selection / traced path.
    fn recolor(&mut self) {
        let path = self.sel.path();
        let Some(gfx) = &mut self.gfx else { return };
        if self.sel.selected.is_none() && path.is_empty() {
            gfx.renderer.set_nodes(&gfx.device, &self.scene.nodes);
            gfx.renderer.set_edges(&gfx.device, &self.scene.edges);
            return;
        }
        let (nodes, edges) = highlight(&self.scene, self.sel.selected, self.sel.anchor, &path);
        gfx.renderer.set_nodes(&gfx.device, &nodes);
        gfx.renderer.set_edges(&gfx.device, &edges);
    }

    fn request_redraw(&self) {
        if let Some(gfx) = &self.gfx {
            gfx.window.request_redraw();
        }
    }
}

/// Best-effort "jump to code": VS Code if present, else `$VISUAL`/`$EDITOR`,
/// else the OS opener. Spawns detached; failures are logged, not fatal.
fn open_in_editor(file: &str, line: u32) {
    use std::process::Command;
    let goto = format!("{file}:{line}");
    if Command::new("code").arg("-g").arg(&goto).spawn().is_ok() {
        lcw_telemetry::info!(target: "lcw::render", %goto, "opened in VS Code");
        return;
    }
    if let Ok(editor) = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")) {
        if Command::new(&editor).arg(file).spawn().is_ok() {
            lcw_telemetry::info!(target: "lcw::render", %editor, %file, "opened in $EDITOR");
            return;
        }
    }
    match os_opener(file).spawn() {
        Ok(_) => lcw_telemetry::info!(target: "lcw::render", %file, "opened via OS opener"),
        Err(e) => lcw_telemetry::warn!(target: "lcw::render", error = %e, "could not open editor"),
    }
}

/// The desktop's "open this with whatever handles it" command, per platform.
///
/// Windows has no such program: `start` is a builtin of `cmd`, so it has to be
/// invoked through the shell, and its first argument is the console *title* —
/// omit the empty string and a path containing spaces is taken for the title
/// and nothing opens. This used to fall through to `xdg-open` off macOS, which
/// on Windows is a program that does not exist, so the viewer's open-in-editor
/// key did nothing for anyone without VS Code on PATH.
fn os_opener(file: &str) -> std::process::Command {
    use std::process::Command;
    if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(file);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(file);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(file);
        c
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        if let Err(e) = self.init(event_loop) {
            lcw_telemetry::error!(target: "lcw::render", error = %e, "failed to initialize renderer");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize(size.width, size.height),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                let PhysicalPosition { x, y } = position;
                let new = Vec2::new(x as f32, y as f32);
                if self.dragging {
                    let delta = if self.panning {
                        Some(new - self.cursor)
                    } else {
                        match self.press_origin {
                            Some(origin)
                                if !interact::is_click(
                                    origin.into(),
                                    new.into(),
                                    self.scale_factor(),
                                ) =>
                            {
                                // Past the click slop: this is a pan. Catch up
                                // on the travel held back so far, so the graph
                                // stays under the cursor.
                                self.panning = true;
                                Some(new - origin)
                            }
                            _ => None,
                        }
                    };
                    if let Some(delta) = delta {
                        self.camera.pan_pixels(delta);
                        self.request_redraw();
                    }
                }
                self.cursor = new;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            self.dragging = true;
                            self.panning = false;
                            self.press_origin = Some(self.cursor);
                        }
                        ElementState::Released => {
                            self.dragging = false;
                            if !self.panning {
                                // Pick where the press landed: that is where
                                // the user aimed, and the camera has not moved.
                                let at = self.press_origin.unwrap_or(self.cursor);
                                self.pick_at(at);
                            }
                            self.panning = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 60.0,
                };
                let factor = (1.0 + scroll * 0.1).clamp(0.5, 2.0);
                self.camera.zoom_at(self.cursor, factor);
                if let Some(gfx) = &self.gfx {
                    gfx.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Character(c) if c.eq_ignore_ascii_case("f") => self.mark_flow_anchor(),
                Key::Character(c) if c.eq_ignore_ascii_case("m") => self.jump_to_main(),
                Key::Character(c) if c.eq_ignore_ascii_case("o") => self.open_selected(),
                Key::Named(NamedKey::Enter) => self.open_selected(),
                Key::Named(NamedKey::Escape) => self.clear_selection(),
                _ => {}
            },
            _ => {}
        }
    }
}
