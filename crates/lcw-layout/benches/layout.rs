//! Criterion benchmark for the force-directed layout.
//!
//! The repulsion step is O(n^2) per iteration, so this measures how layout
//! cost scales with node count at a fixed iteration budget. Run with
//! `cargo bench -p lcw-layout`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lcw_core::{CodeGraph, Edge, EdgeKind, Node, SourceSpan};
use lcw_layout::{layout, LayoutParams};
use std::hint::black_box;

/// A synthetic graph: `n` nodes wired into a ring plus a few forward chords, so
/// there is real edge attraction fighting the node repulsion.
fn synth_graph(n: usize) -> CodeGraph {
    let mut g = CodeGraph::new();
    let file = g.intern_file("src/lib.rs");
    let ids: Vec<_> = (0..n)
        .map(|i| g.add_node(Node::external(format!("f{i}"))))
        .collect();
    let span = SourceSpan::new(file, 1, 0, 1, 1);
    for i in 0..n {
        for step in [1usize, 3, 7] {
            let to = (i + step) % n;
            if to != i {
                g.add_edge(ids[i], ids[to], Edge::new(EdgeKind::DirectCall, span));
            }
        }
    }
    g
}

fn bench_layout(c: &mut Criterion) {
    // Fixed, modest iteration budget so we isolate the per-iteration cost.
    let params = LayoutParams {
        iterations: 60,
        ..Default::default()
    };
    let mut group = c.benchmark_group("layout_fr");
    group.sample_size(20);

    for &n in &[200usize, 800, 2000] {
        let graph = synth_graph(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, graph| {
            b.iter(|| {
                let laid = layout(black_box(graph), &params);
                black_box(laid.positions.len())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_layout);
criterion_main!(benches);
