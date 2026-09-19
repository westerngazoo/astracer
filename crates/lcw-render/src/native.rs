//! Native winit + wgpu driver: opens a window and renders the graph with
//! pan (drag), zoom (wheel), and click-to-pick. This is the desktop
//! validation path / "power mode"; the same [`Renderer`] later runs in the
//! Tauri webview via wasm.

use std::collections::VecDeque;
use std::sync::Arc;

use glam::Vec2;
use lcw_core::{CodeGraph, NodeKind};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::camera::Camera2D;
use crate::hud;
use crate::scene::{self, SceneData};
use crate::{RenderError, Renderer};

/// Per-node detail + adjacency for the interactive panel and flow tracing,
/// indexed by pick id (which, for the function-level call view, equals
/// `NodeId.0`). Built once from the [`CodeGraph`] by [`build_node_infos`].
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    /// Fully qualified name (also the pick label).
    pub title: String,
    /// One-line summary: `file:line  kind  cc N  params N`.
    pub subtitle: String,
    /// Source file for jump-to-code (empty for external nodes).
    pub file: String,
    pub line: u32,
    /// Caller qualified names (fan-in).
    pub inputs: Vec<String>,
    /// Callee display names (fan-out; externals tagged).
    pub outputs: Vec<String>,
    /// Successor pick ids, for tracing a flow to another node.
    pub succ: Vec<u32>,
}

fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "fn",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn short_name(qualified: &str) -> &str {
    qualified.rsplit("::").next().unwrap_or(qualified)
}

/// Build per-node [`NodeInfo`] in pick-id order (== `NodeId.0` for the
/// function-level call view built by [`scene::build`]).
pub fn build_node_infos(graph: &CodeGraph) -> Vec<NodeInfo> {
    let mut infos = Vec::with_capacity(graph.node_count());
    for id in graph.node_ids() {
        let node = graph.node(id);
        let file = graph
            .file_path(node.span.file())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let subtitle = format!(
            "{}:{}  {}  cc {}  params {}",
            base_name(&file),
            node.span.start_line,
            kind_str(node.kind),
            node.cyclomatic_complexity(),
            node.stats.parameters
        );
        let mut inputs: Vec<String> = graph
            .neighbors_in(id)
            .map(|c| graph.node(c).qualified_name.clone())
            .collect();
        inputs.sort();
        let mut outputs: Vec<String> = graph
            .neighbors_out(id)
            .map(|c| {
                let cn = graph.node(c);
                if cn.kind == NodeKind::External {
                    format!("{}  (ext)", cn.qualified_name)
                } else {
                    cn.qualified_name.clone()
                }
            })
            .collect();
        outputs.sort();
        let succ: Vec<u32> = graph.neighbors_out(id).map(|c| c.0).collect();
        infos.push(NodeInfo {
            title: node.qualified_name.clone(),
            subtitle,
            file,
            line: node.span.start_line,
            inputs,
            outputs,
            succ,
        });
    }
    infos
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
            "f: flow from here   o: open code".into(),
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

/// Produce recolored node instances and edge vertices that focus a selection
/// and/or a traced `path` (pick ids): bright selection, orange flow anchor,
/// blue path nodes, amber path edges (or the selection's incident edges), and
/// everything else dimmed. Pure — reused by the live viewer and by headless
/// annotated screenshots.
pub fn highlight(
    scene: &SceneData,
    selected: Option<usize>,
    anchor: Option<usize>,
    path: &[usize],
) -> (
    Vec<crate::scene::NodeInstance>,
    Vec<crate::scene::EdgeVertex>,
) {
    let sel = selected.map(|i| i as u32);
    let anchor = anchor.map(|i| i as u32);
    let on_path: std::collections::HashSet<u32> = path.iter().map(|&i| i as u32).collect();

    let mut nodes = scene.nodes.clone();
    for (i, inst) in nodes.iter_mut().enumerate() {
        let i = i as u32;
        if Some(i) == sel {
            inst.color = [1.0, 0.95, 0.5, 1.0];
            inst.radius *= 1.4;
        } else if Some(i) == anchor {
            inst.color = [1.0, 0.58, 0.30, 1.0];
            inst.radius *= 1.25;
        } else if on_path.contains(&i) {
            inst.color = [0.40, 0.85, 1.0, 1.0];
            inst.radius *= 1.2;
        } else {
            inst.color[3] *= 0.16;
        }
    }

    let path_pairs: std::collections::HashSet<(u32, u32)> = path
        .windows(2)
        .map(|w| (w[0] as u32, w[1] as u32))
        .collect();
    let mut edges = scene.edges.clone();
    for (i, seg) in scene.edge_nodes.iter().enumerate() {
        let [a, b] = *seg;
        let vi = i * 2;
        let color = if path_pairs.contains(&(a, b)) {
            [0.98, 0.80, 0.30, 0.95]
        } else if path_pairs.is_empty() && (sel == Some(a) || sel == Some(b)) {
            let mut c = edges[vi].color;
            c[3] = 0.9;
            c
        } else {
            let mut c = edges[vi].color;
            c[3] *= if path_pairs.is_empty() { 0.10 } else { 0.05 };
            c
        };
        edges[vi].color = color;
        edges[vi + 1].color = color;
    }
    (nodes, edges)
}

/// Open a window and render `graph` using the given per-node `positions`
/// (indexed by `NodeId.0`). Blocks until the window is closed.
pub fn run(graph: &CodeGraph, positions: &[[f32; 2]]) -> Result<(), RenderError> {
    let scene = scene::build(graph, positions);
    let infos = build_node_infos(graph);
    let labels: Vec<String> = infos.iter().map(|i| i.title.clone()).collect();
    run_app("Live Code Walk", scene, labels, infos)
}

/// Open a window on a prebuilt [`SceneData`] with per-node pick `labels`
/// (indexed by pick id - 1). Used by the module view, which supplies its own
/// aggregated scene rather than one function node per graph node. Blocks until
/// the window is closed.
pub fn run_scene(scene: SceneData, labels: Vec<String>) -> Result<(), RenderError> {
    run_app("Live Code Walk", scene, labels, Vec::new())
}

fn run_app(
    title: &str,
    scene: SceneData,
    labels: Vec<String>,
    infos: Vec<NodeInfo>,
) -> Result<(), RenderError> {
    let event_loop = EventLoop::new().map_err(|e| RenderError::Window(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(title, scene, labels, infos);
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

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or(RenderError::NoAdapter)?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
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
    let _ = device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();

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
    camera: Camera2D,
    gfx: Option<Gfx>,
    cursor: Vec2,
    press_origin: Option<Vec2>,
    dragging: bool,
    moved_while_pressed: bool,
    /// Currently inspected node (pick id).
    selected: Option<usize>,
    /// Node marked as the start of a flow trace (`f` on a selection).
    flow_anchor: Option<usize>,
    /// Pick ids of the currently highlighted flow path (source .. target).
    path: Vec<usize>,
}

impl App {
    fn new(title: &str, scene: SceneData, labels: Vec<String>, infos: Vec<NodeInfo>) -> Self {
        App {
            title: title.to_string(),
            scene,
            labels,
            infos,
            camera: Camera2D::default(),
            gfx: None,
            cursor: Vec2::ZERO,
            press_origin: None,
            dragging: false,
            moved_while_pressed: false,
            selected: None,
            flow_anchor: None,
            path: Vec::new(),
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

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| RenderError::Window(e.to_string()))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or(RenderError::NoAdapter)?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
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
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                gfx.surface.configure(&gfx.device, &gfx.config);
                return;
            }
            Err(_) => return,
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
        frame.present();
    }

    fn pick_at_cursor(&mut self) {
        let Some(gfx) = &mut self.gfx else { return };
        let picked = gfx.renderer.pick(
            &gfx.device,
            &gfx.queue,
            gfx.config.width,
            gfx.config.height,
            self.cursor.x as u32,
            self.cursor.y as u32,
        );
        match picked {
            Some(idx) => self.select(idx as usize),
            None => self.clear_selection(),
        }
    }

    /// Select a node: trace a flow if an anchor is set, rebuild the detail
    /// panel, and recolor the graph to highlight the selection/path.
    fn select(&mut self, idx: usize) {
        self.path = match self.flow_anchor {
            Some(anchor) if anchor != idx => self.trace(anchor, idx),
            _ => Vec::new(),
        };
        let label = self
            .labels
            .get(idx)
            .map(String::as_str)
            .unwrap_or("<unknown>");
        if self.path.len() > 1 {
            lcw_telemetry::info!(target: "lcw::render", node = idx, %label, hops = self.path.len() - 1, "flow traced");
        } else {
            lcw_telemetry::info!(target: "lcw::render", node = idx, %label, "picked node");
        }
        self.selected = Some(idx);
        self.rebuild_hud();
        self.recolor();
        self.request_redraw();
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.flow_anchor = None;
        self.path.clear();
        if let Some(gfx) = &mut self.gfx {
            gfx.renderer.set_hud(&gfx.device, &[]);
            gfx.renderer.set_nodes(&gfx.device, &self.scene.nodes);
            gfx.renderer.set_edges(&gfx.device, &self.scene.edges);
        }
        self.request_redraw();
    }

    /// Mark the current selection as the start of a flow; the next pick traces
    /// the shortest call path to it.
    fn mark_flow_anchor(&mut self) {
        if let Some(sel) = self.selected {
            self.flow_anchor = Some(sel);
            lcw_telemetry::info!(target: "lcw::render", node = sel, "flow anchor set");
            self.rebuild_hud();
            self.recolor();
            self.request_redraw();
        }
    }

    /// Open the selected node's source location in an editor.
    fn open_selected(&self) {
        let Some(sel) = self.selected else { return };
        let Some(info) = self.infos.get(sel) else {
            return;
        };
        if info.file.is_empty() {
            lcw_telemetry::info!(target: "lcw::render", "no source location for selection");
            return;
        }
        open_in_editor(&info.file, info.line);
    }

    /// BFS shortest path over successor adjacency, in pick-id space.
    fn trace(&self, from: usize, to: usize) -> Vec<usize> {
        let n = self.infos.len();
        if from >= n || to >= n {
            return Vec::new();
        }
        if from == to {
            return vec![from];
        }
        let mut prev = vec![usize::MAX; n];
        let mut seen = vec![false; n];
        let mut queue = VecDeque::new();
        queue.push_back(from);
        seen[from] = true;
        while let Some(u) = queue.pop_front() {
            for &v in &self.infos[u].succ {
                let v = v as usize;
                if v >= n || seen[v] {
                    continue;
                }
                seen[v] = true;
                prev[v] = u;
                if v == to {
                    let mut path = vec![to];
                    let mut c = to;
                    while c != from {
                        c = prev[c];
                        path.push(c);
                    }
                    path.reverse();
                    return path;
                }
                queue.push_back(v);
            }
        }
        Vec::new()
    }

    fn rebuild_hud(&mut self) {
        let Some(sel) = self.selected else { return };
        let mut panel = panel_for(&self.infos, sel, &self.path);
        if self.flow_anchor == Some(sel) {
            panel.subtitle = format!("{}   [flow source — pick a target]", panel.subtitle);
        }
        let crumb = (self.path.len() > 1).then(|| {
            let chain: Vec<&str> = self
                .path
                .iter()
                .map(|&i| {
                    self.infos
                        .get(i)
                        .map(|x| short_name(&x.title))
                        .unwrap_or("?")
                })
                .collect();
            format!("flow: {}", chain.join(" -> "))
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
        let Some(gfx) = &mut self.gfx else { return };
        if self.selected.is_none() && self.path.is_empty() {
            gfx.renderer.set_nodes(&gfx.device, &self.scene.nodes);
            gfx.renderer.set_edges(&gfx.device, &self.scene.edges);
            return;
        }
        let (nodes, edges) = highlight(&self.scene, self.selected, self.flow_anchor, &self.path);
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
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    match Command::new(opener).arg(file).spawn() {
        Ok(_) => lcw_telemetry::info!(target: "lcw::render", %file, "opened via OS opener"),
        Err(e) => lcw_telemetry::warn!(target: "lcw::render", error = %e, "could not open editor"),
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
                    let delta = new - self.cursor;
                    if delta.length() > 0.5 {
                        self.moved_while_pressed = true;
                    }
                    self.camera.pan_pixels(delta);
                    if let Some(gfx) = &self.gfx {
                        gfx.window.request_redraw();
                    }
                }
                self.cursor = new;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            self.dragging = true;
                            self.moved_while_pressed = false;
                            self.press_origin = Some(self.cursor);
                        }
                        ElementState::Released => {
                            self.dragging = false;
                            if !self.moved_while_pressed {
                                self.pick_at_cursor();
                            }
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
                Key::Character(c) if c.eq_ignore_ascii_case("o") => self.open_selected(),
                Key::Named(NamedKey::Enter) => self.open_selected(),
                Key::Named(NamedKey::Escape) => self.clear_selection(),
                _ => {}
            },
            _ => {}
        }
    }
}
