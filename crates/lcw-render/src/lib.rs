//! # lcw-render
//!
//! A `wgpu` renderer for the call graph. The [`Renderer`] is **surface
//! agnostic** (Principle I): it draws into any texture view, so the same code
//! serves the native winit window (this crate's `native` feature) and,
//! later, a `<canvas>` inside the Tauri webview (wasm target).
//!
//! Nodes are instanced discs (radius ~ fan-in, color ~ complexity), edges are
//! GPU lines, and picking is done on the GPU by rendering ids into an
//! `R32Uint` target and reading back one texel.

pub mod camera;
pub mod filter;
pub mod flow_view;
pub mod hud;
pub mod labels;
pub mod minimap;
pub mod module_view;
pub mod renderer;
pub mod scene;
pub mod text;

#[cfg(feature = "native")]
pub mod native;

#[cfg(target_arch = "wasm32")]
pub mod web;

pub use camera::Camera2D;
pub use filter::{apply_filter, compute_matches, is_active as filter_is_active, FilterStyle};
pub use flow_view::{build as build_flow_view, FlowGraph, FlowNode, FlowOptions};
pub use labels::{select_labels, LabelOptions, LabelPlacement};
pub use minimap::{MinimapView, Rect as MinimapRect};
pub use module_view::{build as build_module_view, ModuleViewOptions};
pub use renderer::Renderer;
pub use scene::{
    build as build_scene, build_with as build_scene_with, BundleOptions, EdgeVertex, NodeInstance,
    SceneData, SceneOptions,
};

/// Errors from the renderer / native driver.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no compatible GPU adapter was found")]
    NoAdapter,
    #[error("windowing error: {0}")]
    Window(String),
    #[error("screenshot error: {0}")]
    Screenshot(String),
}

#[cfg(all(test, feature = "native"))]
mod headless_tests {
    use super::*;
    use lcw_core::CodeGraph;

    /// Try to obtain a GPU device; returns `None` when no adapter is available
    /// (e.g. headless CI), so the test degrades to a no-op instead of failing.
    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
    }

    #[test]
    fn renders_offscreen_when_gpu_available() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter; skipping offscreen render test");
            return;
        };

        // Build a tiny graph and scene.
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let a = g.add_node(lcw_core::Node::external("a"));
        let b = g.add_node(lcw_core::Node::external("b"));
        g.add_edge(
            a,
            b,
            lcw_core::Edge::new(
                lcw_core::EdgeKind::DirectCall,
                lcw_core::SourceSpan::new(f, 1, 0, 1, 1),
            ),
        );
        let positions = [[-10.0f32, 0.0], [10.0, 0.0]];
        let scene = scene::build(&g, &positions);
        assert_eq!(scene.nodes.len(), 2);

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = Renderer::new(&device, format, &scene);
        let mut cam = Camera2D {
            viewport: glam::Vec2::new(256.0, 256.0),
            ..Default::default()
        };
        cam.fit(scene.min.into(), scene.max.into());
        renderer.update_camera(&queue, &cam);

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        renderer.render(&mut encoder, &view);
        queue.submit(std::iter::once(encoder.finish()));
        let _ = device.poll(wgpu::Maintain::Wait);

        // Picking the center should hit some node (a or b sit near the middle).
        let _ = renderer.pick(&device, &queue, 256, 256, 128, 128);
    }
}
