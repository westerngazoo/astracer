//! wasm/WebGPU driver: bind the same [`Renderer`] to an HTML `<canvas>` inside
//! a webview. This is what the Tauri UI (and a future VS Code webview) uses,
//! proving the renderer is genuinely surface-agnostic (Principle I).

use glam::Vec2;
use web_sys::HtmlCanvasElement;

use crate::camera::Camera2D;
use crate::renderer::Renderer;
use crate::scene::SceneData;

/// A graph viewer bound to a canvas. Create with [`WebViewer::new`], then drive
/// it from DOM events (`render`, `resize`, `pan`, `zoom`).
pub struct WebViewer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    camera: Camera2D,
}

impl WebViewer {
    /// Initialize WebGPU (WebGL2 fallback) on `canvas` and upload `scene`.
    pub async fn new(canvas: HtmlCanvasElement, scene: &SceneData) -> Result<WebViewer, String> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            ..Default::default()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| format!("create_surface: {e}"))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| "no compatible GPU adapter".to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .map_err(|e| format!("request_device: {e}"))?;

        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "surface configuration unsupported".to_string())?;
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        surface.configure(&device, &config);

        let renderer = Renderer::new(&device, config.format, scene);
        let mut camera = Camera2D {
            viewport: Vec2::new(width as f32, height as f32),
            ..Default::default()
        };
        camera.fit(scene.min.into(), scene.max.into());

        Ok(WebViewer {
            surface,
            device,
            queue,
            config,
            renderer,
            camera,
        })
    }

    /// Replace the scene *and* reframe it (e.g. after analyzing a new repo).
    pub fn set_scene(&mut self, scene: &SceneData) {
        self.renderer = Renderer::new(&self.device, self.config.format, scene);
        self.camera.fit(scene.min.into(), scene.max.into());
    }

    /// Swap in a new scene while keeping the current camera. Used when the
    /// scene changes but the view should not jump — toggling edge bundling or
    /// re-coloring for a search filter (Features 3 & 4).
    pub fn replace_scene(&mut self, scene: &SceneData) {
        self.renderer = Renderer::new(&self.device, self.config.format, scene);
    }

    /// A copy of the current camera, so the frontend can run `lcw-render`'s pure
    /// projection helpers (`select_labels`, `MinimapView`) against it.
    pub fn camera(&self) -> Camera2D {
        self.camera
    }

    /// Current viewport size in device px `[w, h]`.
    pub fn viewport(&self) -> [f32; 2] {
        self.camera.viewport.into()
    }

    /// Recenter the camera on a world point (minimap click-to-navigate).
    pub fn center_on(&mut self, x: f32, y: f32) {
        self.camera.center = Vec2::new(x, y);
    }

    /// Project a world point to canvas px (positions DOM label overlays).
    pub fn world_to_screen(&self, x: f32, y: f32) -> [f32; 2] {
        self.camera.world_to_screen(Vec2::new(x, y)).into()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.camera.viewport = Vec2::new(width as f32, height as f32);
        }
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.camera.pan_pixels(Vec2::new(dx, dy));
    }

    pub fn zoom_at(&mut self, x: f32, y: f32, factor: f32) {
        self.camera.zoom_at(Vec2::new(x, y), factor);
    }

    /// Frame the whole scene bounds into the viewport.
    pub fn fit(&mut self, min: [f32; 2], max: [f32; 2]) {
        self.camera.fit(min.into(), max.into());
    }

    /// Map a canvas-pixel position to world space, for CPU-side hit testing
    /// against node centers (see the note on [`pick`]).
    ///
    /// [`pick`]: WebViewer::pick
    pub fn screen_to_world(&self, x: f32, y: f32) -> [f32; 2] {
        self.camera.screen_to_world(Vec2::new(x, y)).into()
    }

    /// Draw a frame.
    pub fn render(&mut self) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer.update_camera(&self.queue, &self.camera);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lcw web frame"),
            });
        self.renderer.render(&mut encoder, &view);
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }

    /// Node index under the cursor, if any.
    ///
    /// NOTE: on the web the readback path needs an async buffer map (browsers
    /// forbid blocking the main thread), so this currently returns `None`. The
    /// frontend does CPU-side hit testing against node positions instead; the
    /// native path uses the full GPU picking implementation.
    pub fn pick(&mut self, _x: u32, _y: u32) -> Option<u32> {
        None
    }
}
