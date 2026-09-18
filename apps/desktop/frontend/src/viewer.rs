//! Rendering-adjacent helpers that don't belong in the UI components: CPU-side
//! hit testing (the webview can't reliably read back the GPU pick buffer) and
//! turning a picked node into display data for the detail panel.

use lcw_core::{CodeGraph, NodeId, NodeKind};
use lcw_render::SceneData;

/// Human-readable detail for the currently selected node.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDetail {
    pub name: String,
    pub qualified_name: String,
    pub module_path: String,
    pub kind: String,
    pub location: String,
    pub complexity: u32,
    pub fan_in: u32,
    pub fan_out: u32,
    pub lines_of_code: u32,
    pub parameters: u32,
    pub allocations: u32,
    pub flags: Vec<String>,
}

/// Return the index of the node whose disc contains `world`, nearest first.
///
/// Scene node centers/radii are in world units, so the caller converts the
/// cursor to world space (via `WebViewer::screen_to_world`) before calling.
pub fn hit_test(scene: &SceneData, world: [f32; 2]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_dist = f32::MAX;
    for (i, node) in scene.nodes.iter().enumerate() {
        let dx = node.center[0] - world[0];
        let dy = node.center[1] - world[1];
        let dist = (dx * dx + dy * dy).sqrt();
        // A few px of slop so small nodes stay clickable.
        if dist <= node.radius + 3.0 && dist < best_dist {
            best_dist = dist;
            best = Some(i);
        }
    }
    best
}

/// Build detail-panel data for node `index` from the reconstructed graph.
pub fn detail_for(graph: &CodeGraph, index: usize) -> Option<NodeDetail> {
    let id = NodeId(index as u32);
    let node = graph.try_node(id)?;

    let location = match graph.file_path(node.span.file()) {
        Some(path) => format!("{}:{}", path.display(), node.span.start_line),
        None => "<external>".to_string(),
    };

    let mut flags = Vec::new();
    if node.flags.is_pub {
        flags.push("pub".to_string());
    }
    if node.flags.is_async {
        flags.push("async".to_string());
    }
    if node.flags.is_unsafe {
        flags.push("unsafe".to_string());
    }
    if node.flags.is_test {
        flags.push("test".to_string());
    }
    if node.flags.is_generic {
        flags.push("generic".to_string());
    }
    if node.flags.is_method {
        flags.push("method".to_string());
    }

    Some(NodeDetail {
        name: node.name.clone(),
        qualified_name: node.qualified_name.clone(),
        module_path: node.module_path.clone(),
        kind: kind_label(node.kind).to_string(),
        location,
        complexity: node.cyclomatic_complexity(),
        fan_in: graph.in_degree(id) as u32,
        fan_out: graph.out_degree(id) as u32,
        lines_of_code: node.stats.lines_of_code,
        parameters: node.stats.parameters,
        allocations: node.stats.allocations,
        flags,
    })
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}
