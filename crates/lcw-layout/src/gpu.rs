//! GPU-compute force-directed layout (wgpu), behind the `gpu` feature.
//!
//! The CPU layout in [`crate::layout`] is O(n^2) per iteration in its repulsion
//! step; for very large graphs that dominates. This backend runs the same
//! algorithm as wgpu compute shaders: node repulsion, edge attraction (via a
//! symmetric CSR adjacency), integration and cooling all execute on the GPU,
//! with a single read-back of the final positions.
//!
//! It is surface-less (headless): it requests any adapter, so it also works off
//! a compositor. If no adapter is available (e.g. CI), callers can fall back to
//! the CPU path — see [`layout_gpu_or_cpu`].

use std::fmt;

use lcw_core::CodeGraph;
use wgpu::util::DeviceExt;

use crate::{bounds, seed_positions, Layout, LayoutParams, Vec2};

/// Simulation constants shared with `shaders/layout.wgsl` (std140 uniform).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Sim {
    n: u32,
    k: f32,
    repulsion: f32,
    gravity: f32,
    cooling: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Why a GPU layout could not run.
#[derive(Debug)]
pub enum GpuLayoutError {
    /// No usable wgpu adapter (no GPU / headless without a software fallback).
    NoAdapter,
    /// Requesting the logical device failed.
    Device(String),
    /// Reading positions back from the GPU failed.
    Readback(String),
}

impl fmt::Display for GpuLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuLayoutError::NoAdapter => write!(f, "no compatible wgpu adapter"),
            GpuLayoutError::Device(e) => write!(f, "wgpu device error: {e}"),
            GpuLayoutError::Readback(e) => write!(f, "gpu readback failed: {e}"),
        }
    }
}

impl std::error::Error for GpuLayoutError {}

/// Symmetric CSR adjacency built from the graph's unique edges.
struct Adjacency {
    offsets: Vec<u32>,
    neighbors: Vec<u32>,
}

fn build_adjacency(graph: &CodeGraph, n: usize) -> Adjacency {
    let mut degree = vec![0u32; n];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (a, b, _) in graph.edges() {
        let (a, b) = (a.0 as usize, b.0 as usize);
        if a == b || a >= n || b >= n {
            continue;
        }
        degree[a] += 1;
        degree[b] += 1;
        pairs.push((a, b));
    }

    let mut offsets = vec![0u32; n + 1];
    for i in 0..n {
        offsets[i + 1] = offsets[i] + degree[i];
    }
    let total = offsets[n] as usize;
    // Storage buffers cannot be zero-sized, so keep at least one slot.
    let mut neighbors = vec![0u32; total.max(1)];
    let mut cursor = offsets.clone();
    for (a, b) in pairs {
        neighbors[cursor[a] as usize] = b as u32;
        cursor[a] += 1;
        neighbors[cursor[b] as usize] = a as u32;
        cursor[b] += 1;
    }

    Adjacency { offsets, neighbors }
}

fn request_device() -> Result<(wgpu::Device, wgpu::Queue), GpuLayoutError> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or(GpuLayoutError::NoAdapter)?;

    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
        .map_err(|e| GpuLayoutError::Device(e.to_string()))
}

/// Compute a force-directed [`Layout`] on the GPU.
pub fn layout_gpu(graph: &CodeGraph, params: &LayoutParams) -> Result<Layout, GpuLayoutError> {
    let n = graph.node_count();
    if n == 0 {
        return Ok(Layout {
            positions: Vec::new(),
            min: [0.0, 0.0],
            max: [0.0, 0.0],
        });
    }

    let k = params.ideal_length.max(1.0);
    let seed = seed_positions(n, k);
    let adj = build_adjacency(graph, n);

    let (device, queue) = request_device()?;

    let sim = Sim {
        n: n as u32,
        k,
        repulsion: params.repulsion,
        gravity: params.gravity,
        cooling: params.temperature / (params.iterations.max(1) as f32),
        _pad0: 0.0,
        _pad1: 0.0,
        _pad2: 0.0,
    };

    let sim_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lcw layout sim"),
        contents: bytemuck::bytes_of(&sim),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let pos_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lcw layout pos"),
        contents: bytemuck::cast_slice(&seed),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let disp_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lcw layout disp"),
        size: (n * std::mem::size_of::<Vec2>()) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let off_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lcw layout adj_off"),
        contents: bytemuck::cast_slice(&adj.offsets),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let nbr_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lcw layout adj_nbr"),
        contents: bytemuck::cast_slice(&adj.neighbors),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let temp_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lcw layout temp"),
        contents: bytemuck::bytes_of(&params.temperature),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lcw layout staging"),
        size: (n * std::mem::size_of::<Vec2>()) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lcw layout compute"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/layout.wgsl").into()),
    });

    let storage_rw = |read_only: bool| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("lcw layout bgl"),
        entries: &[
            entry(
                0,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            entry(1, storage_rw(false)),
            entry(2, storage_rw(false)),
            entry(3, storage_rw(true)),
            entry(4, storage_rw(true)),
            entry(5, storage_rw(false)),
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lcw layout bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: sim_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: pos_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: disp_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: off_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: nbr_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: temp_buf.as_entire_binding(),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lcw layout pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let make_pipeline = |entry_point: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("lcw layout pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let forces = make_pipeline("forces");
    let integrate = make_pipeline("integrate");
    let cool = make_pipeline("cool");

    let groups = (n as u32).div_ceil(64);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lcw layout"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("lcw layout pass"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &bind_group, &[]);
        // WebGPU orders dispatches within a pass and makes each one's storage
        // writes visible to the next, so we can record the whole schedule here.
        for _ in 0..params.iterations {
            pass.set_pipeline(&forces);
            pass.dispatch_workgroups(groups, 1, 1);
            pass.set_pipeline(&integrate);
            pass.dispatch_workgroups(groups, 1, 1);
            pass.set_pipeline(&cool);
            pass.dispatch_workgroups(1, 1, 1);
        }
    }
    encoder.copy_buffer_to_buffer(&pos_buf, 0, &staging, 0, staging.size());
    queue.submit(Some(encoder.finish()));

    let positions = read_positions(&device, &staging, n)?;
    let (min, max) = bounds(&positions);
    Ok(Layout {
        positions,
        min,
        max,
    })
}

/// Try the GPU layout, transparently falling back to the CPU path on any GPU
/// error (missing adapter, device loss, ...). Convenience for callers that just
/// want "the fastest layout available".
pub fn layout_gpu_or_cpu(graph: &CodeGraph, params: &LayoutParams) -> Layout {
    match layout_gpu(graph, params) {
        Ok(layout) => layout,
        Err(_) => crate::layout(graph, params),
    }
}

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

fn read_positions(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    n: usize,
) -> Result<Vec<Vec2>, GpuLayoutError> {
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| GpuLayoutError::Readback(e.to_string()))?
        .map_err(|e| GpuLayoutError::Readback(e.to_string()))?;

    let data = slice.get_mapped_range();
    let positions: Vec<Vec2> = bytemuck::cast_slice::<u8, Vec2>(&data)[..n].to_vec();
    drop(data);
    staging.unmap();
    Ok(positions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{Edge, EdgeKind, Node, SourceSpan};

    fn ring(n: usize) -> CodeGraph {
        let mut g = CodeGraph::new();
        let f = g.intern_file("src/lib.rs");
        let ids: Vec<_> = (0..n)
            .map(|i| g.add_node(Node::external(format!("f{i}"))))
            .collect();
        for i in 0..n {
            let j = (i + 1) % n;
            g.add_edge(
                ids[i],
                ids[j],
                Edge::new(EdgeKind::DirectCall, SourceSpan::new(f, 1, 0, 1, 1)),
            );
        }
        g
    }

    #[test]
    fn gpu_layout_matches_node_count_when_available() {
        let g = ring(24);
        let params = LayoutParams {
            iterations: 40,
            ..Default::default()
        };
        match layout_gpu(&g, &params) {
            Ok(layout) => {
                assert_eq!(layout.positions.len(), 24);
                assert!(layout
                    .positions
                    .iter()
                    .all(|p| p[0].is_finite() && p[1].is_finite()));
            }
            // Headless CI without any adapter: nothing to assert, but the
            // fallback must still produce a valid layout.
            Err(GpuLayoutError::NoAdapter) => {
                let layout = layout_gpu_or_cpu(&g, &params);
                assert_eq!(layout.positions.len(), 24);
            }
            Err(e) => panic!("unexpected gpu error: {e}"),
        }
    }

    #[test]
    fn empty_graph_is_ok() {
        let g = CodeGraph::new();
        let layout = layout_gpu(&g, &LayoutParams::default()).unwrap();
        assert!(layout.positions.is_empty());
    }
}
