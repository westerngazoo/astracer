//! Layer 1 seam: the closed interface every language front end implements
//! (Principle I). Swapping tree-sitter for a semantic rust-analyzer backend is
//! just a different `LanguageAdapter` impl behind this trait.

use std::path::PathBuf;

use crate::fragment::FileFragment;
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

    /// Whether this adapter supports the incremental extract/resolve split.
    ///
    /// When `true`, the engine caches per-file [`FileFragment`]s by content
    /// hash and only re-runs [`extract_fragment`](Self::extract_fragment) on
    /// files that changed, then links everything with
    /// [`resolve_fragments`](Self::resolve_fragments). Default: `false` (the
    /// engine falls back to the whole-batch [`parse`](Self::parse)).
    fn supports_incremental(&self) -> bool {
        false
    }

    /// Extract one file's [`FileFragment`] — its definitions plus unresolved
    /// call sites — without any cross-file linking. Must be a pure function of
    /// the file's path + text so the result is safely cacheable.
    ///
    /// Only called when [`supports_incremental`](Self::supports_incremental)
    /// returns `true`; the default errors.
    fn extract_fragment(&self, file: &SourceFile) -> Result<FileFragment, AdapterError> {
        let _ = file;
        Err(AdapterError::Other(
            "this adapter does not support incremental extraction".into(),
        ))
    }

    /// Link a set of fragments into one [`CodeGraph`] (the global cross-file
    /// resolution pass). The result must not depend on whether a given fragment
    /// was freshly extracted or served from cache.
    ///
    /// Only called when [`supports_incremental`](Self::supports_incremental)
    /// returns `true`; the default errors.
    fn resolve_fragments(&self, fragments: &[FileFragment]) -> Result<CodeGraph, AdapterError> {
        let _ = fragments;
        Err(AdapterError::Other(
            "this adapter does not support incremental resolution".into(),
        ))
    }
}
