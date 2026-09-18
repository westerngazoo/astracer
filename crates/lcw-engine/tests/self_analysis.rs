//! Dogfooding: the engine analyzes the livewalk workspace itself and checks
//! that the resulting graph is sane. This is a real end-to-end smoke test of
//! file discovery + Layer 1 parsing.

use std::path::PathBuf;

use lcw_config::Config;
use lcw_engine::Engine;

fn workspace_root() -> PathBuf {
    // crates/lcw-engine -> workspace root is two levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

#[test]
fn analyzes_own_workspace() {
    let engine = Engine::new(Config::default());
    let report = engine
        .analyze(&workspace_root())
        .expect("analysis of the workspace should succeed");

    // The workspace has well over 100 functions across its crates.
    assert!(
        report.graph.node_count() > 100,
        "expected >100 nodes, got {}",
        report.graph.node_count()
    );
    assert!(report.graph.edge_count() > 100);

    // A couple of known symbols should be present and correctly qualified.
    assert!(
        report
            .graph
            .node_by_qualified("lcw_engine::Engine::analyze")
            .is_some(),
        "Engine::analyze should be discovered"
    );
    assert!(report
        .graph
        .node_by_qualified("lcw_core::graph::CodeGraph::add_node")
        .is_some());
}
