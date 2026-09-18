//! Layer 1 seam: the closed interface every language front end implements
//! (Principle I). Swapping tree-sitter for a semantic rust-analyzer backend is
//! just a different `LanguageAdapter` impl behind this trait.

use std::path::PathBuf;

use crate::graph::CodeGraph;
use crate::source::SourceFile;

/// Errors an adapter can raise. Parsing is best-effort: an adapter should
/// prefer skipping a broken file (and reporting it) over aborting the run.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("failed to parse {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("adapter error: {0}")]
    Other(String),
}

/// A language front end that turns source files into a [`CodeGraph`].
pub trait LanguageAdapter: Send + Sync {
    /// Stable identifier, e.g. `"treesitter-rust"`.
    fn name(&self) -> &'static str;

    /// File extensions this adapter handles, without the dot, e.g. `["rs"]`.
    fn extensions(&self) -> &'static [&'static str];

    /// Parse a batch of files into a single graph. Cross-file resolution is
    /// the adapter's responsibility.
    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError>;
}
