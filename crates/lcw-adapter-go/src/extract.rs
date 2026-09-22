//! Tree-sitter based extraction of a [`CodeGraph`] from Go source.
//!
//! Two passes over the parsed trees (Principle II: no cleverness, just data):
//!   1. `collect_defs` records every `func` (free function or method with a
//!      receiver) as a node and indexes it by short name for later resolution.
//!   2. `collect_calls` walks each function body and links call sites to the
//!      best-matching definition, falling back to an `External` placeholder.
//!
//! Resolution is deliberately *heuristic* (fast mode), matching the Rust
//! adapter: unique short name wins, then same-module short name. Go's package
//! system means fully precise cross-package resolution needs a type-aware
//! backend behind the same trait.

use std::collections::HashMap;
use std::path::Path;

use lcw_core::{
    AdapterError, CodeGraph, Edge, EdgeKind, FileId, Node, NodeFlags, NodeId, NodeKind, NodeStats,
    SourceFile, SourceSpan,
};
use tree_sitter::{Node as TsNode, Parser, Tree};

/// Parse a batch of Go files into a single call graph.
pub fn parse_go(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|e| AdapterError::Other(format!("failed to load Go grammar: {e}")))?;

    let mut graph = CodeGraph::new();
    let mut parsed: Vec<Parsed> = Vec::with_capacity(files.len());
    for sf in files {
        let tree = parser
            .parse(sf.text.as_bytes(), None)
            .ok_or_else(|| AdapterError::Parse {
                path: sf.path.clone(),
                message: "tree-sitter produced no tree".into(),
            })?;
        let file = graph.intern_file(sf.path.clone());
        // A Go file's scope is its declared package, not its path.
        let module_base =
            package_name(tree.root_node(), &sf.text).unwrap_or_else(|| module_path_from(&sf.path));
        parsed.push(Parsed {
            file,
            text: sf.text.clone(),
            tree,
            module_base,
        });
    }

    // Pass 1: definitions.
    let mut by_short: HashMap<String, Vec<NodeId>> = HashMap::new();
    for p in &parsed {
        collect_defs(
            &mut graph,
            &mut by_short,
            p.tree.root_node(),
            &p.text,
            p.file,
            &p.module_base,
        );
    }
    for ids in by_short.values_mut() {
        ids.sort_unstable_by_key(|id| id.0);
        ids.dedup();
    }

    // Pass 2: calls / edges.
    for p in &parsed {
        collect_calls(
            &mut graph,
            &by_short,
            p.tree.root_node(),
            &p.text,
            p.file,
            &p.module_base,
        );
    }

    Ok(graph)
}

struct Parsed {
    file: FileId,
    text: String,
    tree: Tree,
    module_base: String,
}

/// The declared `package` name, used as the module scope for the file.
fn package_name(root: TsNode, text: &str) -> Option<String> {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "package_clause" {
            let mut c = child.walk();
            for n in child.named_children(&mut c) {
                if n.kind() == "package_identifier" {
                    return Some(node_text(n, text));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Pass 1: definitions
// ---------------------------------------------------------------------------

fn collect_defs(
    graph: &mut CodeGraph,
    by_short: &mut HashMap<String, Vec<NodeId>>,
    node: TsNode,
    text: &str,
    file: FileId,
    scope: &str,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                let id = add_function_node(graph, child, text, file, scope, &qualified, false);
                by_short.entry(name).or_default().push(id);
            }
            "method_declaration" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                // `func (c *Counter) M()` is owned by `Counter`.
                let owner = join(scope, &receiver_type(child, text));
                let qualified = join(&owner, &name);
                let id = add_function_node(graph, child, text, file, &owner, &qualified, true);
                by_short.entry(name).or_default().push(id);
            }
            _ => collect_defs(graph, by_short, child, text, file, scope),
        }
    }
}

fn add_function_node(
    graph: &mut CodeGraph,
    func: TsNode,
    text: &str,
    file: FileId,
    module_path: &str,
    qualified: &str,
    is_method: bool,
) -> NodeId {
    let name = last_segment(qualified);
    let flags = compute_flags(func, &name, is_method);
    let stats = compute_stats(func, text);
    let kind = if is_method {
        NodeKind::Method
    } else {
        NodeKind::Function
    };
    graph.add_node(Node {
        id: NodeId(0),
        name,
        qualified_name: qualified.to_string(),
        module_path: module_path.to_string(),
        kind,
        span: span_of(func, file),
        flags,
        stats,
    })
}

fn compute_flags(func: TsNode, name: &str, is_method: bool) -> NodeFlags {
    NodeFlags {
        // Go marks exported identifiers with a leading uppercase letter.
        is_pub: name.chars().next().is_some_and(|c| c.is_uppercase()),
        is_async: false,
        is_unsafe: false,
        is_test: name.starts_with("Test") || name.starts_with("Benchmark"),
        is_method,
        is_generic: func.child_by_field_name("type_parameters").is_some(),
        // A capitalized Go name is exported to other *Go* packages, which
        // `is_pub` already records. True foreign-ABI export needs a cgo
        // `//export` directive; detecting that is a separate change.
        is_exported: false,
    }
}

fn compute_stats(func: TsNode, text: &str) -> NodeStats {
    let mut acc = StatAcc::default();
    if let Some(body) = func.child_by_field_name("body") {
        scan_stats(body, text, 0, &mut acc);
    }
    let start = func.start_position().row as u32;
    let end = func.end_position().row as u32;
    NodeStats {
        lines_of_code: end.saturating_sub(start) + 1,
        decision_points: acc.decision_points,
        parameters: count_params(func),
        statements: acc.statements,
        max_nesting: acc.max_nesting,
        returns: acc.returns,
        unsafe_blocks: 0,
        allocations: acc.allocations,
        awaits: 0,
    }
}

#[derive(Default)]
struct StatAcc {
    decision_points: u32,
    statements: u32,
    returns: u32,
    allocations: u32,
    max_nesting: u32,
}

fn scan_stats(node: TsNode, text: &str, depth: u32, acc: &mut StatAcc) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // A nested func literal is anonymous; its calls/stats fold into the
        // enclosing function (Go has no nested named functions).
        let kind = child.kind();
        let mut d = depth;
        match kind {
            "if_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "for_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "expression_switch_statement" | "type_switch_statement" | "select_statement" => d += 1,
            "expression_case" | "type_case" | "communication_case" => acc.decision_points += 1,
            "binary_expression" => {
                if is_logical_binary(child) {
                    acc.decision_points += 1;
                }
            }
            "return_statement" => acc.returns += 1,
            "short_var_declaration"
            | "assignment_statement"
            | "expression_statement"
            | "inc_statement"
            | "dec_statement"
            | "var_declaration"
            | "const_declaration"
            | "defer_statement"
            | "go_statement"
            | "send_statement" => acc.statements += 1,
            _ => {}
        }
        if kind == "call_expression" {
            if let Some(call) = callee_of_call(child, text) {
                if is_alloc_name(&call.short) {
                    acc.allocations += 1;
                }
            }
        }
        if d > acc.max_nesting {
            acc.max_nesting = d;
        }
        scan_stats(child, text, d, acc);
    }
}

// ---------------------------------------------------------------------------
// Pass 2: calls / edges
// ---------------------------------------------------------------------------

fn collect_calls(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    node: TsNode,
    text: &str,
    file: FileId,
    scope: &str,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                scan_def_body(graph, by_short, child, text, file, &qualified, scope);
            }
            "method_declaration" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let owner = join(scope, &receiver_type(child, text));
                let qualified = join(&owner, &name);
                scan_def_body(graph, by_short, child, text, file, &qualified, &owner);
            }
            _ => collect_calls(graph, by_short, child, text, file, scope),
        }
    }
}

fn scan_def_body(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    func: TsNode,
    text: &str,
    file: FileId,
    qualified: &str,
    owner_module: &str,
) {
    if let Some(body) = func.child_by_field_name("body") {
        if let Some(caller) = graph.node_by_qualified(qualified) {
            scan_calls(graph, by_short, body, text, file, caller, owner_module);
        }
    }
}

fn scan_calls(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    node: TsNode,
    text: &str,
    file: FileId,
    caller: NodeId,
    caller_module: &str,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some(call) = callee_of_call(child, text) {
                let target = resolve_target(graph, by_short, &call, caller_module);
                let edge_kind = if graph.node(target).kind == NodeKind::External {
                    EdgeKind::Unresolved
                } else {
                    call.kind.edge_kind()
                };
                graph.add_edge(caller, target, Edge::new(edge_kind, span_of(child, file)));
            }
        }
        scan_calls(graph, by_short, child, text, file, caller, caller_module);
    }
}

fn resolve_target(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    call: &Callee,
    caller_module: &str,
) -> NodeId {
    if let Some(cands) = by_short.get(&call.short) {
        match cands.as_slice() {
            [] => {}
            [only] => return *only,
            many => {
                if let Some(id) = many
                    .iter()
                    .copied()
                    .find(|&id| graph.node(id).module_path == caller_module)
                {
                    return id;
                }
                return many[0];
            }
        }
    }
    let key = call.short.clone();
    graph.intern_node(&key, || Node::external(key.clone()))
}

// ---------------------------------------------------------------------------
// Callee classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallClass {
    Direct,
    Method,
}

impl CallClass {
    fn edge_kind(self) -> EdgeKind {
        match self {
            CallClass::Direct => EdgeKind::DirectCall,
            CallClass::Method => EdgeKind::MethodCall,
        }
    }
}

struct Callee {
    kind: CallClass,
    /// Resolvable short name (identifier, or the selector's field).
    short: String,
}

fn callee_of_call(call: TsNode, text: &str) -> Option<Callee> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(Callee {
            kind: CallClass::Direct,
            short: node_text(func, text),
        }),
        // `x.Foo()` (method) and `pkg.Foo()` (qualified) share this shape; we
        // resolve by the trailing field name, matching the Rust adapter.
        "selector_expression" => {
            let short = func
                .child_by_field_name("field")
                .map(|f| node_text(f, text))
                .unwrap_or_else(|| last_segment(&node_text(func, text)));
            Some(Callee {
                kind: CallClass::Method,
                short,
            })
        }
        _ => Some(Callee {
            kind: CallClass::Direct,
            short: last_segment(&node_text(func, text)),
        }),
    }
}

/// Heuristic allocation builtins.
fn is_alloc_name(short: &str) -> bool {
    matches!(short, "make" | "new" | "append")
}

// ---------------------------------------------------------------------------
// Small tree-sitter helpers
// ---------------------------------------------------------------------------

/// The base type name of a method receiver, e.g. `Counter` for both
/// `(c Counter)` and `(c *Counter)`.
fn receiver_type(method: TsNode, text: &str) -> String {
    let Some(recv) = method.child_by_field_name("receiver") else {
        return String::new();
    };
    let mut cursor = recv.walk();
    for param in recv.named_children(&mut cursor) {
        if param.kind() == "parameter_declaration" {
            if let Some(ty) = param.child_by_field_name("type") {
                return type_base_name(ty, text);
            }
        }
    }
    String::new()
}

fn type_base_name(t: TsNode, text: &str) -> String {
    match t.kind() {
        "type_identifier" => node_text(t, text),
        "pointer_type" | "generic_type" => t
            .named_child(0)
            .map(|c| type_base_name(c, text))
            .unwrap_or_default(),
        "qualified_type" => t
            .child_by_field_name("name")
            .map(|n| node_text(n, text))
            .unwrap_or_else(|| last_segment(&node_text(t, text))),
        _ => last_segment(&node_text(t, text)),
    }
}

fn node_text(node: TsNode, text: &str) -> String {
    text[node.start_byte()..node.end_byte()].to_string()
}

fn field_text(node: TsNode, field: &str, text: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(n, text))
        .unwrap_or_default()
}

fn span_of(node: TsNode, file: FileId) -> SourceSpan {
    let s = node.start_position();
    let e = node.end_position();
    SourceSpan::new(
        file,
        s.row as u32 + 1,
        s.column as u32,
        e.row as u32 + 1,
        e.column as u32,
    )
}

fn is_logical_binary(node: TsNode) -> bool {
    let mut c = node.walk();
    let found = node
        .children(&mut c)
        .any(|ch| matches!(ch.kind(), "&&" | "||"));
    found
}

fn count_params(func: TsNode) -> u32 {
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        return params
            .named_children(&mut c)
            .filter(|p| {
                matches!(
                    p.kind(),
                    "parameter_declaration" | "variadic_parameter_declaration"
                )
            })
            .count() as u32;
    }
    0
}

fn last_segment(path: &str) -> String {
    path.rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn join(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        scope.to_string()
    } else {
        format!("{scope}::{name}")
    }
}

/// Fallback module path when a file has no `package` clause: the file stem.
pub fn module_path_from(path: &Path) -> String {
    path.file_stem()
        .map(|s| sanitize(&s.to_string_lossy()))
        .unwrap_or_else(|| "main".to_string())
}

fn sanitize(s: &str) -> String {
    s.replace(['-', '.', ' '], "_")
}
