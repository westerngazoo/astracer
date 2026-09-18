//! The call graph: the central artifact produced by Layer 1 (parsing) and
//! consumed by Layers 2 (lenses) and 3 (suggestions), plus the renderer.
//!
//! Backed by a `petgraph` [`DiGraph`] for cheap traversal / graph algorithms,
//! but exposed through a small, closed API (Principle I: strict modularity).
//! Callers never touch `petgraph` types directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction::{Incoming, Outgoing};
use serde::{Deserialize, Serialize};

use crate::ids::{FileId, NodeId};

/// What kind of symbol a node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A free function (`fn foo()`).
    Function,
    /// A method or associated function defined in an `impl` block.
    Method,
    /// A closure or async block that we chose to track as its own node.
    Closure,
    /// A call target we could not resolve to a definition (heuristic mode),
    /// e.g. `std`/external crate items or dynamic dispatch.
    External,
}

/// The relationship a directed edge encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `foo()` — free/associated function call resolved by path.
    DirectCall,
    /// `x.foo()` — method call (receiver-based).
    MethodCall,
    /// `Type::foo()` — associated call by type path.
    AssociatedCall,
    /// `foo!(...)` — macro invocation.
    MacroCall,
    /// A call resolved through a trait object / generic bound (semantic mode).
    TraitDispatch,
    /// Edge to an [`NodeKind::External`] / unresolved target.
    Unresolved,
}

impl EdgeKind {
    /// Small stable discriminant used for edge de-duplication keys.
    fn tag(self) -> u8 {
        match self {
            EdgeKind::DirectCall => 0,
            EdgeKind::MethodCall => 1,
            EdgeKind::AssociatedCall => 2,
            EdgeKind::MacroCall => 3,
            EdgeKind::TraitDispatch => 4,
            EdgeKind::Unresolved => 5,
        }
    }
}

/// A source location (1-based lines, 0-based columns), byte-free so it is
/// cheap to copy and serialize.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub file: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl SourceSpan {
    pub fn new(file: FileId, start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Self {
        Self {
            file: file.0,
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }

    pub fn file(&self) -> FileId {
        FileId(self.file)
    }
}

/// Boolean attributes of a node, packed together to stay cache friendly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeFlags {
    pub is_pub: bool,
    pub is_async: bool,
    pub is_unsafe: bool,
    pub is_test: bool,
    pub is_method: bool,
    pub is_generic: bool,
}

/// Raw structural counts captured while parsing a function body. These are the
/// *inputs* Layer 2 turns into named [`crate::metric::Metric`]s; keeping them
/// on the node avoids a second pass over the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStats {
    pub lines_of_code: u32,
    /// Number of decision points (`if`, `match` arm, `while`, `for`, `&&`,
    /// `||`, `?`) used to derive cyclomatic complexity as `decision_points + 1`.
    pub decision_points: u32,
    pub parameters: u32,
    pub statements: u32,
    pub max_nesting: u32,
    pub returns: u32,
    pub unsafe_blocks: u32,
    /// Heuristic count of heap allocations (`Box::new`, `Vec::new`, `vec!`,
    /// `.to_owned()`, `.clone()` on owned types, ...).
    pub allocations: u32,
    pub awaits: u32,
}

/// A node in the call graph: a callable symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// Short name, e.g. `parse`.
    pub name: String,
    /// Fully qualified path, e.g. `lcw_core::graph::CodeGraph::parse`.
    pub qualified_name: String,
    /// Owning module path, e.g. `lcw_core::graph`.
    pub module_path: String,
    pub kind: NodeKind,
    pub span: SourceSpan,
    pub flags: NodeFlags,
    pub stats: NodeStats,
}

impl Node {
    /// Convenience constructor for an unresolved / external call target.
    pub fn external(qualified_name: impl Into<String>) -> Self {
        let qualified_name = qualified_name.into();
        let name = qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&qualified_name)
            .to_string();
        Node {
            id: NodeId(u32::MAX),
            name,
            qualified_name,
            module_path: String::new(),
            kind: NodeKind::External,
            span: SourceSpan::default(),
            flags: NodeFlags::default(),
            stats: NodeStats::default(),
        }
    }

    /// Cyclomatic complexity derived from captured decision points.
    pub fn cyclomatic_complexity(&self) -> u32 {
        self.stats.decision_points + 1
    }
}

/// A directed call edge with a call site and a multiplicity `count`
/// (repeated identical calls are merged and counted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    pub call_site: SourceSpan,
    pub count: u32,
}

impl Edge {
    pub fn new(kind: EdgeKind, call_site: SourceSpan) -> Self {
        Edge {
            kind,
            call_site,
            count: 1,
        }
    }
}

/// The call graph plus its interned file table.
#[derive(Debug, Clone, Default)]
pub struct CodeGraph {
    graph: DiGraph<Node, Edge>,
    files: Vec<PathBuf>,
    file_index: HashMap<PathBuf, FileId>,
    qualified_index: HashMap<String, NodeId>,
    edge_index: HashMap<(u32, u32, u8), petgraph::graph::EdgeIndex>,
}

#[inline]
fn to_idx(id: NodeId) -> NodeIndex {
    NodeIndex::new(id.index())
}

#[inline]
fn to_id(idx: NodeIndex) -> NodeId {
    NodeId(idx.index() as u32)
}

impl CodeGraph {
    pub fn new() -> Self {
        Self::default()
    }

    // -- files ------------------------------------------------------------

    /// Intern a file path, returning a stable [`FileId`].
    pub fn intern_file(&mut self, path: impl Into<PathBuf>) -> FileId {
        let path = path.into();
        if let Some(&id) = self.file_index.get(&path) {
            return id;
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(path.clone());
        self.file_index.insert(path, id);
        id
    }

    pub fn file_path(&self, id: FileId) -> Option<&Path> {
        self.files.get(id.index()).map(|p| p.as_path())
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &Path)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, p)| (FileId(i as u32), p.as_path()))
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    // -- nodes ------------------------------------------------------------

    /// Insert a node, assigning and returning its [`NodeId`]. If a node with
    /// the same non-empty `qualified_name` already exists it is returned
    /// as-is (interning), which is how call targets get linked to defs.
    pub fn add_node(&mut self, mut node: Node) -> NodeId {
        if !node.qualified_name.is_empty() {
            if let Some(&existing) = self.qualified_index.get(&node.qualified_name) {
                // Upgrade a previously-external placeholder to a real def.
                if self.graph[to_idx(existing)].kind == NodeKind::External
                    && node.kind != NodeKind::External
                {
                    node.id = existing;
                    self.graph[to_idx(existing)] = node;
                }
                return existing;
            }
        }
        let idx = self.graph.add_node(node);
        let id = to_id(idx);
        self.graph[idx].id = id;
        let qn = self.graph[idx].qualified_name.clone();
        if !qn.is_empty() {
            self.qualified_index.insert(qn, id);
        }
        id
    }

    /// Look up an existing node by fully qualified name.
    pub fn node_by_qualified(&self, qualified_name: &str) -> Option<NodeId> {
        self.qualified_index.get(qualified_name).copied()
    }

    /// Get-or-create a node for a fully qualified name, creating an external
    /// placeholder via `make` when absent.
    pub fn intern_node(&mut self, qualified_name: &str, make: impl FnOnce() -> Node) -> NodeId {
        if let Some(&id) = self.qualified_index.get(qualified_name) {
            return id;
        }
        self.add_node(make())
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.graph[to_idx(id)]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.graph[to_idx(id)]
    }

    pub fn try_node(&self, id: NodeId) -> Option<&Node> {
        self.graph.node_weight(to_idx(id))
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.graph.node_weights()
    }

    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.graph.node_indices().map(to_id)
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    // -- edges ------------------------------------------------------------

    /// Add a call edge, merging with an identical existing edge (same
    /// endpoints and kind) by incrementing its `count`.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, edge: Edge) {
        let key = (from.0, to.0, edge.kind.tag());
        if let Some(&eidx) = self.edge_index.get(&key) {
            if let Some(w) = self.graph.edge_weight_mut(eidx) {
                w.count = w.count.saturating_add(edge.count);
                return;
            }
        }
        let eidx = self.graph.add_edge(to_idx(from), to_idx(to), edge);
        self.edge_index.insert(key, eidx);
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Iterate `(from, to, &edge)` over all edges.
    pub fn edges(&self) -> impl Iterator<Item = (NodeId, NodeId, &Edge)> {
        self.graph
            .edge_references()
            .map(|e| (to_id(e.source()), to_id(e.target()), e.weight()))
    }

    pub fn neighbors_out(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.graph
            .neighbors_directed(to_idx(id), Outgoing)
            .map(to_id)
    }

    pub fn neighbors_in(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.graph
            .neighbors_directed(to_idx(id), Incoming)
            .map(to_id)
    }

    pub fn out_degree(&self, id: NodeId) -> usize {
        self.graph.neighbors_directed(to_idx(id), Outgoing).count()
    }

    pub fn in_degree(&self, id: NodeId) -> usize {
        self.graph.neighbors_directed(to_idx(id), Incoming).count()
    }

    /// Access the underlying `petgraph` graph for algorithms that live in
    /// other crates (e.g. SCC detection in `lcw-analysis`). This is the one
    /// deliberate seam in the closed API.
    pub fn raw(&self) -> &DiGraph<Node, Edge> {
        &self.graph
    }

    // -- export -----------------------------------------------------------

    /// Produce a flat, serialization-friendly view (nodes + edges arrays).
    pub fn export(&self) -> GraphExport<'_> {
        let edges = self
            .edges()
            .map(|(from, to, e)| EdgeExport {
                from,
                to,
                kind: e.kind,
                call_site: e.call_site,
                count: e.count,
            })
            .collect();
        GraphExport {
            files: self.files.iter().map(|p| p.as_path()).collect(),
            nodes: self.graph.node_weights().collect(),
            edges,
        }
    }

    /// Produce an **owned**, round-trippable snapshot. Unlike [`export`], this
    /// deserializes back into a `CodeGraph` (via [`from_snapshot`]), which is
    /// how the engine ships a graph across the Tauri/webview boundary to the
    /// wasm renderer.
    ///
    /// [`export`]: CodeGraph::export
    /// [`from_snapshot`]: CodeGraph::from_snapshot
    pub fn snapshot(&self) -> GraphSnapshot {
        GraphSnapshot {
            files: self.files.clone(),
            nodes: self.graph.node_weights().cloned().collect(),
            edges: self
                .edges()
                .map(|(from, to, e)| EdgeExport {
                    from,
                    to,
                    kind: e.kind,
                    call_site: e.call_site,
                    count: e.count,
                })
                .collect(),
        }
    }

    /// Rebuild a graph from a [`GraphSnapshot`], preserving node indices (so
    /// edge endpoints, layout positions and pick ids all stay aligned).
    ///
    /// Nodes are inserted in order — `petgraph` hands back sequential indices —
    /// so `nodes[i]` keeps index `i`; the qualified-name and edge lookup tables
    /// are rebuilt to match.
    pub fn from_snapshot(snapshot: GraphSnapshot) -> Self {
        let mut g = CodeGraph::new();
        for (i, path) in snapshot.files.into_iter().enumerate() {
            let id = FileId(i as u32);
            g.files.push(path.clone());
            g.file_index.insert(path, id);
        }
        for node in snapshot.nodes {
            let idx = g.graph.add_node(node);
            let id = to_id(idx);
            g.graph[idx].id = id;
            let qn = g.graph[idx].qualified_name.clone();
            if !qn.is_empty() {
                g.qualified_index.insert(qn, id);
            }
        }
        let n = g.graph.node_count() as u32;
        for e in snapshot.edges {
            if e.from.0 < n && e.to.0 < n {
                let key = (e.from.0, e.to.0, e.kind.tag());
                let eidx = g.graph.add_edge(
                    to_idx(e.from),
                    to_idx(e.to),
                    Edge {
                        kind: e.kind,
                        call_site: e.call_site,
                        count: e.count,
                    },
                );
                g.edge_index.insert(key, eidx);
            }
        }
        g
    }
}

/// Owned/borrowed flat view of a [`CodeGraph`] for JSON export and tests.
#[derive(Debug, Serialize)]
pub struct GraphExport<'a> {
    pub files: Vec<&'a Path>,
    pub nodes: Vec<&'a Node>,
    pub edges: Vec<EdgeExport>,
}

/// An owned, round-trippable snapshot of a [`CodeGraph`] (see
/// [`CodeGraph::snapshot`] / [`CodeGraph::from_snapshot`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub files: Vec<PathBuf>,
    pub nodes: Vec<Node>,
    pub edges: Vec<EdgeExport>,
}

/// An edge flattened with explicit endpoints (edges alone don't store them).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EdgeExport {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub call_site: SourceSpan,
    pub count: u32,
}
