//! Receiver-type hints for method calls (fast mode).
//!
//! `x.expect()` names only a method; without knowing what `x` is, the name
//! alone binds it to every same-named method in the repo (`Result::expect`
//! becomes a call to some parser's `expect`). This module infers the type of
//! a receiver from what one file says, cheaply and conservatively:
//!
//! * `self` (and `Self`) inside an `impl T` / `trait T` block is `T`;
//! * a parameter with an explicit type (`x: &T`, `x: T`, `x: Box<T>`);
//! * `let x: T = ..`, `let x = T { .. }`, `let x = T::new(..)` and other
//!   constructors that return `Self`, `let x = T::open(..)?`;
//! * `self.field` / `x.field` when the struct declares the field's type.
//!
//! A name bound more than once with different (or unknown) types — shadowing,
//! `if let`, `for`, match arms, untyped closure parameters — is unknown. A
//! wrong type would invent an edge; an unknown one only withholds it.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use tree_sitter::Node as TsNode;

use super::{impl_type_name, last_segment, node_text};

/// How an associated function's return type relates to its impl type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ret {
    /// `-> Self` / `-> T`.
    Plain,
    /// `-> Result<Self, _>` / `-> Option<T>`: `Self` after `?` or `.unwrap()`.
    Wrapped,
    /// Anything else.
    Other,
}

/// What one file declares about types: struct field types and the return
/// shape of associated functions. Per-file on purpose, so extraction stays a
/// pure function of one file and its fragment stays cacheable.
#[derive(Debug, Default)]
pub(super) struct FileTypes {
    /// struct name -> field name (or tuple index) -> field type. `None` when
    /// the file declares two structs with that name.
    fields: HashMap<String, Option<HashMap<String, String>>>,
    returns: HashMap<(String, String), Ret>,
}

impl FileTypes {
    pub(super) fn collect(root: TsNode, text: &str) -> Self {
        let mut out = FileTypes::default();
        out.walk(root, text);
        out
    }

    fn walk(&mut self, node: TsNode, text: &str) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "struct_item" => self.add_struct(child, text),
                "impl_item" => self.add_impl(child, text),
                _ => {}
            }
            self.walk(child, text);
        }
    }

    fn add_struct(&mut self, item: TsNode, text: &str) {
        let Some(name) = item.child_by_field_name("name").map(|n| node_text(n, text)) else {
            return;
        };
        let mut fields = HashMap::new();
        if let Some(body) = item.child_by_field_name("body") {
            let mut cursor = body.walk();
            match body.kind() {
                "field_declaration_list" => {
                    for f in body.named_children(&mut cursor) {
                        let (Some(n), Some(t)) =
                            (f.child_by_field_name("name"), f.child_by_field_name("type"))
                        else {
                            continue;
                        };
                        if let Some(ty) = type_name(t, text) {
                            fields.insert(node_text(n, text), ty);
                        }
                    }
                }
                "ordered_field_declaration_list" => {
                    let types = body
                        .named_children(&mut cursor)
                        .filter(|c| !matches!(c.kind(), "visibility_modifier" | "attribute_item"));
                    for (i, t) in types.enumerate() {
                        if let Some(ty) = type_name(t, text) {
                            fields.insert(i.to_string(), ty);
                        }
                    }
                }
                _ => {}
            }
        }
        match self.fields.entry(name) {
            Entry::Vacant(e) => {
                e.insert(Some(fields));
            }
            Entry::Occupied(mut e) => {
                e.insert(None);
            }
        }
    }

    fn add_impl(&mut self, item: TsNode, text: &str) {
        let ty = impl_type_name(item, text);
        let Some(body) = item.child_by_field_name("body") else {
            return;
        };
        if ty.is_empty() {
            return;
        }
        let mut cursor = body.walk();
        for f in body.named_children(&mut cursor) {
            if f.kind() != "function_item" {
                continue;
            }
            let Some(name) = f.child_by_field_name("name").map(|n| node_text(n, text)) else {
                continue;
            };
            let ret = f
                .child_by_field_name("return_type")
                .map_or(Ret::Other, |r| classify_return(r, text, &ty));
            self.returns.entry((ty.clone(), name)).or_insert(ret);
        }
    }

    fn field_type(&self, owner: &str, field: &str) -> Option<String> {
        self.fields
            .get(local_name(owner)?)?
            .as_ref()?
            .get(field)
            .cloned()
    }

    fn returns(&self, owner: &str, name: &str) -> Option<Ret> {
        self.returns
            .get(&(local_name(owner)?.to_string(), name.to_string()))
            .copied()
    }
}

/// The name under which this file would have declared `ty`: `Parser` or
/// `crate::cas::Parser`, but not `reqwest::Client`, which is someone else's.
fn local_name(ty: &str) -> Option<&str> {
    let mut segs = ty.split("::");
    let first = segs.next()?;
    match segs.last() {
        None => Some(first),
        Some(last) if matches!(first, "crate" | "self" | "super") => Some(last),
        Some(_) => None,
    }
}

fn classify_return(ret: TsNode, text: &str, owner: &str) -> Ret {
    let is_self = |t: Option<String>| t.is_some_and(|t| t == "Self" || t == owner);
    if ret.kind() == "generic_type" {
        let base = ret
            .child_by_field_name("type")
            .map(|b| last_segment(&node_text(b, text)));
        if matches!(base.as_deref(), Some("Result" | "Option")) {
            return if is_self(first_type_arg(ret).and_then(|a| type_name(a, text))) {
                Ret::Wrapped
            } else {
                Ret::Other
            };
        }
    }
    if is_self(type_name(ret, text)) {
        Ret::Plain
    } else {
        Ret::Other
    }
}

/// Name of a type as written, path included (`reqwest::Client` stays
/// qualified, so it is never mistaken for a repo's own `Client`), seen
/// through references and the smart pointers that auto-deref to their
/// contents (`&mut Box<Parser>` -> `Parser`). `dyn T` and `impl T` name the
/// trait `T`. Types with no single name (tuples, arrays, fn pointers) yield
/// `None`.
fn type_name(t: TsNode, text: &str) -> Option<String> {
    match t.kind() {
        "type_identifier" | "primitive_type" | "scoped_type_identifier" => Some(node_text(t, text)),
        "reference_type" => type_name(t.child_by_field_name("type")?, text),
        "generic_type" => {
            let base = node_text(t.child_by_field_name("type")?, text);
            if matches!(last_segment(&base).as_str(), "Box" | "Rc" | "Arc") {
                type_name(first_type_arg(t)?, text)
            } else {
                Some(base)
            }
        }
        "dynamic_type" | "abstract_type" => type_name(t.child_by_field_name("trait")?, text),
        _ => None,
    }
}

/// `fn(..)`, `impl Fn(..)`, `&dyn FnMut(..)`, `Box<dyn FnOnce(..)>`.
fn is_callable_type(t: TsNode, text: &str) -> bool {
    if t.kind() == "function_type" {
        return true;
    }
    let s = node_text(t, text);
    ["Fn(", "FnMut(", "FnOnce("].iter().any(|f| s.contains(f))
}

fn first_type_arg(generic: TsNode) -> Option<TsNode> {
    let args = generic.child_by_field_name("type_arguments")?;
    let mut cursor = args.walk();
    let first = args
        .named_children(&mut cursor)
        .find(|a| a.kind() != "lifetime");
    first
}

/// Associated functions that, by near-universal convention, return `Self`.
fn is_plain_ctor(name: &str) -> bool {
    matches!(name, "new" | "default" | "from")
        || name.starts_with("new_")
        || name.starts_with("with_")
}

/// Constructors that conventionally return `Result<Self>` / `Option<Self>`;
/// trusted only after `?`, `.unwrap()` or `.expect(..)`.
fn is_fallible_ctor(name: &str) -> bool {
    is_plain_ctor(name)
        || name.starts_with("from_")
        || name.starts_with("try_")
        || matches!(name, "open" | "load" | "parse" | "connect" | "create")
}

/// The types of the names bound in one function body.
pub(super) struct LocalTypes<'a> {
    file: &'a FileTypes,
    self_ty: Option<String>,
    /// `None` marks a name whose type is unknown or ambiguous.
    vars: HashMap<String, Option<String>>,
    /// Locals that hold something callable — `let f = |..| ..` or a
    /// parameter typed `impl Fn(..)` / `&dyn FnMut(..)` / `fn(..)`. A bare
    /// `f(..)` then calls the local (locals shadow items), never a free `fn f`.
    callables: HashSet<String>,
}

impl<'a> LocalTypes<'a> {
    /// Environment for a function item; `self_ty` is the enclosing impl or
    /// trait type, if any.
    pub(super) fn for_function(
        func: TsNode,
        text: &str,
        file: &'a FileTypes,
        self_ty: Option<&str>,
    ) -> Self {
        let mut env = LocalTypes {
            file,
            self_ty: self_ty.map(str::to_string),
            vars: HashMap::new(),
            callables: HashSet::new(),
        };
        if let Some(params) = func.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for p in params.named_children(&mut cursor) {
                if p.kind() == "parameter" {
                    env.bind_param(p, text);
                }
            }
        }
        if let Some(body) = func.child_by_field_name("body") {
            env.walk(body, text);
        }
        env
    }

    /// The inferred type of `self`, i.e. of the enclosing impl/trait.
    pub(super) fn self_ty(&self) -> Option<&str> {
        self.self_ty.as_deref()
    }

    /// The inferred type of a local name.
    pub(super) fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name)?.as_deref()
    }

    /// Is `name` a local closure or function value, so that `name(..)` is
    /// not a call to any `fn` item?
    pub(super) fn is_callable_local(&self, name: &str) -> bool {
        self.callables.contains(name)
    }

    fn bind(&mut self, name: String, ty: Option<String>) {
        let ty = match ty.as_deref() {
            Some("Self") => self.self_ty.clone(),
            _ => ty,
        };
        match self.vars.entry(name) {
            Entry::Vacant(e) => {
                e.insert(ty);
            }
            Entry::Occupied(mut e) => {
                if *e.get() != ty {
                    e.insert(None);
                }
            }
        }
    }

    fn poison(&mut self, pattern: TsNode, text: &str) {
        if pattern.kind() == "identifier" {
            self.vars.insert(node_text(pattern, text), None);
        }
        let mut cursor = pattern.walk();
        for c in pattern.named_children(&mut cursor) {
            self.poison(c, text);
        }
    }

    /// A typed parameter `x: T` (or `mut x: T`) of a function or closure.
    fn bind_param(&mut self, p: TsNode, text: &str) {
        let Some(pat) = p.child_by_field_name("pattern") else {
            return;
        };
        match simple_binding(pat, text) {
            Some(name) => {
                let ty_node = p.child_by_field_name("type");
                if ty_node.is_some_and(|t| is_callable_type(t, text)) {
                    self.callables.insert(name.clone());
                }
                let ty = ty_node.and_then(|t| type_name(t, text));
                self.bind(name, ty);
            }
            None => self.poison(pat, text),
        }
    }

    fn walk(&mut self, node: TsNode, text: &str) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                // Nested fns get their own environment.
                "function_item" => continue,
                "let_declaration" => {
                    if let Some(pat) = child.child_by_field_name("pattern") {
                        match simple_binding(pat, text) {
                            Some(name) => {
                                let value = child.child_by_field_name("value");
                                let callable = value
                                    .is_some_and(|v| v.kind() == "closure_expression")
                                    || child
                                        .child_by_field_name("type")
                                        .is_some_and(|t| is_callable_type(t, text));
                                if callable {
                                    self.callables.insert(name.clone());
                                }
                                let ty = match child.child_by_field_name("type") {
                                    Some(t) => type_name(t, text),
                                    None => child
                                        .child_by_field_name("value")
                                        .and_then(|v| self.type_of(v, text)),
                                };
                                self.bind(name, ty);
                            }
                            None => self.poison(pat, text),
                        }
                    }
                }
                "let_condition" | "for_expression" => {
                    if let Some(pat) = child.child_by_field_name("pattern") {
                        self.poison(pat, text);
                    }
                }
                "match_pattern" => self.poison(child, text),
                "closure_parameters" => {
                    let mut c = child.walk();
                    for p in child.named_children(&mut c) {
                        if p.kind() == "parameter" {
                            self.bind_param(p, text);
                        } else {
                            self.poison(p, text);
                        }
                    }
                }
                _ => {}
            }
            self.walk(child, text);
        }
    }

    /// The type of an expression, when one file is enough to tell.
    pub(super) fn type_of(&self, expr: TsNode, text: &str) -> Option<String> {
        match expr.kind() {
            "self" => self.self_ty.clone(),
            "identifier" => self.var(&node_text(expr, text)).map(str::to_string),
            "field_expression" => {
                let owner = self.type_of(expr.child_by_field_name("value")?, text)?;
                let field = node_text(expr.child_by_field_name("field")?, text);
                self.file.field_type(&owner, &field)
            }
            "reference_expression" => self.type_of(expr.child_by_field_name("value")?, text),
            "parenthesized_expression" => self.type_of(expr.named_child(0)?, text),
            "struct_expression" => {
                let name = expr.child_by_field_name("name")?;
                let ty = match name.kind() {
                    "generic_type_with_turbofish" => {
                        node_text(name.child_by_field_name("type")?, text)
                    }
                    _ => node_text(name, text),
                };
                self.resolve_self(ty)
            }
            "call_expression" => {
                let func = expr.child_by_field_name("function")?;
                if func.kind() == "field_expression" {
                    // `T::open(..).unwrap()` / `.expect(..)`.
                    let method = node_text(func.child_by_field_name("field")?, text);
                    if !matches!(method.as_str(), "unwrap" | "expect") {
                        return None;
                    }
                    return self.ctor_type(func.child_by_field_name("value")?, text, true);
                }
                if let Some((owner, name)) = assoc_call(expr, text) {
                    let base = last_segment(&owner);
                    if matches!(base.as_str(), "Box" | "Rc" | "Arc") && name == "new" {
                        let args = expr.child_by_field_name("arguments")?;
                        return self.type_of(args.named_child(0)?, text);
                    }
                }
                self.ctor_type(expr, text, false)
            }
            "try_expression" => self.ctor_type(expr.named_child(0)?, text, true),
            "integer_literal" | "float_literal" => literal_suffix(&node_text(expr, text)),
            "string_literal" | "raw_string_literal" => Some("str".into()),
            "boolean_literal" => Some("bool".into()),
            "char_literal" => Some("char".into()),
            _ => None,
        }
    }

    /// `T::f(..)` that yields a `T`: plainly, or once unwrapped.
    fn ctor_type(&self, call: TsNode, text: &str, unwrapped: bool) -> Option<String> {
        if call.kind() != "call_expression" {
            return None;
        }
        let (owner, name) = assoc_call(call, text)?;
        let owner = self.resolve_self(owner)?;
        if !last_segment(&owner).starts_with(char::is_uppercase) {
            return None;
        }
        let ok = match (self.file.returns(&owner, &name), unwrapped) {
            (Some(Ret::Plain), false) | (Some(Ret::Wrapped), true) => true,
            (Some(_), _) => false,
            (None, false) => is_plain_ctor(&name),
            (None, true) => is_fallible_ctor(&name),
        };
        ok.then_some(owner)
    }

    fn resolve_self(&self, ty: String) -> Option<String> {
        if ty == "Self" {
            self.self_ty.clone()
        } else {
            Some(ty)
        }
    }
}

/// `a::T::f(..)` -> (`a::T`, `f`): the owner path and the function name.
fn assoc_call(call: TsNode, text: &str) -> Option<(String, String)> {
    let func = call.child_by_field_name("function")?;
    if func.kind() != "scoped_identifier" {
        return None;
    }
    let path = func.child_by_field_name("path")?;
    let name = func.child_by_field_name("name")?;
    let owner = match path.kind() {
        "generic_type" => node_text(path.child_by_field_name("type")?, text),
        _ => node_text(path, text),
    };
    Some((owner, node_text(name, text)))
}

/// The single name a pattern binds when it is just `x` / `mut x` / `ref x`.
fn simple_binding(pat: TsNode, text: &str) -> Option<String> {
    match pat.kind() {
        "identifier" => Some(node_text(pat, text)),
        "mut_pattern" | "ref_pattern" => {
            let mut cursor = pat.walk();
            let inner = pat
                .named_children(&mut cursor)
                .find(|c| c.kind() != "mutable_specifier");
            inner.and_then(|i| simple_binding(i, text))
        }
        _ => None,
    }
}

/// The type a numeric literal's suffix spells (`3u32` -> `u32`), if any.
fn literal_suffix(lit: &str) -> Option<String> {
    const SUFFIXES: &[&str] = &[
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        "f32", "f64",
    ];
    SUFFIXES
        .iter()
        .find(|s| lit.ends_with(*s) && !lit.starts_with("0x"))
        .map(|s| s.to_string())
}
