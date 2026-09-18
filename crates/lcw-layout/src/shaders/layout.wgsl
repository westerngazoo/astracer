// Force-directed layout (Fruchterman-Reingold) as GPU compute.
//
// One iteration = three ordered dispatches recorded back-to-back in the same
// compute pass (WebGPU guarantees each dispatch sees the previous one's storage
// writes):
//   1. `forces`    — per node: O(n) repulsion over all nodes + attraction over
//                     its CSR neighbours -> disp[i]. No data races: each node
//                     writes only its own slot.
//   2. `integrate` — apply gravity, cap by temperature, move the node.
//   3. `cool`      — a single invocation lowers the temperature.
//
// The maths mirror the CPU path in `lib.rs` exactly (seed, forces, cooling), so
// the two backends converge to visually equivalent layouts.

struct Sim {
    n: u32,
    k: f32,
    repulsion: f32,
    gravity: f32,
    cooling: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> sim: Sim;
@group(0) @binding(1) var<storage, read_write> pos: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> disp: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read> adj_off: array<u32>;
@group(0) @binding(4) var<storage, read> adj_nbr: array<u32>;
@group(0) @binding(5) var<storage, read_write> temp: array<f32>;

@compute @workgroup_size(64)
fn forces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= sim.n) {
        return;
    }
    let pi = pos[i];
    var d = vec2<f32>(0.0, 0.0);

    // Repulsion against every other node.
    for (var j: u32 = 0u; j < sim.n; j = j + 1u) {
        if (j == i) {
            continue;
        }
        let pj = pos[j];
        var e = pi - pj;
        var d2 = dot(e, e);
        if (d2 < 1e-4) {
            // Deterministic nudge for coincident nodes (matches the CPU path).
            e = vec2<f32>(
                f32((i * 31u + j) % 7u) - 3.0,
                f32((i * 17u + j) % 5u) - 2.0,
            );
            d2 = dot(e, e) + 1e-3;
        }
        let dist = sqrt(d2);
        let force = sim.repulsion * (sim.k * sim.k) / dist;
        d = d + e / dist * force;
    }

    // Attraction along incident edges (symmetric CSR adjacency).
    let start = adj_off[i];
    let end = adj_off[i + 1u];
    for (var a: u32 = start; a < end; a = a + 1u) {
        let j = adj_nbr[a];
        let e = pi - pos[j];
        let dist = max(sqrt(dot(e, e)), 1e-3);
        let force = (dist * dist) / sim.k;
        d = d - e / dist * force;
    }

    disp[i] = d;
}

@compute @workgroup_size(64)
fn integrate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= sim.n) {
        return;
    }
    var d = disp[i];
    let pi = pos[i];
    d = d - pi * (sim.gravity * sim.k);
    let len = max(length(d), 1e-6);
    let capped = min(len, temp[0]);
    pos[i] = pi + d / len * capped;
}

@compute @workgroup_size(1)
fn cool(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x == 0u) {
        temp[0] = max(temp[0] - sim.cooling, 0.0);
    }
}
