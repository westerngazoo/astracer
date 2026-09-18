// Call-graph shaders: instanced circular nodes + line-list edges, plus a
// pick variant that writes node ids into an R32Uint attachment.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

struct NodeVsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) pick_id: u32,
};

@vertex
fn vs_node(
    @location(0) corner: vec2<f32>,
    @location(1) center: vec2<f32>,
    @location(2) radius: f32,
    @location(3) color: vec4<f32>,
    @location(4) pick_id: u32,
) -> NodeVsOut {
    var out: NodeVsOut;
    let world = center + corner * radius;
    out.clip = camera.view_proj * vec4<f32>(world, 0.0, 1.0);
    out.local = corner;
    out.color = color;
    out.pick_id = pick_id;
    return out;
}

@fragment
fn fs_node(in: NodeVsOut) -> @location(0) vec4<f32> {
    let d = length(in.local);
    if (d > 1.0) {
        discard;
    }
    // Anti-aliased rim + subtle darkened border.
    let edge = 1.0 - smoothstep(0.85, 1.0, d);
    let border = smoothstep(0.75, 0.9, d);
    let rgb = mix(in.color.rgb, in.color.rgb * 0.55, border);
    return vec4<f32>(rgb, in.color.a * edge);
}

@fragment
fn fs_node_pick(in: NodeVsOut) -> @location(0) u32 {
    if (length(in.local) > 1.0) {
        discard;
    }
    return in.pick_id;
}

// ---------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------

struct EdgeVsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_edge(
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
) -> EdgeVsOut {
    var out: EdgeVsOut;
    out.clip = camera.view_proj * vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_edge(in: EdgeVsOut) -> @location(0) vec4<f32> {
    return in.color;
}
