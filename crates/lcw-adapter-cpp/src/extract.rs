//! Tree-sitter based extraction of a [`CodeGraph`] from C++ source.

use std::collections::HashMap;
use std::path::Path;

use lcw_core::{
    AdapterError, CodeGraph, Edge, EdgeKind, FileId, Node, NodeFlags, NodeId, NodeKind, NodeStats,
    SourceFile, SourceSpan,
};
use tree_sitter::{Node as TsNode, Parser, Tree};

/// Parse a batch of C++ files into a single call graph.
pub fn parse_cpp(files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .map_err(|e| AdapterError::Other(format!("failed to load C++ grammar: {e}")))?;

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
            "namespace_definition" => {
                let name = field_text(child, "name", text);
                let inner = if name.is_empty() {
                    scope.to_string()
                } else {
                    join(scope, &name)
                };
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &inner);
                }
            }
            "class_specifier" | "struct_specifier" => {
                let name = type_name(child, text);
                if name.is_empty() {
                    collect_defs(graph, by_short, child, text, file, scope);
                    continue;
                }
                let inner = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &inner);
                }
            }
            "function_definition" => {
                let name = function_name(child, text);
                if name.is_empty() {
                    continue;
                }
                let owner = if is_method_def(child) {
                    scope.to_string()
                } else {
                    scope.to_string()
                };
                let qualified = if is_method_def(child) {
                    join(scope, &name)
                } else {
                    join(scope, &name)
                };
                let is_method = is_method_def(child);
                let id = add_function_node(
                    graph,
                    child,
                    text,
                    file,
                    &owner,
                    &qualified,
                    is_method,
                );
                by_short.entry(name).or_default().push(id);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_defs(graph, by_short, body, text, file, &qualified);
                }
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
    let flags = compute_flags(&name, is_method);
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

fn compute_flags(name: &str, is_method: bool) -> NodeFlags {
    NodeFlags {
        is_pub: true,
        is_async: false,
        is_unsafe: false,
        is_test: name.starts_with("test") || name.starts_with("Test"),
        is_method,
        is_generic: false,
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
        if is_nested_def(child) {
            continue;
        }
        let kind = child.kind();
        let mut d = depth;
        match kind {
            "if_statement" | "switch_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "for_statement" | "while_statement" | "do_statement" => {
                acc.decision_points += 1;
                d += 1;
            }
            "return_statement" => acc.returns += 1,
            "declaration" | "expression_statement" => acc.statements += 1,
            "call_expression" => {
                if let Some(call) = callee_of_call(child, text) {
                    if matches!(call.short.as_str(), "new" | "make_unique" | "make_shared") {
                        acc.allocations += 1;
                    }
                }
            }
            _ => {}
        }
        if d > acc.max_nesting {
            acc.max_nesting = d;
        }
        scan_stats(child, text, d, acc);
    }
}

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
            "namespace_definition" => {
                let name = field_text(child, "name", text);
                let inner = if name.is_empty() {
                    scope.to_string()
                } else {
                    join(scope, &name)
                };
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls(graph, by_short, body, text, file, &inner);
                }
            }
            "class_specifier" | "struct_specifier" => {
                let name = type_name(child, text);
                if !name.is_empty() {
                    let inner = join(scope, &name);
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_calls(graph, by_short, body, text, file, &inner);
                    }
                } else {
                    collect_calls(graph, by_short, child, text, file, scope);
                }
            }
            "function_definition" => {
                let name = function_name(child, text);
                if name.is_empty() {
                    continue;
                }
                let qualified = join(scope, &name);
                if let Some(body) = child.child_by_field_name("body") {
                    if let Some(caller) = graph.node_by_qualified(&qualified) {
                        let caller_mod = graph.node(caller).module_path.clone();
                        scan_calls(graph, by_short, body, text, file, caller, &caller_mod);
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
        let cands: Vec<NodeId> = cands
            .iter()
            .copied()
            .filter(|&id| {
                let def = graph.node(id);
                can_name(call.kind, def)
                    || (call.kind == CallClass::Direct
                        && def.flags.is_method
                        && def.module_path == caller_module)
            })
            .collect();
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

fn can_name(call: CallClass, def: &Node) -> bool {
    call != CallClass::Direct || !def.flags.is_method
}

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
    short: String,
}

fn callee_of_call(call: TsNode, text: &str) -> Option<Callee> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" | "field_identifier" => Some(Callee {
            kind: CallClass::Direct,
            short: node_text(func, text),
        }),
        "field_expression" => {
            let short = func
                .child_by_field_name("field")
                .map(|a| node_text(a, text))
                .unwrap_or_default();
            Some(Callee {
                kind: CallClass::Method,
                short,
            })
        }
        "qualified_identifier" | "scoped_identifier" => {
            let name = node_text(func, text);
            Some(Callee {
                kind: CallClass::Direct,
                short: last_segment(name.replace("::", "::").as_str()),
            })
        }
        _ => Some(Callee {
            kind: CallClass::Direct,
            short: last_segment(&node_text(func, text)),
        }),
    }
}

fn is_method_def(func: TsNode) -> bool {
    let mut cur = func.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_specifier" | "struct_specifier" => return true,
            "function_definition" | "translation_unit" | "namespace_definition" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

fn is_nested_def(node: TsNode) -> bool {
    matches!(
        node.kind(),
        "function_definition" | "class_specifier" | "struct_specifier"
    )
}

fn function_name(func: TsNode, text: &str) -> String {
    func.child_by_field_name("declarator")
        .map(|d| name_from_declarator(d, text))
        .unwrap_or_default()
}

fn name_from_declarator(decl: TsNode, text: &str) -> String {
    match decl.kind() {
        "identifier" | "field_identifier" | "destructor_name" | "operator_name" => {
            node_text(decl, text)
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "abstract_function_declarator" => decl
            .child_by_field_name("declarator")
            .map(|d| name_from_declarator(d, text))
            .unwrap_or_default(),
        "qualified_identifier" | "scoped_identifier" => {
            last_segment(node_text(decl, text).replace("::", "::").as_str())
        }
        _ => {
            let mut cursor = decl.walk();
            for child in decl.named_children(&mut cursor) {
                let name = name_from_declarator(child, text);
                if !name.is_empty() {
                    return name;
                }
            }
            String::new()
        }
    }
}

fn type_name(spec: TsNode, text: &str) -> String {
    spec.child_by_field_name("name")
        .map(|n| node_text(n, text))
        .unwrap_or_default()
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

fn count_params(func: TsNode) -> u32 {
    let Some(declarator) = func.child_by_field_name("declarator") else {
        return 0;
    };
    let Some(params) = find_parameter_list(declarator) else {
        return 0;
    };
    let mut c = params.walk();
    params
        .named_children(&mut c)
        .filter(|p| !matches!(p.kind(), "comment"))
        .count() as u32
}

fn find_parameter_list(mut node: TsNode) -> Option<TsNode> {
    loop {
        if node.kind() == "parameter_list" {
            return Some(node);
        }
        if let Some(inner) = node.child_by_field_name("declarator") {
            node = inner;
            continue;
        }
        if let Some(inner) = node.child_by_field_name("parameters") {
            return Some(inner);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_parameter_list(child) {
                return Some(found);
            }
        }
        return None;
    }
}

fn last_segment(path: &str) -> String {
    path.rsplit(['.', ':', '>', '<'])
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

/// Derive a module path from a file path, e.g. `src/foo/bar.cpp` -> `src::foo::bar`.
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
    comps[last] = strip_cpp_ext(&comps[last]).to_string();
    comps
        .iter()
        .map(|c| sanitize(c))
        .collect::<Vec<_>>()
        .join("::")
}

fn strip_cpp_ext(name: &str) -> &str {
    for ext in [
        ".cpp", ".cxx", ".cc", ".c++", ".hpp", ".hxx", ".hh", ".h",
    ] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem;
        }
    }
    name
}

fn sanitize(s: &str) -> String {
    s.replace(['-', '.', ' '], "_")
}
