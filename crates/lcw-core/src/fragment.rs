//! Layer-1 *fragment* types for incremental analysis.
//!
//! A [`FileFragment`] is one source file's contribution to the call graph
//! **before global resolution**: the function/method definitions it declares
//! plus its unresolved call sites. It is a pure function of that file's path +
//! text, so the engine can cache it by content hash and re-extract only the
//! files that actually changed (plan / Manifesto: incremental analysis for
//! giant repos). Linking calls to their definitions across files is then a
//! cheap second pass over the union of fragments
//! ([`LanguageAdapter::resolve_fragments`]).
//!
//! The default tree-sitter adapter opts in; adapters that cannot cheaply
//! separate extraction from resolution simply don't, and the engine falls back
//! to the whole-batch [`LanguageAdapter::parse`].
//!
//! [`LanguageAdapter::resolve_fragments`]: crate::adapter::LanguageAdapter::resolve_fragments
//! [`LanguageAdapter::parse`]: crate::adapter::LanguageAdapter::parse

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::graph::{Node, SourceSpan};

/// How a call site was written. Kept alongside the raw call so resolution can
/// pick the right [`EdgeKind`] once the target is known, without re-parsing.
///
/// [`EdgeKind`]: crate::graph::EdgeKind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// `foo()` — free/associated function call resolved by path.
    Direct,
    /// `x.foo()` — method call (receiver-based).
    Method,
    /// `Type::foo()` — associated call by type path.
    Associated,
    /// `foo!(...)` — macro invocation.
    Macro,
}

/// A single unresolved call site captured during per-file extraction.
///
/// The callee is stored *as written* (`path` + `short`) rather than as a node
/// id, because the target may live in another file that is resolved later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCall {
    /// Fully qualified name of the enclosing caller (its definition's
    /// `qualified_name`); used to find the caller node after resolution.
    pub caller: String,
    /// Syntactic class of the call.
    pub kind: CallKind,
    /// Callee as written, e.g. `Type::foo`, `foo`, `bar`.
    pub path: String,
    /// Last path segment — the resolvable short name.
    pub short: String,
    /// Call-site location. Its `file` field is fragment-local (there is only
    /// one file per fragment) and is remapped during resolution.
    pub call_site: SourceSpan,
}

/// One file's pre-resolution contribution to the graph: the definitions it
/// declares and the call sites it contains.
///
/// Node ids inside `defs` are placeholders (assigned when the fragment is
/// resolved into a graph), and every `span.file` / `call_site.file` is
/// fragment-local and remapped at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFragment {
    /// Path of the file this fragment was extracted from.
    pub path: PathBuf,
    /// Functions/methods declared in the file, in source order.
    pub defs: Vec<Node>,
    /// Unresolved call sites in the file, in source order.
    pub calls: Vec<RawCall>,
}

impl FileFragment {
    /// Build an (empty) fragment for `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileFragment {
            path: path.into(),
            defs: Vec::new(),
            calls: Vec::new(),
        }
    }
}
