//! Criterion benchmark for Layer 1 parsing + heuristic call resolution.
//!
//! Generates synthetic Go source of increasing size (functions that call a
//! handful of their neighbours) and measures end-to-end
//! [`LanguageAdapter::parse`] throughput. Run with `cargo bench -p
//! lcw-adapter-go`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lcw_adapter_go::GoTreeSitterAdapter;
use lcw_core::{LanguageAdapter, SourceFile};
use std::hint::black_box;

/// Build one `.go` file with `fns` functions. Each function calls the next few
/// functions so the extractor has real call-sites to resolve.
fn synth_source(fns: usize) -> String {
    let mut s = String::with_capacity(fns * 96);
    s.push_str("package bench\n\n");
    for i in 0..fns {
        s.push_str(&format!("func f{i}(x int) int {{\n\tacc := x + {i}\n"));
        for step in 1..=3 {
            let callee = (i + step) % fns;
            s.push_str(&format!("\tacc += f{callee}(acc)\n"));
        }
        s.push_str("\treturn acc\n}\n");
    }
    s
}

fn bench_parse(c: &mut Criterion) {
    let adapter = GoTreeSitterAdapter::new();
    let mut group = c.benchmark_group("parse_go");

    for &fns in &[100usize, 500, 2000] {
        let src = synth_source(fns);
        // Report throughput in bytes so numbers are comparable across sizes.
        group.throughput(Throughput::Bytes(src.len() as u64));
        let files = vec![SourceFile::new("bench/bench.go", src)];
        group.bench_with_input(BenchmarkId::from_parameter(fns), &files, |b, files| {
            b.iter(|| {
                let graph = adapter.parse(black_box(files)).expect("parse ok");
                black_box(graph.node_count())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
