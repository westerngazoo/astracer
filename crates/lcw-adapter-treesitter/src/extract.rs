//! Tree-sitter based extraction of a [`CodeGraph`] from Rust source.
//!
//! Two passes over the parsed trees (Principle II: no cleverness, just data):
//!   1. `collect_defs` records every function/method as a node and indexes it
//!      by short name for later resolution.
//!   2. `collect_calls` walks each function body and links call sites to the
//!      best-matching definition, falling back to an `External` placeholder.
//!
//! Resolution is deliberately *heuristic* (fast mode). The semantic adapter
//! (`lcw-adapter-ra`) is the precise alternative behind the same trait.

use std::collections::HashMap;
use std::path::Path;

use lcw_core::{
    AdapterError, CallKind, CodeGraph, Edge, EdgeKind, FileFragment, FileId, Node, NodeFlags,
    NodeId, NodeKind, NodeStats, RawCall, SourceFile, SourceSpan,
};
use tree_sitter::{Node as TsNode, Parser};

/// Parse a batch of Rust files into a single call graph.
///
/// Implemented as *extract then resolve* — the very same two passes the
/// incremental path uses — so the whole-batch result is byte-identical to
/// feeding the files through [`extract_file`] + [`resolve_fragments`].
pub fn parse_rust(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    // One parser reused across the batch: reloading the grammar per file is
    // measurably slower on big repos.
    let mut parser = make_parser()?;
    let mut fragments = Vec::with_capacity(files.len());
    for sf in files {
        fragments.push(extract_with_parser(&mut parser, sf)?);
    }
    Ok(resolve_fragments(&fragments))
}

/// A tree-sitter parser configured for Rust.
fn make_parser() -> Result<Parser, AdapterError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| AdapterError::Other(format!("failed to load Rust grammar: {e}")))?;
    Ok(parser)
}

/// Extract one file's [`FileFragment`] (definitions + unresolved call sites)
/// with no cross-file linking. Pure w.r.t. `sf`, hence safely cacheable by the
/// engine's incremental path.
pub fn extract_file(sf: &SourceFile) -> Result<FileFragment, AdapterError> {
    let mut parser = make_parser()?;
    extract_with_parser(&mut parser, sf)
}

fn extract_with_parser(parser: &mut Parser, sf: &SourceFile) -> Result<FileFragment, AdapterError> {
    let tree = parser
        .parse(sf.text.as_bytes(), None)
        .ok_or_else(|| AdapterError::Parse {
            path: sf.path.clone(),
            message: "tree-sitter produced no tree".into(),
        })?;
    let module_base = module_path_from(&sf.path);

    // Spans are captured with a fragment-local file id (0); resolution remaps
    // them onto the real interned file id.
    let mut defs = Vec::new();
    collect_defs_into(&mut defs, tree.root_node(), &sf.text, &module_base);

    let mut calls = Vec::new();
    collect_calls_into(&mut calls, tree.root_node(), &sf.text, &module_base);

    Ok(FileFragment {
        path: sf.path.clone(),
        defs,
        calls,
    })
}

/// Link a set of per-file fragments into a single graph — the global
/// resolution pass. Definitions are interned first (so a call can resolve to a
/// def in any file), then every call site is linked to a def or an `External`
/// placeholder. The result is independent of whether a fragment was freshly
/// extracted or served from cache.
pub fn resolve_fragments(fragments: &[FileFragment]) -> CodeGraph {
    let mut graph = CodeGraph::new();

    // Pass 1: definitions (remapping each fragment-local span to its real file).
    let mut by_short: HashMap<String, Vec<NodeId>> = HashMap::new();
    for frag in fragments {
        let file = graph.intern_file(frag.path.clone());
        for def in &frag.defs {
            let mut node = def.clone();
            node.span = with_file(node.span, file);
            let name = node.name.clone();
            let id = graph.add_node(node);
            by_short.entry(name).or_default().push(id);
        }
    }
    for ids in by_short.values_mut() {
        ids.sort_unstable_by_key(|id| id.0);
        ids.dedup();
    }

    // Pass 2: calls / edges.
    for frag in fragments {
        let file = graph.intern_file(frag.path.clone());
        for rc in &frag.calls {
            let Some(caller) = graph.node_by_qualified(&rc.caller) else {
                continue;
            };
            // The caller's module scope == its node's `module_path` (defs and
            // calls reconstruct the same scope), so we needn't store it twice.
            let caller_module = graph.node(caller).module_path.clone();
            let callee = Callee {
                kind: callkind_to_class(rc.kind),
                path: rc.path.clone(),
                short: rc.short.clone(),
            };
            let target = resolve_target(&mut graph, &by_short, &callee, &caller_module);
            let edge_kind = if graph.node(target).kind == NodeKind::External {
                EdgeKind::Unresolved
            } else {
                callee.kind.edge_kind()
            };
            let call_site = with_file(rc.call_site, file);
            graph.add_edge(caller, target, Edge::new(edge_kind, call_site));
        }
    }

    graph
}

/// Rewrite a fragment-local span's `file` field to a real interned [`FileId`].
fn with_file(mut span: SourceSpan, file: FileId) -> SourceSpan {
    span.file = file.0;
    span
}

// ---------------------------------------------------------------------------
// Pass 1: definitions
// ---------------------------------------------------------------------------

fn collect_defs_into(defs: &mut Vec<Node>, node: TsNode, text: &str, scope: &str) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let name = field_text(child, "name", text);
                let inner = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs_into(defs, body, text, &inner);
                }
            }
            "impl_item" => {
                let ty = impl_type_name(child, text);
                let inner = if ty.is_empty() {
                    scope.to_string()
                } else {
                    join(scope, &ty)
                };
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs_into(defs, body, text, &inner);
                }
            }
            "trait_item" => {
                let name = field_text(child, "name", text);
                let inner = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs_into(defs, body, text, &inner);
                }
            }
            "function_item" | "function_signature_item" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                defs.push(make_function_node(child, text, scope, &name, &qualified));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs_into(defs, body, text, &qualified);
                }
            }
            _ => collect_defs_into(defs, child, text, scope),
        }
    }
}

/// Build a function/method [`Node`] with a fragment-local span (file id 0,
/// remapped at resolution). Its `id` is a placeholder assigned on insertion.
fn make_function_node(
    func: TsNode,
    text: &str,
    module_path: &str,
    name: &str,
    qualified: &str,
) -> Node {
    let flags = compute_flags(func, text, module_path, name);
    let stats = compute_stats(func, text);
    let kind = if flags.is_method {
        NodeKind::Method
    } else {
        NodeKind::Function
    };
    Node {
        id: NodeId(0),
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        module_path: module_path.to_string(),
        kind,
        span: span_of(func, FileId(0)),
        flags,
        stats,
    }
}

fn compute_flags(func: TsNode, text: &str, module_path: &str, name: &str) -> NodeFlags {
    let name_start = func
        .child_by_field_name("name")
        .map(|n| n.start_byte())
        .unwrap_or_else(|| func.start_byte());
    let header = &text[func.start_byte()..name_start];
    NodeFlags {
        is_pub: header.contains("pub"),
        is_async: header.contains("async"),
        is_unsafe: header.contains("unsafe"),
        is_test: is_test_fn(func, text, module_path, name),
        is_method: has_self_param(func),
        is_generic: func.child_by_field_name("type_parameters").is_some(),
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
        unsafe_blocks: acc.unsafe_blocks,
        allocations: acc.allocations,
        awaits: acc.awaits,
    }
}

#[derive(Default)]
struct StatAcc {
    decision_points: u32,
    statements: u32,
    returns: u32,
    unsafe_blocks: u32,
    allocations: u32,
    awaits: u32,
    max_nesting: u32,
}

fn scan_stats(node: TsNode, text: &str, depth: u32, acc: &mut StatAcc) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // Nested functions are their own nodes; don't fold their bodies in.
        if matches!(kind, "function_item" | "function_signature_item") {
            continue;
        }
        let mut d = depth;
        match kind {
            "if_expression" => {
                acc.decision_points += 1;
                d += 1;
            }
            "match_expression" => {
                d += 1;
            }
            "match_arm" => acc.decision_points += 1,
            "while_expression" | "for_expression" | "loop_expression" => {
                acc.decision_points += 1;
                d += 1;
            }
            "binary_expression" => {
                if is_logical_binary(child) {
                    acc.decision_points += 1;
                }
            }
            "try_expression" => acc.decision_points += 1,
            "return_expression" => acc.returns += 1,
            "await_expression" => acc.awaits += 1,
            "unsafe_block" => {
                acc.unsafe_blocks += 1;
                d += 1;
            }
            "let_declaration" | "expression_statement" => acc.statements += 1,
            _ => {}
        }
        if matches!(kind, "call_expression" | "macro_invocation") {
            if let Some(call) = callee_of_call(child, text) {
                if is_alloc_name(&call.path, &call.short, call.kind) {
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

fn collect_calls_into(calls: &mut Vec<RawCall>, node: TsNode, text: &str, scope: &str) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let inner = join(scope, &field_text(child, "name", text));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(calls, body, text, &inner);
                }
            }
            "impl_item" => {
                let ty = impl_type_name(child, text);
                let inner = if ty.is_empty() {
                    scope.to_string()
                } else {
                    join(scope, &ty)
                };
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(calls, body, text, &inner);
                }
            }
            "trait_item" => {
                let inner = join(scope, &field_text(child, "name", text));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(calls, body, text, &inner);
                }
            }
            "function_item" | "function_signature_item" => {
                let name = field_text(child, "name", text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    // The caller's own body calls first, then nested fns (each
                    // its own caller) — mirrors the original edge order.
                    scan_calls_into(calls, body, text, &qualified);
                    collect_calls_into(calls, body, text, &qualified);
                }
            }
            _ => collect_calls_into(calls, child, text, scope),
        }
    }
}

fn scan_calls_into(calls: &mut Vec<RawCall>, node: TsNode, text: &str, caller: &str) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // Nested functions are their own callers; their bodies are handled by
        // `collect_calls_into`, not folded into this caller.
        if matches!(kind, "function_item" | "function_signature_item") {
            continue;
        }
        if matches!(kind, "call_expression" | "macro_invocation") {
            if let Some(call) = callee_of_call(child, text) {
                calls.push(RawCall {
                    caller: caller.to_string(),
                    kind: class_to_callkind(call.kind),
                    path: call.path,
                    short: call.short,
                    call_site: span_of(child, FileId(0)),
                });
            }
        }
        scan_calls_into(calls, child, text, caller);
    }
}

fn resolve_target(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    call: &Callee,
    caller_module: &str,
) -> NodeId {
    // Exact qualified match wins (helps `Type::assoc` and scoped paths).
    if call.path.contains("::") {
        if let Some(id) = graph.node_by_qualified(&call.path) {
            return id;
        }
    }
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
                if call.path.contains("::") {
                    if let Some(id) = many
                        .iter()
                        .copied()
                        .find(|&id| graph.node(id).qualified_name.ends_with(&call.path))
                    {
                        return id;
                    }
                }
                return many[0];
            }
        }
    }
    let key = if call.path.is_empty() {
        call.short.clone()
    } else {
        call.path.clone()
    };
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
    Macro,
}

impl CallClass {
    fn edge_kind(self) -> EdgeKind {
        match self {
            CallClass::Direct => EdgeKind::DirectCall,
            CallClass::Method => EdgeKind::MethodCall,
            CallClass::Associated => EdgeKind::AssociatedCall,
            CallClass::Macro => EdgeKind::MacroCall,
        }
    }
}

/// Bridge the adapter's syntactic call class to the serializable
/// [`CallKind`] stored in a fragment, and back for resolution.
fn class_to_callkind(c: CallClass) -> CallKind {
    match c {
        CallClass::Direct => CallKind::Direct,
        CallClass::Method => CallKind::Method,
        CallClass::Associated => CallKind::Associated,
        CallClass::Macro => CallKind::Macro,
    }
}

fn callkind_to_class(k: CallKind) -> CallClass {
    match k {
        CallKind::Direct => CallClass::Direct,
        CallKind::Method => CallClass::Method,
        CallKind::Associated => CallClass::Associated,
        CallKind::Macro => CallClass::Macro,
    }
}

struct Callee {
    kind: CallClass,
    /// Full callee text as written (e.g. `Type::foo`, `foo`, `bar`).
    path: String,
    /// Last path segment (the resolvable short name).
    short: String,
}

fn callee_of_call(call: TsNode, text: &str) -> Option<Callee> {
    if call.kind() == "macro_invocation" {
        let m = call.child_by_field_name("macro")?;
        let path = node_text(m, text);
        let short = last_segment(&path);
        return Some(Callee {
            kind: CallClass::Macro,
            path,
            short,
        });
    }
    let func = call.child_by_field_name("function")?;
    resolve_callee_expr(func, text)
}

fn resolve_callee_expr(func: TsNode, text: &str) -> Option<Callee> {
    match func.kind() {
        "identifier" => {
            let n = node_text(func, text);
            Some(Callee {
                kind: CallClass::Direct,
                path: n.clone(),
                short: n,
            })
        }
        "scoped_identifier" => {
            let path = node_text(func, text);
            let short = func
                .child_by_field_name("name")
                .map(|n| node_text(n, text))
                .unwrap_or_else(|| last_segment(&path));
            Some(Callee {
                kind: CallClass::Associated,
                path,
                short,
            })
        }
        "field_expression" => {
            let field = func.child_by_field_name("field")?;
            let n = node_text(field, text);
            Some(Callee {
                kind: CallClass::Method,
                path: n.clone(),
                short: n,
            })
        }
        "generic_function" => {
            let inner = func.child_by_field_name("function")?;
            resolve_callee_expr(inner, text)
        }
        _ => {
            let n = node_text(func, text);
            let short = last_segment(&n);
            Some(Callee {
                kind: CallClass::Direct,
                path: n,
                short,
            })
        }
    }
}

fn is_alloc_name(path: &str, short: &str, kind: CallClass) -> bool {
    if kind == CallClass::Macro {
        return matches!(short, "vec" | "format");
    }
    if matches!(short, "clone" | "to_owned" | "to_vec" | "to_string") {
        return true;
    }
    matches!(
        path,
        "Box::new"
            | "Rc::new"
            | "Arc::new"
            | "Vec::new"
            | "Vec::with_capacity"
            | "Vec::from"
            | "String::new"
            | "String::from"
            | "HashMap::new"
            | "BTreeMap::new"
            | "HashSet::new"
    )
}

// ---------------------------------------------------------------------------
// Small tree-sitter helpers
// ---------------------------------------------------------------------------

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

fn has_self_param(func: TsNode) -> bool {
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        return params
            .named_children(&mut c)
            .any(|p| p.kind() == "self_parameter");
    }
    false
}

fn count_params(func: TsNode) -> u32 {
    let mut n = 0;
    if let Some(params) = func.child_by_field_name("parameters") {
        let mut c = params.walk();
        for p in params.named_children(&mut c) {
            if matches!(p.kind(), "parameter" | "variadic_parameter") {
                n += 1;
            }
        }
    }
    n
}

fn is_test_fn(func: TsNode, text: &str, module_path: &str, name: &str) -> bool {
    if module_path.contains("tests") || name.starts_with("test_") {
        return true;
    }
    let mut sib = func.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                if node_text(s, text).contains("test") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        sib = s.prev_sibling();
    }
    false
}

fn impl_type_name(impl_item: TsNode, text: &str) -> String {
    match impl_item.child_by_field_name("type") {
        Some(t) => type_base_name(t, text),
        None => String::new(),
    }
}

fn type_base_name(t: TsNode, text: &str) -> String {
    match t.kind() {
        "type_identifier" => node_text(t, text),
        "generic_type" => t
            .child_by_field_name("type")
            .map(|x| type_base_name(x, text))
            .unwrap_or_default(),
        "reference_type" => t
            .child_by_field_name("type")
            .map(|x| type_base_name(x, text))
            .unwrap_or_default(),
        "scoped_type_identifier" => t
            .child_by_field_name("name")
            .map(|x| node_text(x, text))
            .unwrap_or_else(|| last_segment(&node_text(t, text))),
        _ => last_segment(&node_text(t, text)),
    }
}

fn last_segment(path: &str) -> String {
    path.rsplit("::").next().unwrap_or(path).to_string()
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
/// `crates/lcw-core/src/graph.rs` -> `lcw_core::graph`.
pub fn module_path_from(path: &Path) -> String {
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    if let Some(src_pos) = comps.iter().rposition(|c| c == "src") {
        let crate_name = if src_pos > 0 {
            sanitize(&comps[src_pos - 1])
        } else {
            "crate".to_string()
        };
        let mut mods = vec![crate_name];
        let after = &comps[src_pos + 1..];
        for (i, c) in after.iter().enumerate() {
            let is_last = i + 1 == after.len();
            if is_last {
                let stem = c.strip_suffix(".rs").unwrap_or(c);
                if !matches!(stem, "lib" | "main" | "mod") {
                    mods.push(sanitize(stem));
                }
            } else {
                mods.push(sanitize(c));
            }
        }
        mods.join("::")
    } else {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "crate".to_string());
        sanitize(&stem)
    }
}

fn sanitize(s: &str) -> String {
    s.replace(['-', '.', ' '], "_")
}
