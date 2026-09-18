//! # lcw-adapter-ra
//!
//! The optional **semantic** Layer 1 [`LanguageAdapter`]: a call-graph
//! extractor backed by [rust-analyzer](https://rust-analyzer.github.io/)
//! (`ra_ap_*` crates). Unlike the default tree-sitter adapter, this resolves
//! calls through rust-analyzer's real name resolution and type inference, so
//! it captures method dispatch, trait calls and cross-crate targets precisely.
//!
//! It is **feature-gated** behind `semantic` (Manifesto risk note: `ra_ap_*`
//! has an unstable API and is heavy on giant repos, so it lives off the fast
//! path). Built without the feature, [`RustAnalyzerAdapter::parse`] returns a
//! clear error and the crate pulls in none of the rust-analyzer dependencies.
//!
//! The engine selects this adapter when `adapter.mode = "semantic"` **and** the
//! workspace was compiled with `--features semantic`; otherwise it transparently
//! falls back to the fast tree-sitter adapter.

use lcw_core::{AdapterError, CodeGraph, LanguageAdapter, SourceFile};

#[cfg(feature = "semantic")]
mod semantic;

/// Semantic Rust front end backed by rust-analyzer.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustAnalyzerAdapter;

impl RustAnalyzerAdapter {
    pub fn new() -> Self {
        RustAnalyzerAdapter
    }

    /// Whether this build actually contains the rust-analyzer backend.
    pub const fn is_semantic_enabled() -> bool {
        cfg!(feature = "semantic")
    }
}

impl LanguageAdapter for RustAnalyzerAdapter {
    fn name(&self) -> &'static str {
        "rust-analyzer"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    #[cfg(feature = "semantic")]
    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        semantic::parse(files)
    }

    #[cfg(not(feature = "semantic"))]
    fn parse(&self, _files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        Err(AdapterError::Other(
            "lcw-adapter-ra was built without the `semantic` feature; rebuild with \
             `--features semantic` (or the engine's `semantic` feature) to enable the \
             rust-analyzer backend"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_extensions_and_name() {
        let a = RustAnalyzerAdapter::new();
        assert_eq!(a.name(), "rust-analyzer");
        assert_eq!(a.extensions(), &["rs"]);
    }

    #[cfg(not(feature = "semantic"))]
    #[test]
    fn parse_without_feature_errors_clearly() {
        let a = RustAnalyzerAdapter::new();
        let err = a.parse(&[]).unwrap_err();
        assert!(err.to_string().contains("semantic"));
        assert!(!RustAnalyzerAdapter::is_semantic_enabled());
    }
}
