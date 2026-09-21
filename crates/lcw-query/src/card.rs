//! The **node card**: everything a reader wants when they click one function —
//! where it lives, how big/complex it is, and above all *what comes in* (its
//! callers) and *what goes out* (its callees), each with the edge kind, the
//! call multiplicity and the call site to jump to.
//!
//! This is the one record every front end renders (CLI `explain`, the native
//! HUD panel, the desktop detail pane), so the three can never disagree.

use lcw_core::{CodeGraph, EdgeKind, Node, NodeId, NodeKind, SourceSpan};
use serde::{Deserialize, Serialize};

use crate::entries::{classify_entry, EntryKind};

/// One caller or callee, with the edge that connects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRef {
    pub id: NodeId,
    pub name: String,
    pub qualified_name: String,
    pub kind: EdgeKind,
    /// Merged multiplicity: how many identical call sites this edge stands for.
    pub count: u32,
    /// First recorded call site (in the *caller's* file).
    pub call_site: SourceSpan,
    /// Path of the file that contains `call_site` (empty when unknown).
    pub site_file: String,
    /// The neighbor is an unresolved / external target.
    pub external: bool,
}

/// Raw per-function measurements, straight from the parser's [`lcw_core::NodeStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardMetrics {
    pub cyclomatic: u32,
    pub parameters: u32,
    pub lines_of_code: u32,
    pub max_nesting: u32,
    pub decision_points: u32,
    pub allocations: u32,
    pub unsafe_blocks: u32,
    pub awaits: u32,
}

/// The full detail record for one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCard {
    pub id: NodeId,
    pub name: String,
    pub qualified_name: String,
    pub module_path: String,
    pub kind: NodeKind,
    /// Source file (empty for external nodes) and 1-based line range.
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub flags: Vec<String>,
    pub metrics: CardMetrics,
    /// Set when the node is an entry point (see [`crate::entry_points`]).
    pub entry: Option<EntryKind>,
    pub fan_in: usize,
    pub fan_out: usize,
    /// Callers (inputs), sorted by qualified name.
    pub inputs: Vec<CallRef>,
    /// Callees (outputs), sorted by qualified name.
    pub outputs: Vec<CallRef>,
}

impl NodeCard {
    /// `file:line`, or `<external>`.
    pub fn location(&self) -> String {
        if self.file.is_empty() {
            "<external>".to_string()
        } else {
            format!("{}:{}", self.file, self.line)
        }
    }

    /// `basename:line`, the compact form for tight UIs.
    pub fn location_short(&self) -> String {
        if self.file.is_empty() {
            String::new()
        } else {
            format!("{}:{}", base_name(&self.file), self.line)
        }
    }
}

/// Build the card for `id`, or `None` when the id is out of range.
pub fn node_card(graph: &CodeGraph, id: NodeId) -> Option<NodeCard> {
    let n = graph.try_node(id)?;
    let file = file_of(graph, n.span.file());

    let mut inputs: Vec<CallRef> = graph
        .edges_in(id)
        .map(|(from, e)| call_ref(graph, from, e.kind, e.count, e.call_site))
        .collect();
    let mut outputs: Vec<CallRef> = graph
        .edges_out(id)
        .map(|(to, e)| call_ref(graph, to, e.kind, e.count, e.call_site))
        .collect();
    let by_name = |a: &CallRef, b: &CallRef| {
        a.external
            .cmp(&b.external)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
            .then_with(|| edge_kind_str(a.kind).cmp(edge_kind_str(b.kind)))
    };
    inputs.sort_by(by_name);
    outputs.sort_by(by_name);

    Some(NodeCard {
        id,
        name: n.name.clone(),
        qualified_name: n.qualified_name.clone(),
        module_path: n.module_path.clone(),
        kind: n.kind,
        file,
        line: n.span.start_line,
        end_line: n.span.end_line,
        flags: flags_of(n).into_iter().map(String::from).collect(),
        metrics: CardMetrics {
            cyclomatic: n.cyclomatic_complexity(),
            parameters: n.stats.parameters,
            lines_of_code: n.stats.lines_of_code,
            max_nesting: n.stats.max_nesting,
            decision_points: n.stats.decision_points,
            allocations: n.stats.allocations,
            unsafe_blocks: n.stats.unsafe_blocks,
            awaits: n.stats.awaits,
        },
        entry: classify_entry(graph, id),
        fan_in: graph.in_degree(id),
        fan_out: graph.out_degree(id),
        inputs,
        outputs,
    })
}

fn call_ref(
    graph: &CodeGraph,
    other: NodeId,
    kind: EdgeKind,
    count: u32,
    site: SourceSpan,
) -> CallRef {
    let o = graph.node(other);
    CallRef {
        id: other,
        name: o.name.clone(),
        qualified_name: o.qualified_name.clone(),
        kind,
        count,
        call_site: site,
        site_file: file_of(graph, site.file()),
        external: o.kind == NodeKind::External,
    }
}

fn file_of(graph: &CodeGraph, file: lcw_core::FileId) -> String {
    graph
        .file_path(file)
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Stable, human-readable node kind (`function`, `method`, `closure`, `external`).
pub fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

/// Stable, human-readable edge kind.
pub fn edge_kind_str(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::DirectCall => "direct",
        EdgeKind::MethodCall => "method",
        EdgeKind::AssociatedCall => "assoc",
        EdgeKind::MacroCall => "macro",
        EdgeKind::TraitDispatch => "trait",
        EdgeKind::Unresolved => "unresolved",
    }
}

/// The node's boolean attributes as short tags (`pub`, `async`, ...).
pub fn flags_of(n: &Node) -> Vec<&'static str> {
    let mut v = Vec::new();
    if n.flags.is_pub {
        v.push("pub");
    }
    if n.flags.is_async {
        v.push("async");
    }
    if n.flags.is_unsafe {
        v.push("unsafe");
    }
    if n.flags.is_test {
        v.push("test");
    }
    if n.flags.is_generic {
        v.push("generic");
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{app_graph, id};

    #[test]
    fn card_lists_inputs_and_outputs_with_edge_data() {
        let g = app_graph();
        let card = node_card(&g, id(&g, "app::core::parse")).unwrap();
        assert_eq!(card.location(), "src/core.rs:8");
        assert_eq!(card.location_short(), "core.rs:8");
        assert_eq!(card.flags, vec!["pub"]);
        assert_eq!(card.metrics.cyclomatic, 3);
        assert_eq!(card.entry, None);
        assert_eq!((card.fan_in, card.fan_out), (2, 1));

        let inputs: Vec<(&str, u32, u32)> = card
            .inputs
            .iter()
            .map(|c| (c.qualified_name.as_str(), c.count, c.call_site.start_line))
            .collect();
        // The two identical run -> parse call sites merged into count 2.
        assert_eq!(
            inputs,
            vec![("app::core::tests::test_parse", 1, 81), ("app::run", 2, 22)]
        );
        assert_eq!(card.inputs[1].site_file, "src/main.rs");

        let out = &card.outputs[0];
        assert_eq!(out.qualified_name, "app::core::Lexer::next");
        assert_eq!(out.kind, EdgeKind::MethodCall);
        assert!(!out.external);
    }

    #[test]
    fn externals_are_tagged_and_sorted_last() {
        let g = app_graph();
        let card = node_card(&g, id(&g, "app::io::load")).unwrap();
        assert_eq!(card.outputs.len(), 1);
        assert!(card.outputs[0].external);
        assert_eq!(card.outputs[0].kind, EdgeKind::Unresolved);
        let main = node_card(&g, id(&g, "app::main")).unwrap();
        assert_eq!(main.entry, Some(EntryKind::Main));
        assert!(node_card(&g, NodeId(999)).is_none());
    }

    #[test]
    fn card_serializes_for_tools() {
        let g = app_graph();
        let card = node_card(&g, id(&g, "app::run")).unwrap();
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["qualified_name"], "app::run");
        assert_eq!(json["outputs"].as_array().unwrap().len(), 2);
        assert_eq!(json["outputs"][1]["kind"], "direct_call");
    }
}
