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
//! Heuristic, but precision first: a method call binds by its receiver's type
//! when [`infer`] can tell it, and otherwise only to a method whose name is
//! unique in the repo and not one every std type has (see
//! [`is_ubiquitous_method`]). A wrong edge is worse than a missing one.

mod infer;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lcw_core::{
    AdapterError, CallKind, CodeGraph, Edge, EdgeKind, FileFragment, FileId, Node, NodeFlags,
    NodeId, NodeKind, NodeStats, RawCall, SourceFile, SourceSpan,
};
use tree_sitter::{Node as TsNode, Parser};

use infer::{FileTypes, LocalTypes};

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
    let mut spawned = Vec::new();
    let mut taken = HashSet::new();
    let file_types = FileTypes::collect(tree.root_node(), &sf.text);
    let mut scan = Scan {
        calls: &mut calls,
        spawned: &mut spawned,
        taken: &mut taken,
        file_types: &file_types,
    };
    collect_calls_into(&mut scan, tree.root_node(), &sf.text, &module_base, None);
    // Spawned closures are definitions too: resolution interns them with the
    // rest, so their spawn edge and their own calls link like any other.
    defs.extend(spawned);

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
    // Every definition's module path keyed by its last segment: for methods,
    // the impl/trait type that owns them.
    let mut owners: HashMap<String, HashSet<String>> = HashMap::new();
    for frag in fragments {
        let file = graph.intern_file(frag.path.clone());
        for def in &frag.defs {
            let mut node = def.clone();
            node.span = with_file(node.span, file);
            let name = node.name.clone();
            let id = graph.add_node(node);
            by_short.entry(name).or_default().push(id);
            let module = &graph.node(id).module_path;
            if let Some(owner) = module.rsplit("::").next() {
                owners
                    .entry(owner.to_string())
                    .or_default()
                    .insert(module.clone());
            }
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
            let caller_node = graph.node(caller);
            let (name, module) = (
                caller_node.qualified_name.clone(),
                caller_node.module_path.clone(),
            );
            let scope = CallerScope {
                name: &name,
                module: &module,
            };
            if rc.kind == CallKind::Spawn {
                if let Some(target) = resolve_spawn(&graph, &by_short, rc, &scope) {
                    let call_site = with_file(rc.call_site, file);
                    graph.add_edge(caller, target, Edge::new(EdgeKind::Spawn, call_site));
                }
                continue;
            }
            let callee = Callee {
                kind: callkind_to_class(rc.kind),
                path: rc.path.clone(),
                short: rc.short.clone(),
                receiver: rc.receiver.clone(),
            };
            let target = resolve_target(&mut graph, &by_short, &owners, &callee, &scope);
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
        span: span_of_definition(func, FileId(0)),
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
        is_exported: is_exported_fn(func, text, header),
    }
}

/// Is this definition reachable from outside Rust, through a foreign ABI?
///
/// Two independent signals, either of which is enough:
///
/// * an `#[no_mangle]` / `#[export_name = "..."]` attribute, which exists only
///   so a foreign caller can find the symbol by name (Rust 2024's
///   `#[unsafe(no_mangle)]` is the same attribute and matches too);
/// * an explicit `extern "C"`-style ABI in the signature, which exists only so
///   a foreign caller can call it (directly, or as a callback it was handed).
///
/// A body is required, so the `fn` declarations inside an `extern` block —
/// which are *imports*, the opposite direction — never qualify.
fn is_exported_fn(func: TsNode, text: &str, header: &str) -> bool {
    if func.child_by_field_name("body").is_none() {
        return false;
    }
    if header.contains("extern") {
        return true;
    }
    // Attributes sit above the `fn`, possibly behind doc comments. Only
    // attribute nodes are inspected, so prose mentioning `#[no_mangle]` in a
    // doc comment cannot trigger this.
    let mut sib = func.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                let t = node_text(s, text);
                if t.contains("no_mangle") || t.contains("export_name") {
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

/// Where a call scan puts what it finds: call sites, synthetic nodes for
/// spawned closures, and the qualified names already given to those nodes.
struct Scan<'a> {
    calls: &'a mut Vec<RawCall>,
    spawned: &'a mut Vec<Node>,
    taken: &'a mut HashSet<String>,
    file_types: &'a FileTypes,
}

/// `self_ty` is the type of the `impl`/`trait` block the items sit directly
/// in; a nested `fn` cannot see its outer impl's `Self`, so it is reset there.
fn collect_calls_into(
    scan: &mut Scan,
    node: TsNode,
    text: &str,
    scope: &str,
    self_ty: Option<&str>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let inner = join(scope, &field_text(child, "name", text));
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(scan, body, text, &inner, None);
                }
            }
            "impl_item" => {
                let ty = impl_type_name(child, text);
                let inner = if ty.is_empty() {
                    scope.to_string()
                } else {
                    join(scope, &ty)
                };
                let self_ty = (!ty.is_empty()).then_some(ty.as_str());
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(scan, body, text, &inner, self_ty);
                }
            }
            "trait_item" => {
                let name = field_text(child, "name", text);
                let inner = join(scope, &name);
                let self_ty = (!name.is_empty()).then_some(name.as_str());
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls_into(scan, body, text, &inner, self_ty);
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
                    let locals = local_bindings(child, text);
                    let types = LocalTypes::for_function(child, text, scan.file_types, self_ty);
                    let env = Env {
                        locals: &locals,
                        types: &types,
                    };
                    scan_calls_into(scan, body, text, &qualified, &env);
                    collect_calls_into(scan, body, text, &qualified, None);
                }
            }
            _ => collect_calls_into(scan, child, text, scope, self_ty),
        }
    }
}

/// What a function body binds: every local name (for spawn arguments) and
/// the types of those that can be inferred (for method receivers).
struct Env<'a> {
    locals: &'a HashSet<String>,
    types: &'a LocalTypes<'a>,
}

fn scan_calls_into(scan: &mut Scan, node: TsNode, text: &str, caller: &str, env: &Env) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // Nested functions are their own callers; their bodies are handled by
        // `collect_calls_into`, not folded into this caller.
        if matches!(kind, "function_item" | "function_signature_item") {
            continue;
        }
        if matches!(kind, "call_expression" | "macro_invocation") {
            if let Some(call) = callee_of_call(child, text).filter(|c| !calls_a_local(c, env.types))
            {
                let spawns = call.kind != CallClass::Macro && is_spawn_name(&call.short);
                let is_macro = call.kind == CallClass::Macro;
                let receiver = receiver_type(child, &call, text, env.types);
                scan.calls.push(RawCall {
                    caller: caller.to_string(),
                    kind: class_to_callkind(call.kind),
                    path: call.path,
                    short: call.short,
                    receiver,
                    call_site: span_of(child, FileId(0)),
                });
                if is_macro {
                    scan_macro_args(scan, child, text, caller, env.types);
                }
                if spawns && scan_spawn(scan, child, text, caller, env) {
                    continue;
                }
            }
        }
        scan_calls_into(scan, child, text, caller, env);
    }
}

/// `f(..)` where `f` is a local closure or function value: a call, but not
/// of any definition in the graph.
fn calls_a_local(call: &Callee, types: &LocalTypes) -> bool {
    call.kind == CallClass::Direct
        && !call.path.contains("::")
        && types.is_callable_local(&call.path)
}

/// The receiver type of a call, when it can be inferred: the value in front
/// of `x.m()`, or the impl type behind `Self::f()`.
fn receiver_type(call: TsNode, callee: &Callee, text: &str, types: &LocalTypes) -> Option<String> {
    match callee.kind {
        CallClass::Method => {
            let func = call.child_by_field_name("function")?;
            let func = if func.kind() == "generic_function" {
                func.child_by_field_name("function")?
            } else {
                func
            };
            types.type_of(func.child_by_field_name("value")?, text)
        }
        CallClass::Associated => self_assoc_receiver(&callee.path, types),
        CallClass::Direct | CallClass::Macro => None,
    }
}

fn self_assoc_receiver(path: &str, types: &LocalTypes) -> Option<String> {
    let rest = path.strip_prefix("Self::")?;
    (!rest.contains("::"))
        .then(|| types.self_ty().map(str::to_string))
        .flatten()
}

// ---------------------------------------------------------------------------
// Calls inside macro arguments
// ---------------------------------------------------------------------------

/// Macro arguments are an unparsed token tree, so `assert_eq!(slugify(x), ..)`
/// holds no call expression and its call would be lost. Recognize the plain
/// call shapes in the tokens — `f(`, `a::b::f(`, `Self::f(`, and `.m(` when
/// the receiver is a lone `self` or a local of known type — at any depth,
/// including inside nested macros. A method on an unknown receiver is not
/// recorded at all: macro arguments are mostly assertions about other
/// libraries' values (`assert_eq!(res.status(), 200)`), where even a unique
/// method name is no evidence. Anything subtler stays unseen too: a missed
/// call costs an edge, a misread one would invent it.
fn scan_macro_args(scan: &mut Scan, mac: TsNode, text: &str, caller: &str, types: &LocalTypes) {
    let mut cursor = mac.walk();
    let trees: Vec<TsNode> = mac
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "token_tree")
        .collect();
    for tt in trees {
        scan_token_tree(scan, tt, text, caller, types);
    }
}

fn scan_token_tree(scan: &mut Scan, tt: TsNode, text: &str, caller: &str, types: &LocalTypes) {
    let mut cursor = tt.walk();
    let toks: Vec<TsNode> = tt.children(&mut cursor).collect();
    let kind_at = |i: usize| toks.get(i).map(|t| t.kind());
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        if tok.kind() == "token_tree" {
            scan_token_tree(scan, tok, text, caller, types);
            i += 1;
            continue;
        }
        let is_head = matches!(tok.kind(), "identifier" | "self" | "super" | "crate")
            || (tok.kind() == "primitive_type" && kind_at(i + 1) == Some("::"));
        // A path segment after `::` was consumed with its head; after `fn` it
        // is a declaration, after `'` a lifetime.
        let prev = i.checked_sub(1).and_then(kind_at);
        if !is_head || matches!(prev, Some("::" | "fn" | "'")) {
            i += 1;
            continue;
        }
        let mut segs = vec![node_text(tok, text)];
        let mut j = i;
        while kind_at(j + 1) == Some("::") && kind_at(j + 2) == Some("identifier") {
            segs.push(node_text(toks[j + 2], text));
            j += 2;
        }
        let args = toks.get(j + 1).copied();
        let is_call = args
            .is_some_and(|a| a.kind() == "token_tree" && text[a.start_byte()..].starts_with('('));
        if is_call
            && segs
                .last()
                .is_some_and(|s| s.chars().next().is_some_and(is_ident_start))
        {
            let short = segs.last().cloned().unwrap_or_default();
            let path = segs.join("::");
            let found = if prev == Some(".") {
                let receiver = token_receiver(&toks, i, text, types);
                (segs.len() == 1 && receiver.is_some()).then_some((CallClass::Method, receiver))
            } else if segs.len() == 1 {
                (tok.kind() == "identifier" && !types.is_callable_local(&short))
                    .then_some((CallClass::Direct, None))
            } else {
                Some((CallClass::Associated, self_assoc_receiver(&path, types)))
            };
            let Some((kind, receiver)) = found else {
                i = j + 1;
                continue;
            };
            let end = args.map_or(tok, |a| a);
            scan.calls.push(RawCall {
                caller: caller.to_string(),
                kind: class_to_callkind(kind),
                path: if kind == CallClass::Method {
                    short.clone()
                } else {
                    path
                },
                short,
                receiver,
                call_site: span_between(tok, end, FileId(0)),
            });
        }
        i = j + 1;
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

/// The receiver of `.m(` at `toks[dot + 1]`: only the simplest shapes, a lone
/// `self` or local name directly before the dot (`self.m(`, `x.m(`).
fn token_receiver(
    toks: &[TsNode],
    name_at: usize,
    text: &str,
    types: &LocalTypes,
) -> Option<String> {
    let recv = toks.get(name_at.checked_sub(2)?)?;
    let before = name_at
        .checked_sub(3)
        .and_then(|k| toks.get(k))
        .map(|t| t.kind());
    if matches!(before, Some("." | "::")) {
        return None;
    }
    match recv.kind() {
        "self" => types.self_ty().map(str::to_string),
        "identifier" => types.var(&node_text(*recv, text)).map(str::to_string),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Spawn sites: where a new thread of control begins
// ---------------------------------------------------------------------------

/// Names that start work on another thread or task: `spawn` and the
/// `spawn_*` family (`spawn_blocking`, `spawn_local`, `spawn_scoped`, and a
/// project's own `spawn_worker`). That covers `std::thread::spawn`,
/// `Builder::spawn`, `Scope::spawn`, `tokio::spawn`, `task::spawn_blocking`,
/// rayon's `spawn` and custom runtimes alike.
///
/// A name alone is not enough evidence, and nothing here relies on it alone:
/// a spawn site only produces an edge when an argument is something that can
/// run — a closure, an async block, or a path that resolves to a definition.
/// `Command::new("ls").spawn()` has no such argument and yields nothing, and
/// Wari's kernel `spawn(slot)` passes a variable, which is filtered out below.
fn is_spawn_name(short: &str) -> bool {
    short == "spawn" || short.starts_with("spawn_")
}

/// Handle a spawn call's subtree: calls in the callee expression stay with
/// the caller (`Builder::new().name(..)` in a builder chain), a closure or
/// async block argument becomes its own node, and a function path becomes a
/// spawn reference. Returns `false` when there is no argument list, so the
/// caller scans the node normally.
fn scan_spawn(scan: &mut Scan, call: TsNode, text: &str, caller: &str, env: &Env) -> bool {
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    if let Some(func) = call.child_by_field_name("function") {
        scan_calls_into(scan, func, text, caller, env);
    }
    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        match arg.kind() {
            "closure_expression" | "async_block" => spawn_closure(scan, arg, text, caller, env),
            // A bare name that is a parameter or a `let` binding is a value —
            // `spawn(slot)` with `slot: u8` — not a function to run.
            "identifier" if env.locals.contains(node_text(arg, text).as_str()) => {}
            "identifier" | "scoped_identifier" => spawn_reference(scan, arg, text, caller),
            _ => scan_calls_into(scan, arg, text, caller, env),
        }
    }
    true
}

/// Give a spawned closure or async block its own node, spawned by `caller`,
/// and credit the calls in its body to it rather than to `caller`: the body
/// runs on the new thread, not on the caller's.
///
/// It is named for where it is — `<spawned@L42>` — and nested under its
/// caller exactly as a nested `fn` would be (`module_path` is the caller), so
/// the outline shows it inside the function that starts it. The angle
/// brackets keep the name out of reach of ordinary call resolution.
fn spawn_closure(scan: &mut Scan, closure: TsNode, text: &str, caller: &str, outer: &Env) {
    let line = closure.start_position().row + 1;
    let base = format!("<spawned@L{line}>");
    let mut name = base.clone();
    let mut n = 1;
    while !scan.taken.insert(join(caller, &name)) {
        n += 1;
        name = format!("{base}#{n}");
    }
    let qualified = join(caller, &name);

    let is_closure = closure.kind() == "closure_expression";
    let body = if is_closure {
        closure.child_by_field_name("body")
    } else {
        Some(closure)
    };
    let mut acc = StatAcc::default();
    if let Some(body) = body {
        scan_stats(body, text, 0, &mut acc);
    }
    let parameters = closure
        .child_by_field_name("parameters")
        .map_or(0, |p| p.named_child_count() as u32);
    let start = closure.start_position().row as u32;
    let end = closure.end_position().row as u32;
    scan.spawned.push(Node {
        id: NodeId(0),
        name: name.clone(),
        qualified_name: qualified.clone(),
        module_path: caller.to_string(),
        kind: NodeKind::Closure,
        span: span_of(closure, FileId(0)),
        flags: NodeFlags {
            is_async: !is_closure || node_text(closure, text).starts_with("async"),
            ..Default::default()
        },
        stats: NodeStats {
            lines_of_code: end.saturating_sub(start) + 1,
            decision_points: acc.decision_points,
            parameters,
            statements: acc.statements,
            max_nesting: acc.max_nesting,
            returns: acc.returns,
            unsafe_blocks: acc.unsafe_blocks,
            allocations: acc.allocations,
            awaits: acc.awaits,
        },
    });
    scan.calls.push(RawCall {
        caller: caller.to_string(),
        kind: CallKind::Spawn,
        path: qualified.clone(),
        short: name,
        receiver: None,
        call_site: span_of(closure, FileId(0)),
    });

    // Captured variables are still values inside the closure.
    let mut locals = outer.locals.clone();
    if let Some(params) = closure.child_by_field_name("parameters") {
        identifiers_in(params, text, &mut locals);
    }
    if let Some(body) = body {
        bindings_in(body, text, &mut locals);
    }
    // The enclosing function's type environment already covers the closure:
    // its bindings were collected from the whole body.
    let env = Env {
        locals: &locals,
        types: outer.types,
    };
    // Scan the closure node, not its body: the scan looks at a node's
    // children, and in `move || worker(rx)` the body *is* the call, so
    // scanning the body alone would lose the thread's only call.
    scan_calls_into(scan, closure, text, &qualified, &env);
}

/// `spawn(worker)` / `spawn(workers::run)`: the function is passed, not
/// called, so without this there is no edge at all and `worker` looks like
/// dead code. Resolution decides whether it names a real definition.
fn spawn_reference(scan: &mut Scan, arg: TsNode, text: &str, caller: &str) {
    let path = node_text(arg, text);
    scan.calls.push(RawCall {
        caller: caller.to_string(),
        kind: CallKind::Spawn,
        short: last_segment(&path),
        path,
        receiver: None,
        call_site: span_of(arg, FileId(0)),
    });
}

/// Names bound inside a function: its parameters and every pattern in its
/// body (`let`, `for`, `if let`, match arms, closure parameters), stopping at
/// nested functions. Over-approximate on purpose — treating a name as local
/// only ever withholds a spawn edge, never invents one.
fn local_bindings(func: TsNode, text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(params) = func.child_by_field_name("parameters") {
        identifiers_in(params, text, &mut out);
    }
    if let Some(body) = func.child_by_field_name("body") {
        bindings_in(body, text, &mut out);
    }
    out
}

fn bindings_in(node: TsNode, text: &str, out: &mut HashSet<String>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_item" => continue,
            "let_declaration" | "let_condition" | "for_expression" => {
                if let Some(pat) = child.child_by_field_name("pattern") {
                    identifiers_in(pat, text, out);
                }
            }
            "closure_parameters" | "match_pattern" => identifiers_in(child, text, out),
            _ => {}
        }
        bindings_in(child, text, out);
    }
}

fn identifiers_in(node: TsNode, text: &str, out: &mut HashSet<String>) {
    if node.kind() == "identifier" {
        out.insert(node_text(node, text));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        identifiers_in(child, text, out);
    }
}

/// Resolve a spawn target to a definition, or to nothing — never to an
/// external placeholder, since a thread root that is not really there would
/// be an invented one.
///
/// A spawned closure matches its synthetic node exactly. A path must match
/// its qualifiers, as a qualified call must. A bare name is weaker evidence
/// than a call (it may be a variable the binding scan did not see), so it
/// only binds within the caller's own crate.
fn resolve_spawn(
    graph: &CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    rc: &RawCall,
    scope: &CallerScope,
) -> Option<NodeId> {
    if let Some(id) = graph.node_by_qualified(&rc.path) {
        return (graph.node(id).kind != NodeKind::External).then_some(id);
    }
    let qualifiers = path_qualifiers(&rc.path);
    let caller_crate = scope.module.split("::").next().unwrap_or("");
    let eligible: Vec<NodeId> = by_short
        .get(&rc.short)?
        .iter()
        .copied()
        .filter(|&id| {
            let n = graph.node(id);
            if !rc.path.contains("::") {
                // Passed by bare name, like a bare call: a free function only.
                can_name(CallClass::Direct, n)
                    && n.module_path.split("::").next() == Some(caller_crate)
            } else if qualifiers.is_empty() {
                n.module_path.split("::").next() == Some(caller_crate)
            } else {
                has_qualifiers(&n.qualified_name, &qualifiers)
            }
        })
        .collect();
    pick_nearest(graph, &eligible, scope)
}

fn resolve_target(
    graph: &mut CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    owners: &HashMap<String, HashSet<String>>,
    call: &Callee,
    scope: &CallerScope,
) -> NodeId {
    // Macros are not functions, and no definition in the graph is a macro, so
    // a macro call is always external — even when a function shares its name
    // (`matches!` is not `fn matches`).
    if call.kind == CallClass::Macro {
        let key = call.path.clone();
        return graph.intern_node(&key, || Node::external(key.clone()));
    }
    // Exact qualified match wins (helps `Type::assoc` and scoped paths).
    if call.path.contains("::") {
        if let Some(id) = graph.node_by_qualified(&call.path) {
            return id;
        }
    }
    if call.kind == CallClass::Method {
        if let Some(id) = resolve_method(graph, by_short, owners, call, scope) {
            return id;
        }
    } else if let Some(cands) = by_short.get(&call.short) {
        let mut qualifiers = path_qualifiers(&call.path);
        // `Self::f()` is `T::f()` for the impl's `T`.
        if let Some(ty) = &call.receiver {
            qualifiers.push(ty);
        }
        // A type- or module-qualified call (`Calc::new`, `math::sum`) may only
        // bind to a definition whose path actually contains that qualifier.
        // Otherwise the qualifier names something outside the analyzed code
        // (`Vec::new`, `Cli::parse`) and binding it to an unrelated same-named
        // def would invent an edge — which is exactly what makes flow traces
        // lie. Relative heads (`Self::`, `self::`, `super::`, `crate::`) carry
        // no such information and resolve like a plain call.
        let eligible: Vec<NodeId> = cands
            .iter()
            .copied()
            .filter(|&id| {
                let n = graph.node(id);
                can_name(call.kind, n) && has_qualifiers(&n.qualified_name, &qualifiers)
            })
            .collect();
        if let Some(id) = pick_nearest(graph, &eligible, scope) {
            return id;
        }
    }
    let key = if call.path.is_empty() {
        call.short.clone()
    } else {
        call.path.clone()
    };
    graph.intern_node(&key, || Node::external(key.clone()))
}

/// Bind `x.m()` to a method definition, or to nothing.
///
/// With a known receiver type `T`, only a method declared directly in an
/// `impl T` / `impl Trait for T` / `trait T` block qualifies; when `T` owns
/// nothing in the repo it is foreign and nothing does. Otherwise — receiver
/// unknown, or a repo `T` that declares no `m` (a trait's default method) —
/// a name is evidence only when exactly one method in
/// the whole repo has it *and* it is not a name the standard library puts on
/// everything (`expect`, `map`, `get`, ...): `opt.map(..)` must not become a
/// call to the one `Camera::map` in the repo.
fn resolve_method(
    graph: &CodeGraph,
    by_short: &HashMap<String, Vec<NodeId>>,
    owners: &HashMap<String, HashSet<String>>,
    call: &Callee,
    scope: &CallerScope,
) -> Option<NodeId> {
    let methods: Vec<NodeId> = by_short
        .get(&call.short)?
        .iter()
        .copied()
        .filter(|&id| can_name(CallClass::Method, graph.node(id)))
        .collect();
    if let Some(ty) = &call.receiver {
        // A path-qualified type (`reqwest::Client`) must match its qualifiers
        // too, exactly as a `Type::f()` call must.
        let name = ty.rsplit("::").next().unwrap_or(ty);
        let qualifiers = path_qualifiers(ty);
        let own: Vec<NodeId> = methods
            .iter()
            .copied()
            .filter(|&id| {
                let n = graph.node(id);
                n.module_path.rsplit("::").next() == Some(name)
                    && has_qualifiers(&n.qualified_name, &qualifiers)
            })
            .collect();
        if let Some(id) = pick_unambiguous(graph, &own, scope) {
            return Some(id);
        }
        // A type that owns no definition here is another library's
        // (`res: Response`), or a generic parameter: no repo method is its.
        let known = owners
            .get(name)
            .is_some_and(|ms| ms.iter().any(|m| has_qualifiers(m, &qualifiers)));
        if !known {
            return None;
        }
    }
    if is_ubiquitous_method(&call.short) {
        return None;
    }
    match methods.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Like [`pick_nearest`], but where it would fall back to "the first
/// declared" among candidates in other crates, refuse instead: two
/// same-named types elsewhere are two different answers.
fn pick_unambiguous(graph: &CodeGraph, cands: &[NodeId], scope: &CallerScope) -> Option<NodeId> {
    if cands.len() <= 1 {
        return cands.first().copied();
    }
    let caller_crate = scope.module.split("::").next().unwrap_or("");
    let in_crate = cands
        .iter()
        .any(|&id| graph.node(id).module_path.split("::").next() == Some(caller_crate));
    if in_crate {
        pick_nearest(graph, cands, scope)
    } else {
        None
    }
}

/// Method names so common on std types — `Option`, `Result`, iterators,
/// collections, strings, smart pointers, I/O, sync primitives and the derive
/// traits — that a call by name alone says nothing about which type's method
/// runs. Without a known receiver type these never bind to a repo method.
fn is_ubiquitous_method(name: &str) -> bool {
    matches!(
        name,
        // Option / Result
        "expect" | "expect_err" | "unwrap" | "unwrap_err" | "unwrap_or" | "unwrap_or_else"
            | "unwrap_or_default" | "map" | "map_err" | "map_or" | "map_or_else" | "and_then"
            | "or_else" | "and" | "or" | "ok" | "err" | "ok_or" | "ok_or_else" | "is_some"
            | "is_none" | "is_ok" | "is_err" | "take" | "replace" | "filter" | "flatten"
            | "copied" | "cloned" | "as_deref" | "as_deref_mut" | "zip" | "unzip" | "inspect"
            | "transpose" | "get_or_insert" | "get_or_insert_with"
            // Iterators
            | "iter" | "iter_mut" | "into_iter" | "next" | "next_back" | "collect" | "fold"
            | "for_each" | "find" | "find_map" | "filter_map" | "flat_map" | "any" | "all"
            | "count" | "sum" | "product" | "min" | "max" | "min_by" | "max_by" | "min_by_key"
            | "max_by_key" | "position" | "enumerate" | "rev" | "skip" | "step_by" | "chain"
            | "peekable" | "peek" | "last" | "nth" | "windows" | "chunks" | "scan"
            | "take_while" | "skip_while" | "partition" | "by_ref"
            // Collections / slices / strings
            | "get" | "get_mut" | "insert" | "remove" | "push" | "push_str" | "push_back"
            | "push_front" | "pop" | "pop_back" | "pop_front" | "len" | "is_empty" | "clear"
            | "contains" | "contains_key" | "extend" | "append" | "truncate" | "retain"
            | "drain" | "sort" | "sort_by" | "sort_by_key" | "sort_unstable" | "dedup"
            | "reverse" | "first" | "split" | "split_at" | "splitn" | "join" | "concat"
            | "entry" | "or_insert" | "or_insert_with" | "or_default" | "keys" | "values"
            | "values_mut" | "starts_with" | "ends_with" | "trim" | "lines" | "chars"
            | "bytes" | "as_bytes" | "parse" | "with_capacity" | "capacity" | "reserve"
            | "resize" | "swap" | "fill" | "binary_search" | "to_lowercase" | "to_uppercase"
            | "strip_prefix" | "strip_suffix" | "char_indices" | "split_whitespace"
            | "to_vec" | "as_slice" | "as_mut_slice" | "set"
            // Conversions / derive & std traits
            | "clone" | "clone_from" | "to_string" | "to_owned" | "as_ref" | "as_mut"
            | "as_str" | "into" | "try_into" | "from" | "try_from" | "borrow" | "borrow_mut"
            | "deref" | "deref_mut" | "default" | "new" | "fmt" | "eq" | "ne" | "cmp"
            | "partial_cmp" | "hash" | "drop" | "build"
            // I/O, sync, async
            | "write" | "write_all" | "write_str" | "write_fmt" | "read" | "read_to_string"
            | "read_to_end" | "read_line" | "flush" | "send" | "recv" | "try_recv" | "lock"
            | "try_lock" | "read_lock" | "write_lock" | "load" | "store" | "close"
            | "await" | "poll" | "wait" | "notify_one" | "notify_all" | "call" | "run"
            | "start" | "stop" | "open"
            // HTTP requests / responses, paths, time
            | "status" | "headers" | "header" | "body" | "json" | "text" | "query" | "method"
            | "uri" | "url" | "path" | "to_str" | "display" | "exists" | "is_file" | "is_dir"
            | "file_name" | "extension" | "parent" | "elapsed" | "as_secs" | "as_millis"
    )
}

/// The qualifying segments of a call path (`Calc::new` -> `["Calc"]`,
/// `a::b::f` -> `["a", "b"]`), dropping relative heads that name no scope.
fn path_qualifiers(path: &str) -> Vec<&str> {
    let mut segs: Vec<&str> = path.split("::").collect();
    segs.pop(); // the short name itself
    segs.into_iter()
        .filter(|s| !s.is_empty() && !matches!(*s, "Self" | "self" | "super" | "crate"))
        .collect()
}

/// Does every qualifier appear as a segment of `qualified_name`? Segment-wise
/// (not substring) so `A` never matches `AB`, and order-insensitive so re-export
/// paths (`lcw_query::resolve` for `lcw_query::resolve::resolve`) still bind.
fn has_qualifiers(qualified_name: &str, qualifiers: &[&str]) -> bool {
    qualifiers
        .iter()
        .all(|q| qualified_name.split("::").any(|seg| seg == *q))
}

/// Whether a definition can be what a call of this syntactic class names.
/// Rust's call syntax decides it, so a same-named definition of the wrong
/// kind is not evidence at all:
///
/// * `x.f()` reaches only a method — a function taking `self`. (A free `join`
///   is not what `path.join()` calls.)
/// * A bare `f()` reaches only a free function: a method or an associated
///   function needs a `Type::`, `Self::` or receiver in front of it. (`drop(x)`
///   is `std::mem::drop`, not some type's `Drop::drop`.)
/// * `Type::f()` may name either, and is checked by its qualifiers instead.
/// * A macro is not a function; see [`resolve_target`].
fn can_name(call: CallClass, def: &Node) -> bool {
    match call {
        CallClass::Method => def.flags.is_method,
        CallClass::Direct => !def.flags.is_method && !is_associated(def),
        CallClass::Associated => true,
        CallClass::Macro => false,
    }
}

/// Whether a definition sits directly in an `impl` or `trait` block. The
/// module path records such a block by its type or trait name, and Rust's
/// naming rules make those the only UpperCamelCase segments in a path
/// (modules and functions are snake_case).
fn is_associated(def: &Node) -> bool {
    def.module_path
        .rsplit("::")
        .next()
        .and_then(|owner| owner.chars().next())
        .is_some_and(char::is_uppercase)
}

/// Where a call is made from.
struct CallerScope<'a> {
    /// Qualified name of the calling function. Items declared in its body
    /// (`fn outer() { fn helper() {} helper(); }`) have this as their module
    /// path.
    name: &'a str,
    /// The module the calling function sits in.
    module: &'a str,
}

/// Among several candidate definitions, prefer the one closest to the caller,
/// the way Rust's own name lookup does: declared in the caller's body, then
/// in the same module, then in the same crate (first path segment), then the
/// first declared.
fn pick_nearest(graph: &CodeGraph, cands: &[NodeId], scope: &CallerScope) -> Option<NodeId> {
    match cands {
        [] => None,
        [only] => Some(*only),
        many => {
            for home in [scope.name, scope.module] {
                if let Some(id) = many
                    .iter()
                    .copied()
                    .find(|&id| graph.node(id).module_path == home)
                {
                    return Some(id);
                }
            }
            let caller_crate = scope.module.split("::").next().unwrap_or("");
            if let Some(id) = many
                .iter()
                .copied()
                .find(|&id| graph.node(id).module_path.split("::").next() == Some(caller_crate))
            {
                return Some(id);
            }
            Some(many[0])
        }
    }
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
        // Spawn sites resolve through `resolve_spawn` before this is reached;
        // the arm exists so the match stays exhaustive.
        CallKind::Direct | CallKind::Spawn => CallClass::Direct,
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
    /// Inferred receiver type; see [`RawCall::receiver`].
    receiver: Option<String>,
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
            receiver: None,
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
                receiver: None,
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
                receiver: None,
            })
        }
        "field_expression" => {
            let field = func.child_by_field_name("field")?;
            let n = node_text(field, text);
            Some(Callee {
                kind: CallClass::Method,
                path: n.clone(),
                short: n,
                receiver: None,
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
                receiver: None,
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

/// Span from the start of `first` to the end of `last`.
fn span_between(first: TsNode, last: TsNode, file: FileId) -> SourceSpan {
    let s = first.start_position();
    let e = last.end_position();
    SourceSpan::new(
        file,
        s.row as u32 + 1,
        s.column as u32,
        e.row as u32 + 1,
        e.column as u32,
    )
}

/// Span for a function/method definition, including attributes and doc comments
/// immediately above the `fn` (tree-sitter keeps those as preceding siblings).
fn span_of_definition(func: TsNode, file: FileId) -> SourceSpan {
    let mut start = func.start_position();
    let mut sib = func.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" | "line_comment" | "block_comment" => {
                start = s.start_position();
            }
            _ => break,
        }
        sib = s.prev_sibling();
    }
    let end = func.end_position();
    SourceSpan::new(
        file,
        start.row as u32 + 1,
        start.column as u32,
        end.row as u32 + 1,
        end.column as u32,
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
