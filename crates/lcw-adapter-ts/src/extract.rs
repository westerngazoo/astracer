//! Tree-sitter based extraction of a [`CodeGraph`] from TypeScript source.
//!
//! Two passes over the parsed trees (Principle II: no cleverness, just data):
//!   1. `collect_defs` records every function/method/arrow-const as a node and
//!      indexes it by short name for later resolution.
//!   2. `collect_calls` walks each function body and links call sites to the
//!      best-matching definition, falling back to an `External` placeholder.
//!
//! Resolution is deliberately *heuristic* (fast mode), matching the Rust
//! adapter: exact path wins, then unique short name, then same-module short
//! name. TypeScript has no cross-module type inference here — that is a job for
//! a future semantic backend behind the same trait.

use std::collections::HashMap;
use std::path::Path;

use lcw_core::{
    AdapterError, CodeGraph, Edge, EdgeKind, FileId, Node, NodeFlags, NodeId, NodeKind, NodeStats,
    SourceFile, SourceSpan,
};
use tree_sitter::{Language, Node as TsNode, Parser, Tree};

/// Parse a batch of TypeScript files into a single call graph.
pub fn parse_typescript(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    // Both grammars expose identical node kinds; TSX just also accepts JSX, so
    // we pick per file extension and keep one parser instance.
    let ts_lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let tsx_lang: Language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let mut parser = Parser::new();

    let mut graph = CodeGraph::new();
    let mut parsed: Vec<Parsed> = Vec::with_capacity(files.len());
    for sf in files {
        let lang = if is_tsx(&sf.path) {
            &tsx_lang
        } else {
            &ts_lang
        };
        parser
            .set_language(lang)
            .map_err(|e| AdapterError::Other(format!("failed to load TypeScript grammar: {e}")))?;
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

fn is_tsx(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("tsx") | Some("jsx")
    )
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
            "class_declaration" | "abstract_class_declaration" => {
                let name = field_text(child, "name", text);
                let inner = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &inner);
                }
            }
            "function_declaration" | "generator_function_declaration" | "function_expression" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    // Anonymous function expression: not its own node; its body
                    // is scanned as part of the enclosing scope.
                    continue;
                }
                let qualified = join(scope, &name);
                let id = add_function_node(graph, child, text, file, scope, &qualified, false);
                by_short.entry(name).or_default().push(id);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &qualified);
                }
            }
            "method_definition" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                let id = add_function_node(graph, child, text, file, scope, &qualified, true);
                by_short.entry(name).or_default().push(id);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &qualified);
                }
            }
            "public_field_definition" => {
                // `foo = () => {}` / `foo = function() {}` is effectively a method.
                if let Some(value) = child.child_by_field_name("value").filter(is_function_value) {
                    let name = field_text(child, "name", text);
                    if name.is_empty() {
                        continue;
                    }
                    let qualified = join(scope, &name);
                    let id = add_function_node(graph, value, text, file, scope, &qualified, true);
                    by_short.entry(name).or_default().push(id);
                    if let Some(body) = value.child_by_field_name("body") {
                        collect_defs(graph, by_short, body, text, file, &qualified);
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                collect_var_defs(graph, by_short, child, text, file, scope);
            }
            _ => collect_defs(graph, by_short, child, text, file, scope),
        }
    }
}

/// Handle `const foo = () => {}` / `let bar = function() {}`: a declarator
/// whose initializer is a function becomes a first-class node.
fn collect_var_defs(
    graph: &mut CodeGraph,
    by_short: &mut HashMap<String, Vec<NodeId>>,
    decl: TsNode,
    text: &str,
    file: FileId,
    scope: &str,
) {
    let mut cursor = decl.walk();
    for declarator in decl.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(value) = declarator
            .child_by_field_name("value")
            .filter(is_function_value)
        else {
            continue;
        };
        let name = field_text(declarator, "name", text);
        if name.is_empty() {
            continue;
        }
        let qualified = join(scope, &name);
        let id = add_function_node(graph, value, text, file, scope, &qualified, false);
        by_short.entry(name).or_default().push(id);
        if let Some(body) = value.child_by_field_name("body") {
            collect_defs(graph, by_short, body, text, file, &qualified);
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
    let flags = compute_flags(func, text, &name, is_method);
    let stats = compute_stats(func);
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
    // Class members are public unless explicitly `private`/`protected`; free
    // functions/consts are "public API" when exported.
    let is_pub = if is_method {
        !header_has_kw(header, "private") && !header_has_kw(header, "protected")
    } else {
        is_exported(func)
    };
    NodeFlags {
        is_pub,
        is_async: header_has_kw(header, "async"),
        is_unsafe: false,
        is_test: is_test_name(name),
        is_method,
        is_generic: func.child_by_field_name("type_parameters").is_some(),
    }
}

fn compute_stats(func: TsNode) -> NodeStats {
    let mut acc = StatAcc::default();
    if let Some(body) = func.child_by_field_name("body") {
        scan_stats(body, 0, &mut acc);
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

fn scan_stats(node: TsNode, depth: u32, acc: &mut StatAcc) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // Nested definitions are their own nodes; don't fold their bodies in.
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
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "switch_statement" => d += 1,
            "switch_case" => acc.decision_points += 1,
            "catch_clause" => {
                acc.decision_points += 1;
                d += 1;
            }
            "ternary_expression" => acc.decision_points += 1,
            "binary_expression" => {
                if is_logical_binary(child) {
                    acc.decision_points += 1;
                }
            }
            "return_statement" => acc.returns += 1,
            "await_expression" => acc.awaits += 1,
            "new_expression" => acc.allocations += 1,
            "lexical_declaration" | "variable_declaration" | "expression_statement" => {
                acc.statements += 1
            }
            _ => {}
        }
        if d > acc.max_nesting {
            acc.max_nesting = d;
        }
        scan_stats(child, d, acc);
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
            "class_declaration" | "abstract_class_declaration" => {
                let inner = join(scope, &field_text(child, "name", text));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls(graph, by_short, body, text, file, &inner);
                }
            }
            "function_declaration" | "generator_function_declaration" | "function_expression" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                walk_def_body(graph, by_short, child, text, file, &join(scope, &name));
            }
            "method_definition" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                walk_def_body(graph, by_short, child, text, file, &join(scope, &name));
            }
            "public_field_definition" => {
                if let Some(value) = child.child_by_field_name("value").filter(is_function_value) {
                    let name = field_text(child, "name", text);
                    if !name.is_empty() {
                        walk_def_body(graph, by_short, value, text, file, &join(scope, &name));
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut c = child.walk();
                for declarator in child.named_children(&mut c) {
                    if declarator.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(value) = declarator
                        .child_by_field_name("value")
                        .filter(is_function_value)
                    else {
                        continue;
                    };
                    let name = field_text(declarator, "name", text);
                    if !name.is_empty() {
                        walk_def_body(graph, by_short, value, text, file, &join(scope, &name));
                    }
                }
            }
            _ => collect_calls(graph, by_short, child, text, file, scope),
        }
    }
}

/// For a definition `func` with fully qualified name `qualified`: attribute the
/// call sites in its body to it, then descend to find nested definitions.
fn walk_def_body(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    func: TsNode,
    text: &str,
    file: FileId,
    qualified: &str,
) {
    if let Some(body) = func.child_by_field_name("body") {
        if let Some(caller) = graph.node_by_qualified(qualified) {
            let module = module_of(qualified);
            scan_calls(graph, by_short, body, text, file, caller, module);
        }
        collect_calls(graph, by_short, body, text, file, qualified);
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
        if matches!(child.kind(), "call_expression" | "new_expression") {
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
    // TypeScript call paths use `.` (member access), not `::`, so an
    // unresolved target is keyed by its short name.
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
    Associated,
}

impl CallClass {
    fn edge_kind(self) -> EdgeKind {
        match self {
            CallClass::Direct => EdgeKind::DirectCall,
            CallClass::Method => EdgeKind::MethodCall,
            CallClass::Associated => EdgeKind::AssociatedCall,
        }
    }
}

struct Callee {
    kind: CallClass,
    /// Resolvable short name (the last segment).
    short: String,
}

fn callee_of_call(call: TsNode, text: &str) -> Option<Callee> {
    if call.kind() == "new_expression" {
        let ctor = call.child_by_field_name("constructor")?;
        let short =
            member_short(ctor, text).unwrap_or_else(|| last_segment(&node_text(ctor, text)));
        return Some(Callee {
            kind: CallClass::Associated,
            short,
        });
    }
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(Callee {
            kind: CallClass::Direct,
            short: node_text(func, text),
        }),
        "member_expression" => {
            let short =
                member_short(func, text).unwrap_or_else(|| last_segment(&node_text(func, text)));
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

/// The `property` of a `member_expression` (e.g. `foo` in `obj.foo`).
fn member_short(node: TsNode, text: &str) -> Option<String> {
    node.child_by_field_name("property")
        .map(|p| node_text(p, text))
}

// ---------------------------------------------------------------------------
// Small tree-sitter helpers
// ---------------------------------------------------------------------------

fn is_function_value(node: &TsNode) -> bool {
    matches!(node.kind(), "arrow_function" | "function_expression")
}

/// True for definitions that get their own graph node, so the enclosing scan
/// must not fold their bodies in. Anonymous inline callbacks are *not* tracked
/// separately, so their calls are attributed to the enclosing function.
fn is_nested_def(node: TsNode) -> bool {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" | "method_definition" => true,
        "arrow_function" | "function_expression" => matches!(
            node.parent().map(|p| p.kind()),
            Some("variable_declarator") | Some("public_field_definition")
        ),
        _ => false,
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

/// Text from the start of a definition up to its parameters/name/body — the
/// span that carries modifiers like `async`, `private`, `static`.
fn def_header<'a>(func: TsNode, text: &'a str) -> &'a str {
    let end = func
        .child_by_field_name("parameters")
        .or_else(|| func.child_by_field_name("parameter"))
        .or_else(|| func.child_by_field_name("name"))
        .or_else(|| func.child_by_field_name("body"))
        .map(|n| n.start_byte())
        .unwrap_or_else(|| func.end_byte());
    &text[func.start_byte()..end.max(func.start_byte())]
}

/// Whole-word keyword test so `asyncThing` doesn't read as `async`.
fn header_has_kw(header: &str, kw: &str) -> bool {
    header
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == kw)
}

/// Does an ancestor `export` this definition? (`export function`, `export const`)
fn is_exported(node: TsNode) -> bool {
    let mut cur = node.parent();
    for _ in 0..4 {
        match cur {
            Some(n) if n.kind() == "export_statement" => return true,
            Some(n) => cur = n.parent(),
            None => break,
        }
    }
    false
}

fn is_test_name(name: &str) -> bool {
    name.starts_with("test") || name.starts_with("Test")
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
        .any(|ch| matches!(ch.kind(), "&&" | "||" | "??"));
    found
}

fn count_params(func: TsNode) -> u32 {
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        return params
            .named_children(&mut c)
            .filter(|p| matches!(p.kind(), "required_parameter" | "optional_parameter"))
            .count() as u32;
    }
    // Parenthesis-free arrow: `x => x` exposes a single `parameter` field.
    u32::from(func.child_by_field_name("parameter").is_some())
}

fn last_segment(path: &str) -> String {
    path.rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// The module part of a fully qualified name (everything before the last `::`).
fn module_of(qualified: &str) -> &str {
    match qualified.rfind("::") {
        Some(idx) => &qualified[..idx],
        None => "",
    }
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

/// Derive a pseudo module path from a file path, e.g.
/// `src/models/user.ts` -> `models::user`; a leading `src/` is dropped and the
/// `index` barrel file collapses to its directory (like Rust's `mod.rs`).
pub fn module_path_from(path: &Path) -> String {
    let mut comps: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    if comps.first().map(|c| c == "src").unwrap_or(false) {
        comps.remove(0);
    }
    if comps.is_empty() {
        return "module".to_string();
    }

    let mut mods: Vec<String> = Vec::with_capacity(comps.len());
    let last = comps.len() - 1;
    for (i, c) in comps.iter().enumerate() {
        if i == last {
            let stem = strip_ts_ext(c);
            if stem != "index" {
                mods.push(sanitize(stem));
            }
        } else {
            mods.push(sanitize(c));
        }
    }
    if mods.is_empty() {
        "module".to_string()
    } else {
        mods.join("::")
    }
}

/// Strip a TypeScript extension, including the compound `.d.ts`.
fn strip_ts_ext(name: &str) -> &str {
    for suffix in [".d.ts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem;
        }
    }
    name
}

fn sanitize(s: &str) -> String {
    s.replace(['-', '.', ' '], "_")
}
