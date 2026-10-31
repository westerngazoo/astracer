//! Compact call-tree layout for the detail pane **Flow** tab (DOM/SVG, no wgpu).

/// One node in a bounded call tree, independent of `lcw-query` types so layout
/// and mermaid generation are unit-testable in native `cargo test`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTreeNode {
    pub graph_index: usize,
    pub name: String,
    pub repeat: bool,
    pub external: bool,
    pub children: Vec<FlowTreeNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutNode {
    pub graph_index: usize,
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub repeat: bool,
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallTreeLayout {
    pub nodes: Vec<LayoutNode>,
    /// `(parent_layout_index, child_layout_index)` for connector lines.
    pub edges: Vec<(usize, usize)>,
    pub width: f32,
    pub height: f32,
}

const H_UNIT: f32 = 26.0;
const ROW_H: f32 = 54.0;
const PAD: f32 = 14.0;
pub const NODE_H: f32 = 30.0;
pub const FLOW_NODE_W: f32 = 108.0;
pub const LABEL_MAX: usize = 24;

/// Lay out a rooted call tree top-down (root at top, callees below).
pub fn layout_call_tree(root: &FlowTreeNode) -> CallTreeLayout {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let leaf_span = subtree_leaves(root);
    let total_w = leaf_span * H_UNIT;
    place(root, None, total_w * 0.5 + PAD, 0, &mut nodes, &mut edges);
    let height = if nodes.is_empty() {
        PAD * 2.0
    } else {
        nodes
            .iter()
            .map(|n| n.y)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0)
            + NODE_H
            + PAD
    };
    CallTreeLayout {
        nodes,
        edges,
        width: total_w.max(120.0) + PAD * 2.0,
        height,
    }
}

fn subtree_leaves(node: &FlowTreeNode) -> f32 {
    if node.children.is_empty() {
        return 1.0;
    }
    node.children.iter().map(subtree_leaves).sum::<f32>().max(1.0)
}

fn place(
    node: &FlowTreeNode,
    parent_idx: Option<usize>,
    x_center: f32,
    depth: u32,
    nodes: &mut Vec<LayoutNode>,
    edges: &mut Vec<(usize, usize)>,
) {
    let my_idx = nodes.len();
    nodes.push(LayoutNode {
        graph_index: node.graph_index,
        name: truncate_label(&node.name),
        x: x_center,
        y: PAD + depth as f32 * ROW_H,
        repeat: node.repeat,
        external: node.external,
    });
    if let Some(p) = parent_idx {
        edges.push((p, my_idx));
    }

    if node.children.is_empty() {
        return;
    }

    let span = subtree_leaves(node);
    let left = x_center - span * H_UNIT * 0.5;
    let mut cursor = left;
    for child in &node.children {
        let w = subtree_leaves(child);
        let cx = cursor + w * H_UNIT * 0.5;
        place(child, Some(my_idx), cx, depth + 1, nodes, edges);
        cursor += w * H_UNIT;
    }
}

fn truncate_label(name: &str) -> String {
    if name.len() <= LABEL_MAX {
        return name.to_string();
    }
    format!("{}…", name.chars().take(LABEL_MAX - 1).collect::<String>())
}

/// Mermaid `flowchart TB` for copy/debug (bounded trees only).
#[cfg_attr(not(test), allow(dead_code))]
pub fn call_tree_to_mermaid(root_name: &str, root: &FlowTreeNode) -> String {
    let mut lines = vec![
        "flowchart TB".to_string(),
        format!("  root[\"{}\"]", escape_mermaid(root_name)),
    ];
    walk_mermaid(root, "root", &mut lines);
    lines.join("\n")
}

fn walk_mermaid(node: &FlowTreeNode, parent_id: &str, lines: &mut Vec<String>) {
    for (i, child) in node.children.iter().enumerate() {
        let id = format!("{}_{}", parent_id, i);
        let label = escape_mermaid(&child.name);
        let suffix = if child.repeat { " ↺" } else { "" };
        lines.push(format!("  {}[\"{}{}\"]", id, label, suffix));
        lines.push(format!("  {} --> {}", parent_id, id));
        walk_mermaid(child, &id, lines);
    }
}

fn escape_mermaid(s: &str) -> String {
    s.replace('"', "'")
}

/// Horizontal trace path as mermaid LR (anchor → … → selection).
#[cfg_attr(not(test), allow(dead_code))]
pub fn path_to_mermaid(names: &[&str]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut lines = vec!["flowchart LR".to_string()];
    for (i, name) in names.iter().enumerate() {
        lines.push(format!("  n{i}[\"{}\"]", escape_mermaid(name)));
        if i > 0 {
            lines.push(format!("  n{} --> n{}", i - 1, i));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> FlowTreeNode {
        FlowTreeNode {
            graph_index: 0,
            name: "main".into(),
            repeat: false,
            external: false,
            children: vec![
                FlowTreeNode {
                    graph_index: 1,
                    name: "parse".into(),
                    repeat: false,
                    external: false,
                    children: vec![FlowTreeNode {
                        graph_index: 2,
                        name: "tokenize".into(),
                        repeat: false,
                        external: false,
                        children: vec![],
                    }],
                },
                FlowTreeNode {
                    graph_index: 3,
                    name: "render".into(),
                    repeat: false,
                    external: false,
                    children: vec![],
                },
            ],
        }
    }

    #[test]
    fn layout_has_root_and_edges() {
        let layout = layout_call_tree(&sample_tree());
        assert_eq!(layout.nodes.len(), 4);
        assert_eq!(layout.edges.len(), 3);
        assert!(layout.nodes[0].y < layout.nodes[1].y);
    }

    #[test]
    fn mermaid_contains_nodes_and_links() {
        let tree = sample_tree();
        let md = call_tree_to_mermaid("main", &tree);
        assert!(md.contains("flowchart TB"));
        assert!(md.contains("root[\"main\"]"));
        assert!(md.contains("parse"));
        assert!(md.contains("root --> root_0"));
    }

    #[test]
    fn path_mermaid_chains_hops() {
        let md = path_to_mermaid(&["a", "b", "c"]);
        assert!(md.contains("n0[\"a\"]"));
        assert!(md.contains("n0 --> n1"));
        assert!(md.contains("n1 --> n2"));
    }
}
