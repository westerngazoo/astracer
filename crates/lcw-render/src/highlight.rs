//! Selection / flow-path **highlighting**: recolor a scene so one node, a flow
//! anchor and a traced path stand out and everything else recedes.
//!
//! Pure and target-agnostic (no GPU, no DOM): the native window, the headless
//! screenshot path and the wasm webview all call this and upload the result.

use std::collections::HashSet;

use crate::scene::{EdgeVertex, NodeInstance, SceneData};

/// Produce recolored node instances and edge vertices that focus a selection
/// and/or a traced `path` (pick ids == node indices): bright selection, orange
/// flow anchor, blue path nodes, amber path edges (or the selection's incident
/// edges when there is no path), and everything else dimmed.
pub fn highlight(
    scene: &SceneData,
    selected: Option<usize>,
    anchor: Option<usize>,
    path: &[usize],
) -> (Vec<NodeInstance>, Vec<EdgeVertex>) {
    let sel = selected.map(|i| i as u32);
    let anchor = anchor.map(|i| i as u32);
    let on_path: HashSet<u32> = path.iter().map(|&i| i as u32).collect();

    let mut nodes = scene.nodes.clone();
    for (i, inst) in nodes.iter_mut().enumerate() {
        let i = i as u32;
        if Some(i) == sel {
            inst.color = [1.0, 0.95, 0.5, 1.0];
            inst.radius *= 1.4;
        } else if Some(i) == anchor {
            inst.color = [1.0, 0.58, 0.30, 1.0];
            inst.radius *= 1.25;
        } else if on_path.contains(&i) {
            inst.color = [0.40, 0.85, 1.0, 1.0];
            inst.radius *= 1.2;
        } else {
            inst.color[3] *= 0.16;
        }
    }

    let path_pairs: HashSet<(u32, u32)> = path
        .windows(2)
        .map(|w| (w[0] as u32, w[1] as u32))
        .collect();
    let mut edges = scene.edges.clone();
    for (i, seg) in scene.edge_nodes.iter().enumerate() {
        let [a, b] = *seg;
        let vi = i * 2;
        let color = if path_pairs.contains(&(a, b)) {
            [0.98, 0.80, 0.30, 0.95]
        } else if path_pairs.is_empty() && (sel == Some(a) || sel == Some(b)) {
            let mut c = edges[vi].color;
            c[3] = 0.9;
            c
        } else {
            let mut c = edges[vi].color;
            c[3] *= if path_pairs.is_empty() { 0.10 } else { 0.05 };
            c
        };
        edges[vi].color = color;
        edges[vi + 1].color = color;
    }
    (nodes, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> SceneData {
        let node = |x: f32, pick: u32| NodeInstance {
            center: [x, 0.0],
            radius: 5.0,
            color: [0.5, 0.5, 0.5, 1.0],
            pick_id: pick,
        };
        let v = |x: f32| EdgeVertex {
            pos: [x, 0.0],
            color: [0.7, 0.7, 0.7, 0.5],
        };
        SceneData {
            nodes: vec![node(0.0, 1), node(10.0, 2), node(20.0, 3)],
            edges: vec![v(0.0), v(10.0), v(10.0), v(20.0)],
            edge_nodes: vec![[0, 1], [1, 2]],
            min: [0.0, 0.0],
            max: [20.0, 0.0],
            ..Default::default()
        }
    }

    #[test]
    fn selection_without_path_keeps_incident_edges_bright() {
        let (nodes, edges) = highlight(&scene(), Some(1), None, &[]);
        assert!((nodes[1].radius - 7.0).abs() < 1e-6);
        assert!((nodes[0].color[3] - 0.16).abs() < 1e-6);
        // Both edges touch node 1 -> both stay bright.
        assert!((edges[0].color[3] - 0.9).abs() < 1e-6);
        assert!((edges[2].color[3] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn path_colors_its_edges_and_dims_the_rest() {
        let (nodes, edges) = highlight(&scene(), Some(2), Some(0), &[0, 1, 2]);
        assert_eq!(nodes[0].color, [1.0, 0.58, 0.30, 1.0]); // anchor
        assert_eq!(nodes[1].color, [0.40, 0.85, 1.0, 1.0]); // on path
        assert_eq!(nodes[2].color, [1.0, 0.95, 0.5, 1.0]); // selected
        assert_eq!(edges[0].color, [0.98, 0.80, 0.30, 0.95]);
        assert_eq!(edges[3].color, [0.98, 0.80, 0.30, 0.95]);

        // A path that skips the second edge dims it hard.
        let (_, edges) = highlight(&scene(), Some(1), Some(0), &[0, 1]);
        assert!((edges[2].color[3] - 0.025).abs() < 1e-6);
    }
}
