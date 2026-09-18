//! Native winit + wgpu driver: opens a window and renders the graph with
//! pan (drag), zoom (wheel), and click-to-pick. This is the desktop
//! validation path / "power mode"; the same [`Renderer`] later runs in the
//! Tauri webview via wasm.

use std::sync::Arc;

use glam::Vec2;
use lcw_core::CodeGraph;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::camera::Camera2D;
use crate::scene::{self, SceneData};
use crate::{RenderError, Renderer};

/// Open a window and render `graph` using the given per-node `positions`
/// (indexed by `NodeId.0`). Blocks until the window is closed.
pub fn run(graph: &CodeGraph, positions: &[[f32; 2]]) -> Result<(), RenderError> {
    let scene = scene::build(graph, positions);
    let labels: Vec<String> = graph.nodes().map(|n| n.qualified_name.clone()).collect();

    let event_loop = EventLoop::new().map_err(|e| RenderError::Window(e.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new("Live Code Walk", scene, labels);
    event_loop
        .run_app(&mut app)
        .map_err(|e| RenderError::Window(e.to_string()))
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
    camera: Camera2D,
    gfx: Option<Gfx>,
    cursor: Vec2,
    press_origin: Option<Vec2>,
    dragging: bool,
    moved_while_pressed: bool,
}

impl App {
    fn new(title: &str, scene: SceneData, labels: Vec<String>) -> Self {
        App {
            title: title.to_string(),
            scene,
            labels,
            camera: Camera2D::default(),
            gfx: None,
            cursor: Vec2::ZERO,
            press_origin: None,
            dragging: false,
            moved_while_pressed: false,
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
            Some(idx) => {
                let label = self
                    .labels
                    .get(idx as usize)
                    .map(String::as_str)
                    .unwrap_or("<unknown>");
                lcw_telemetry::info!(target: "lcw::render", node = idx, %label, "picked node");
            }
            None => lcw_telemetry::info!(target: "lcw::render", "picked background"),
        }
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
            _ => {}
        }
    }
}
