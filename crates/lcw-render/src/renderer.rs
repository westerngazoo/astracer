//! Surface-agnostic wgpu renderer. It knows nothing about windows or canvases:
//! it takes a `Device`/`Queue`, a target texture format, and draws the graph.
//! The native driver (and, later, the Tauri/WASM canvas) simply supply a
//! surface texture view (Principle I: one closed rendering interface).

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::camera::Camera2D;
use crate::scene::{EdgeVertex, NodeInstance, SceneData};

/// Integer texture format used for GPU picking.
const PICK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

/// Two triangles covering [-1, 1]^2; expanded per instance into a disc.
const QUAD: &[[f32; 2]; 6] = &[
    [-1.0, -1.0],
    [1.0, -1.0],
    [1.0, 1.0],
    [-1.0, -1.0],
    [1.0, 1.0],
    [-1.0, 1.0],
];

/// GPU resources for drawing one graph.
pub struct Renderer {
    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    node_pipeline: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,
    pick_pipeline: wgpu::RenderPipeline,
    quad_vbo: wgpu::Buffer,
    node_vbo: wgpu::Buffer,
    node_count: u32,
    edge_vbo: wgpu::Buffer,
    edge_vertices: u32,
    pick_target: Option<PickTarget>,
}

struct PickTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Create a renderer for a given color target format and scene.
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        scene: &SceneData,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lcw graph shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/graph.wgsl").into()),
        });

        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lcw camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lcw camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lcw camera bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lcw pipeline layout"),
            bind_group_layouts: &[&camera_bgl],
            push_constant_ranges: &[],
        });

        // Vertex buffer layouts.
        let quad_attrs = wgpu::vertex_attr_array![0 => Float32x2];
        let quad_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 2]>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &quad_attrs,
        };
        let inst_attrs =
            wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32, 3 => Float32x4, 4 => Uint32];
        let inst_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<NodeInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &inst_attrs,
        };
        let edge_attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];
        let edge_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<EdgeVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &edge_attrs,
        };

        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);

        let node_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lcw nodes"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_node"),
                buffers: &[quad_layout.clone(), inst_layout.clone()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_node"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let edge_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lcw edges"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_edge"),
                buffers: &[edge_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_edge"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let pick_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lcw pick"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_node"),
                buffers: &[quad_layout, inst_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_node_pick"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: PICK_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let quad_vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lcw quad"),
            contents: bytemuck::cast_slice(QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let node_vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lcw node instances"),
            contents: bytemuck::cast_slice(&scene.nodes),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let edge_vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lcw edges"),
            contents: bytemuck::cast_slice(&scene.edges),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Renderer {
            camera_buf,
            camera_bg,
            node_pipeline,
            edge_pipeline,
            pick_pipeline,
            quad_vbo,
            node_vbo,
            node_count: scene.nodes.len() as u32,
            edge_vbo,
            edge_vertices: scene.edges.len() as u32,
            pick_target: None,
        }
    }

    /// Push the current camera to the GPU.
    pub fn update_camera(&self, queue: &wgpu::Queue, camera: &Camera2D) {
        let uniform = CameraUniform {
            view_proj: camera.view_proj().to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&uniform));
    }

    /// Draw the graph (edges under nodes) into `target`.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lcw main pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.06,
                        g: 0.07,
                        b: 0.09,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_bind_group(0, &self.camera_bg, &[]);

        if self.edge_vertices > 0 {
            pass.set_pipeline(&self.edge_pipeline);
            pass.set_vertex_buffer(0, self.edge_vbo.slice(..));
            pass.draw(0..self.edge_vertices, 0..1);
        }

        if self.node_count > 0 {
            pass.set_pipeline(&self.node_pipeline);
            pass.set_vertex_buffer(0, self.quad_vbo.slice(..));
            pass.set_vertex_buffer(1, self.node_vbo.slice(..));
            pass.draw(0..6, 0..self.node_count);
        }
    }

    fn ensure_pick_target(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let needs = match &self.pick_target {
            Some(t) => t.width != width || t.height != height,
            None => true,
        };
        if needs {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lcw pick target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PICK_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            self.pick_target = Some(PickTarget {
                texture: tex,
                view,
                width,
                height,
            });
        }
    }

    /// GPU pick: render node ids into an R32Uint target and read back the
    /// pixel under `(x, y)`. Returns the picked node index, or `None` for the
    /// background. Blocks on the readback, so call it on interaction, not
    /// every frame.
    pub fn pick(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        x: u32,
        y: u32,
    ) -> Option<u32> {
        if width == 0 || height == 0 || self.node_count == 0 {
            return None;
        }
        self.ensure_pick_target(device, width, height);
        let x = x.min(width - 1);
        let y = y.min(height - 1);
        let pick = self.pick_target.as_ref().unwrap();
        let view = &pick.view;
        let pick_texture = &pick.texture;

        // 4 bytes per R32Uint texel; bytes_per_row must be 256-aligned.
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lcw pick readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lcw pick"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lcw pick pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pick_pipeline);
            pass.set_bind_group(0, &self.camera_bg, &[]);
            pass.set_vertex_buffer(0, self.quad_vbo.slice(..));
            pass.set_vertex_buffer(1, self.node_vbo.slice(..));
            pass.draw(0..6, 0..self.node_count);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: pick_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        drop(data);
        staging.unmap();

        if id == 0 {
            None
        } else {
            Some(id - 1)
        }
    }
}
