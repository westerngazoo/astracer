//! End-to-end smoke test for the rust-analyzer semantic adapter.
//!
//! Heavy (loads the whole Cargo workspace through rust-analyzer), so it is
//! `#[ignore]`d by default. Run it explicitly with:
//!
//! ```bash
//! cargo test -p lcw-adapter-ra --features semantic --test semantic_smoke -- --ignored --nocapture
//! ```
#![cfg(feature = "semantic")]

use lcw_adapter_ra::RustAnalyzerAdapter;
use lcw_core::{LanguageAdapter, SourceFile};

#[test]
#[ignore = "loads the workspace via rust-analyzer; run on demand"]
fn extracts_calls_from_this_workspace() {
    // A real, non-test source file with intra-file calls: `layout()` calls
    // `seed_positions()` and `bounds()`.
    let manifest = env!("CARGO_MANIFEST_DIR"); // .../crates/lcw-adapter-ra
    let target = std::path::Path::new(manifest)
        .join("../lcw-layout/src/lib.rs")
        .canonicalize()
        .expect("layout source exists");
    let text = std::fs::read_to_string(&target).expect("read layout source");

    let adapter = RustAnalyzerAdapter::new();
    let graph = adapter
        .parse(&[SourceFile::new(target, text)])
        .expect("semantic parse succeeds");

    eprintln!(
        "semantic graph: {} nodes, {} edges",
        graph.node_count(),
        graph.edge_count()
    );
    assert!(graph.node_count() > 0, "expected some function nodes");
    assert!(graph.edge_count() > 0, "expected some resolved call edges");

    // `layout` should resolve a call to `seed_positions` (same file).
    let has_layout = graph.nodes().any(|n| n.name == "layout");
    let has_seed = graph.nodes().any(|n| n.name == "seed_positions");
    assert!(
        has_layout && has_seed,
        "expected layout + seed_positions nodes"
    );
}
