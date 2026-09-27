//! wasm/WebGPU driver: bind the same [`Renderer`] to an HTML `<canvas>` inside
//! a webview. This is what the Tauri UI (and a future VS Code webview) uses,
//! proving the renderer is genuinely surface-agnostic (Principle I).

use glam::Vec2;
use web_sys::HtmlCanvasElement;

use crate::camera::Camera2D;
use crate::renderer::Renderer;
use crate::scene::SceneData;

/// Does this browser expose a WebGPU entry point (`navigator.gpu`)?
///
/// Presence is not the same as usability: a browser can expose `navigator.gpu`
/// and still hand back no adapter (no GPU, a driver blocklist, a headless or
/// software-rendered session). [`WebViewer::new`] therefore treats this as a
/// *preference*, not a decision, and keeps a WebGL2 retry in reserve.
fn webgpu_available() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::get(&window.navigator(), &wasm_bindgen::JsValue::from_str("gpu"))
        .map(|gpu| !gpu.is_undefined() && !gpu.is_null())
        .unwrap_or(false)
}

/// Swap `canvas` for a freshly created, identical one and return it.
///
/// A canvas hands out exactly one kind of drawing context for its whole life:
/// `getContext("webgpu")` yields a `GPUCanvasContext` *even where WebGPU cannot
/// produce an adapter*, and from then on `getContext("webgl2")` on that element
/// returns `null`. So a failed WebGPU attempt poisons the element for the
/// WebGL2 fallback, and the only way back is a new element. Attributes are
/// copied so CSS and layout are unaffected; the caller must re-attach event
/// listeners, since those belonged to the old node.
fn replace_canvas(canvas: &HtmlCanvasElement) -> Result<HtmlCanvasElement, String> {
    use wasm_bindgen::JsCast;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "no document".to_string())?;
    let fresh: HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|e| format!("creating a replacement canvas: {e:?}"))?
        .dyn_into()
        .map_err(|_| "created element is not a canvas".to_string())?;

    let attrs = canvas.attributes();
    for i in 0..attrs.length() {
        if let Some(a) = attrs.item(i) {
            let _ = fresh.set_attribute(&a.name(), &a.value());
        }
    }
    fresh.set_width(canvas.width());
    fresh.set_height(canvas.height());

    let parent = canvas
        .parent_node()
        .ok_or_else(|| "the canvas is not in the document".to_string())?;
    parent
        .replace_child(&fresh, canvas)
        .map_err(|e| format!("replacing the canvas: {e:?}"))?;
    Ok(fresh)
}

/// A graph viewer bound to a canvas. Create with [`WebViewer::new`], then drive
/// it from DOM events (`render`, `resize`, `pan`, `zoom`).
pub struct WebViewer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    camera: Camera2D,
    /// The canvas actually being drawn into. Not always the one passed to
    /// [`WebViewer::new`]: see [`WebViewer::canvas`].
    canvas: HtmlCanvasElement,
}

impl WebViewer {
    /// Bind a viewer to `canvas` and upload `scene`, using WebGPU where the
    /// browser has it and WebGL2 otherwise.
    ///
    /// When WebGPU is advertised but yields no adapter (headless, software
    /// rendering, a driver blocklist), the attempt has already poisoned that
    /// canvas element, so this retries on a **replacement canvas** and the
    /// caller must re-attach its event listeners — see
    /// [`canvas`](WebViewer::canvas).
    pub async fn new(canvas: HtmlCanvasElement, scene: &SceneData) -> Result<WebViewer, String> {
        if !webgpu_available() {
            return Self::with_backend(canvas, scene, wgpu::Backends::GL).await;
        }
        match Self::with_backend(canvas.clone(), scene, wgpu::Backends::BROWSER_WEBGPU).await {
            Ok(viewer) => Ok(viewer),
            Err(webgpu_err) => {
                let fresh = replace_canvas(&canvas)
                    .map_err(|swap_err| format!("{webgpu_err}; WebGL2 retry: {swap_err}"))?;
                Self::with_backend(fresh, scene, wgpu::Backends::GL)
                    .await
                    .map_err(|gl_err| format!("{webgpu_err}; WebGL2 retry: {gl_err}"))
            }
        }
    }

    /// The canvas this viewer draws into. It differs from the element handed to
    /// [`new`](WebViewer::new) when the WebGPU attempt failed and the viewer
    /// fell back to WebGL2 on a replacement element; callers that keep their own
    /// handle (for sizing, pointer events or coordinate math) must refresh it
    /// from here after construction.
    pub fn canvas(&self) -> HtmlCanvasElement {
        self.canvas.clone()
    }

    async fn with_backend(
        canvas: HtmlCanvasElement,
        scene: &SceneData,
        backends: wgpu::Backends,
    ) -> Result<WebViewer, String> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| format!("create_surface: {e}"))?;
        // Prefer a real GPU, then accept a software adapter: a webview without
        // hardware acceleration should still draw the graph rather than show an
        // empty canvas.
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
        {
            Some(a) => a,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: true,
                })
                .await
                .ok_or_else(|| {
                    format!(
                        "no compatible GPU adapter for {backends:?} (this webview offers neither \
                         WebGPU nor WebGL2); the graph canvas stays empty, the rest of the UI \
                         still works"
                    )
                })?,
        };

        // Ask for exactly what this adapter supports. wgpu's *default* limits
        // are well above what WebGL2 can offer, so requesting them makes
        // `request_device` fail outright on the GL backend; and a browser's
        // WebGPU implementation may also report limits below those defaults.
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("lcw web device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
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
            canvas,
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

    /// Center on a world point *and* make it legible: zoom in to at least
    /// `min_zoom` px per world unit (never out). Used when the Explorer jumps
    /// the view to a node picked from the outline or a caller/callee list.
    pub fn focus_on(&mut self, x: f32, y: f32, min_zoom: f32) {
        self.camera.center = Vec2::new(x, y);
        if self.camera.zoom < min_zoom {
            self.camera.zoom = min_zoom;
        }
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
