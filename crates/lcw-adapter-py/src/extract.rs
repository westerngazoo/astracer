//! Tree-sitter based extraction of a [`CodeGraph`] from Python source.
//!
//! Two passes over the parsed trees (Principle II: no cleverness, just data):
//!   1. `collect_defs` records every `def` (module function, method, or nested
//!      function) as a node and indexes it by short name for later resolution.
//!   2. `collect_calls` walks each function body and links call sites to the
//!      best-matching definition, falling back to an `External` placeholder.
//!
//! Resolution is deliberately *heuristic* (fast mode), matching the Rust
//! adapter: unique short name wins, then same-module short name. Python's
//! dynamic dispatch means precise resolution needs a type-aware backend behind
//! the same trait.

use std::collections::HashMap;
use std::path::Path;

use lcw_core::{
    AdapterError, CodeGraph, Edge, EdgeKind, FileId, Node, NodeFlags, NodeId, NodeKind, NodeStats,
    SourceFile, SourceSpan,
};
use tree_sitter::{Node as TsNode, Parser, Tree};

/// Parse a batch of Python files into a single call graph.
pub fn parse_python(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|e| AdapterError::Other(format!("failed to load Python grammar: {e}")))?;

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
        let module_base = module_path_from(&sf.path);
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
            "class_definition" => {
                let name = field_text(child, "name", text);
                let inner = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &inner);
                }
            }
            "function_definition" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                let id = add_function_node(graph, child, text, file, scope, &qualified);
                by_short.entry(name).or_default().push(id);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &qualified);
                }
            }
            // `decorated_definition`, `block`, `if_statement`, ... are just
            // containers; descend so decorated and conditionally-defined
            // functions are still discovered with the right scope.
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
) -> NodeId {
    let name = last_segment(qualified);
    let is_method = is_method_def(func);
    let flags = compute_flags(func, text, &name, is_method);
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

fn compute_flags(func: TsNode, text: &str, name: &str, is_method: bool) -> NodeFlags {
    let header = def_header(func, text);
    NodeFlags {
        // Python convention: a leading underscore marks non-public API.
        is_pub: !name.starts_with('_'),
        is_async: header_has_kw(header, "async"),
        is_unsafe: false,
        is_test: name.starts_with("test"),
        is_method,
        is_generic: func.child_by_field_name("type_parameters").is_some(),
        // Python has no foreign-ABI export marker in its own syntax; a C
        // extension's entry points live in C sources this adapter never sees.
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
        awaits: acc.awaits,
    }
}

#[derive(Default)]
struct StatAcc {
    decision_points: u32,
    statements: u32,
    returns: u32,
    allocations: u32,
    awaits: u32,
    max_nesting: u32,
}

fn scan_stats(node: TsNode, text: &str, depth: u32, acc: &mut StatAcc) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // Nested defs are their own nodes; don't fold their bodies in.
        if is_nested_def(child) {
            continue;
        }
        let kind = child.kind();
        let mut d = depth;
        match kind {
            "if_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "elif_clause" => acc.decision_points += 1,
            "for_statement" | "while_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "with_statement" | "try_statement" | "match_statement" => d += 1,
            "except_clause" | "case_clause" => acc.decision_points += 1,
            "boolean_operator" | "conditional_expression" => acc.decision_points += 1,
            "return_statement" => acc.returns += 1,
            "await" => acc.awaits += 1,
            "list_comprehension"
            | "dictionary_comprehension"
            | "set_comprehension"
            | "generator_expression" => acc.allocations += 1,
            "expression_statement" => acc.statements += 1,
            _ => {}
        }
        if kind == "call" {
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
            "class_definition" => {
                let inner = join(scope, &field_text(child, "name", text));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls(graph, by_short, body, text, file, &inner);
                }
            }
            "function_definition" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    if let Some(caller) = graph.node_by_qualified(&qualified) {
                        scan_calls(graph, by_short, body, text, file, caller, scope);
                    }
                    collect_calls(graph, by_short, body, text, file, &qualified);
                }
            }
            _ => collect_calls(graph, by_short, child, text, file, scope),
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
        if is_nested_def(child) {
            continue;
        }
        if child.kind() == "call" {
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
    /// Resolvable short name (the last attribute / identifier).
    short: String,
}

fn callee_of_call(call: TsNode, text: &str) -> Option<Callee> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(Callee {
            kind: CallClass::Direct,
            short: node_text(func, text),
        }),
        "attribute" => {
            let short = func
                .child_by_field_name("attribute")
                .map(|a| node_text(a, text))
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

/// Heuristic allocation names: the common container builtins.
fn is_alloc_name(short: &str) -> bool {
    matches!(short, "list" | "dict" | "set" | "tuple" | "bytearray")
}

// ---------------------------------------------------------------------------
// Small tree-sitter helpers
// ---------------------------------------------------------------------------

/// A `def` is a method when its nearest enclosing definition is a class (rather
/// than the module or another function). Walking the parent chain also handles
/// `@decorator`-wrapped methods, which sit inside a `decorated_definition`.
fn is_method_def(func: TsNode) -> bool {
    let mut cur = func.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_definition" => return true,
            "function_definition" | "module" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

/// Definitions that get their own graph node, so an enclosing scan must not
/// fold their bodies in.
fn is_nested_def(node: TsNode) -> bool {
    matches!(
        node.kind(),
        "function_definition" | "class_definition" | "decorated_definition"
    )
}

fn node_text(node: TsNode, text: &str) -> String {
    text[node.start_byte()..node.end_byte()].to_string()
}

fn field_text(node: TsNode, field: &str, text: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(n, text))
        .unwrap_or_default()
}

/// Text from the start of a definition up to its name — the span carrying the
/// `async` modifier.
fn def_header<'a>(func: TsNode, text: &'a str) -> &'a str {
    let end = func
        .child_by_field_name("name")
        .map(|n| n.start_byte())
        .unwrap_or_else(|| func.start_byte());
    &text[func.start_byte()..end.max(func.start_byte())]
}

fn header_has_kw(header: &str, kw: &str) -> bool {
    header
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == kw)
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

fn count_params(func: TsNode) -> u32 {
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        return params
            .named_children(&mut c)
            .filter(|p| {
                matches!(
                    p.kind(),
                    "identifier"
                        | "typed_parameter"
                        | "default_parameter"
                        | "typed_default_parameter"
                        | "list_splat_pattern"
                        | "dictionary_splat_pattern"
                        | "tuple_pattern"
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

/// Derive a dotted-style module path from a file path, rendered with `::` for
/// consistency with the rest of the pipeline, e.g. `pkg/sub/mod.py` ->
/// `pkg::sub::mod`. A package's `__init__` collapses to its directory.
pub fn module_path_from(path: &Path) -> String {
    let mut comps: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    if comps.is_empty() {
        return "module".to_string();
    }

    let last = comps.len() - 1;
    let stem = strip_py_ext(&comps[last]);
    if stem == "__init__" {
        comps.pop();
    } else {
        comps[last] = stem.to_string();
    }
    if comps.is_empty() {
        return "module".to_string();
    }

    comps
        .iter()
        .map(|c| sanitize(c))
        .collect::<Vec<_>>()
        .join("::")
}

fn strip_py_ext(name: &str) -> &str {
    name.strip_suffix(".pyi")
        .or_else(|| name.strip_suffix(".py"))
        .unwrap_or(name)
}

fn sanitize(s: &str) -> String {
    s.replace(['-', '.', ' '], "_")
}
