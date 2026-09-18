//! # lcw-engine
//!
//! The orchestrator facade (Manifesto: modular monolith core). It ties the
//! layers together behind one call:
//!
//! ```text
//! config -> discover files -> Layer 1 adapter -> graph
//!        -> Layer 2 lenses  -> metrics + diagnostics
//!        -> Layer 3 advisors -> vertical + suggestions
//! ```
//!
//! Front ends (CLI, Tauri UI) depend only on this crate, never on the
//! individual layers (Principle I).

mod discover;

use std::path::{Path, PathBuf};

use lcw_adapter_treesitter::RustTreeSitterAdapter;
use lcw_config::{AdapterMode, Config};
use lcw_core::{AdapterError, AnalysisReport, LanguageAdapter, SourceFile};

pub use discover::{discover_files, read_sources};

/// Errors surfaced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("failed to walk repository: {0}")]
    Walk(#[from] ignore::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error("no source files found under {0}")]
    NoFiles(PathBuf),
}

/// Streaming analysis progress (Manifesto Principle III + plan: streaming API
/// for giant repos). Emitted at stage boundaries so front ends can show live
/// status without blocking on the whole run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Walking the repository for source files.
    Discovering,
    /// File discovery finished.
    Discovered { files: usize },
    /// Reading discovered files into memory.
    Reading { files: usize },
    /// Layer 1: parsing sources into a call graph.
    Parsing { files: usize },
    /// Layer 1 finished.
    Parsed { nodes: usize, edges: usize },
    /// Layer 2: engineering lenses + metrics.
    Analyzing,
    /// Layer 3: vertical detection + suggestions.
    Advising,
    /// The whole pipeline finished.
    Done {
        nodes: usize,
        diagnostics: usize,
        suggestions: usize,
    },
}

/// Extra hooks the engine calls after the built-in Layer 2 pass to further
/// enrich the report (e.g. Layer 3 advisors in phase 6, or bespoke rules).
/// Keeping them as boxed closures means the engine's public API is stable
/// across phases.
type EnrichFn = Box<dyn Fn(&Config, &mut AnalysisReport) + Send + Sync>;

/// The analysis engine. Cheap to construct; holds the chosen adapter.
pub struct Engine {
    config: Config,
    adapter: Box<dyn LanguageAdapter>,
    lenses: Vec<EnrichFn>,
    advisors: Vec<EnrichFn>,
}

impl Engine {
    /// Build an engine for the given config, selecting the Layer 1 adapter.
    pub fn new(config: Config) -> Self {
        let adapter = select_adapter(&config);
        Engine {
            config,
            adapter,
            lenses: Vec::new(),
            advisors: Vec::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Register a Layer 2 enrichment step (a lens runner).
    pub fn with_lens_stage(mut self, f: EnrichFn) -> Self {
        self.lenses.push(f);
        self
    }

    /// Register a Layer 3 enrichment step (an advisor runner).
    pub fn with_advisor_stage(mut self, f: EnrichFn) -> Self {
        self.advisors.push(f);
        self
    }

    /// Analyze a repository rooted at `root`.
    pub fn analyze(&self, root: &Path) -> Result<AnalysisReport, EngineError> {
        self.analyze_with_progress(root, &mut |_| {})
    }

    /// Analyze `root`, streaming [`Progress`] events through `progress` as each
    /// stage begins/ends. This is the API front ends use for live status on big
    /// repositories (the Tauri UI forwards these to the webview); `analyze` is
    /// just this with a no-op sink.
    pub fn analyze_with_progress(
        &self,
        root: &Path,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<AnalysisReport, EngineError> {
        let _perf = lcw_telemetry::PerfGuard::new("engine.analyze");

        progress(Progress::Discovering);
        let files = discover_files(root, &self.config, self.adapter.extensions())?;
        lcw_telemetry::count("engine.files_discovered", files.len() as u64);
        progress(Progress::Discovered { files: files.len() });
        if files.is_empty() {
            return Err(EngineError::NoFiles(root.to_path_buf()));
        }

        progress(Progress::Reading { files: files.len() });
        let sources = read_sources(&files);
        self.analyze_sources_with_progress(&sources, progress)
    }

    /// Analyze an already-loaded set of sources (used by the UI and tests).
    pub fn analyze_sources(&self, sources: &[SourceFile]) -> Result<AnalysisReport, EngineError> {
        self.analyze_sources_with_progress(sources, &mut |_| {})
    }

    /// [`analyze_sources`](Self::analyze_sources) with progress streaming.
    pub fn analyze_sources_with_progress(
        &self,
        sources: &[SourceFile],
        progress: &mut dyn FnMut(Progress),
    ) -> Result<AnalysisReport, EngineError> {
        progress(Progress::Parsing {
            files: sources.len(),
        });
        let graph = lcw_telemetry::timed("engine.parse", || self.adapter.parse(sources))?;
        lcw_telemetry::count("engine.nodes", graph.node_count() as u64);
        lcw_telemetry::count("engine.edges", graph.edge_count() as u64);

        let mut report = AnalysisReport::new(graph);
        progress(Progress::Parsed {
            nodes: report.graph.node_count(),
            edges: report.graph.edge_count(),
        });

        // Layer 2: built-in engineering lenses + quality metrics.
        progress(Progress::Analyzing);
        {
            let _perf = lcw_telemetry::PerfGuard::new("engine.lenses");
            lcw_analysis::analyze(&self.config, &mut report);
        }
        lcw_telemetry::count("engine.diagnostics", report.diagnostics.len() as u64);

        // Layer 3: vertical detection + target-driven suggestions.
        progress(Progress::Advising);
        {
            let _perf = lcw_telemetry::PerfGuard::new("engine.advise");
            lcw_suggest::advise(&self.config, &mut report);
        }
        lcw_telemetry::count("engine.suggestions", report.suggestions.len() as u64);

        // Extra caller-registered stages (extensibility hooks / bespoke rules).
        for lens in &self.lenses {
            lens(&self.config, &mut report);
        }
        for advisor in &self.advisors {
            advisor(&self.config, &mut report);
        }

        progress(Progress::Done {
            nodes: report.graph.node_count(),
            diagnostics: report.diagnostics.len(),
            suggestions: report.suggestions.len(),
        });
        Ok(report)
    }
}

fn select_adapter(config: &Config) -> Box<dyn LanguageAdapter> {
    match config.adapter.mode {
        AdapterMode::Fast => Box::new(RustTreeSitterAdapter::new()),
        AdapterMode::Semantic => select_semantic_adapter(),
    }
}

/// With the `semantic` feature, use the rust-analyzer backend; otherwise warn
/// and fall back to tree-sitter so a config asking for `semantic` still runs.
#[cfg(feature = "semantic")]
fn select_semantic_adapter() -> Box<dyn LanguageAdapter> {
    Box::new(lcw_adapter_ra::RustAnalyzerAdapter::new())
}

#[cfg(not(feature = "semantic"))]
fn select_semantic_adapter() -> Box<dyn LanguageAdapter> {
    lcw_telemetry::warn!(
        target: "lcw::engine",
        "adapter.mode = semantic requested but this build lacks the `semantic` feature; using fast (tree-sitter). Rebuild with --features semantic."
    );
    Box::new(RustTreeSitterAdapter::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_in_memory_sources() {
        let engine = Engine::new(Config::default());
        let sources = vec![SourceFile::new("src/lib.rs", "fn a() { b(); } fn b() {}")];
        let report = engine.analyze_sources(&sources).unwrap();
        assert_eq!(report.graph.node_count(), 2);
        assert_eq!(report.graph.edge_count(), 1);
        let summary = report.summary();
        assert_eq!(summary.nodes, 2);
    }

    #[test]
    fn streams_progress_in_order() {
        let engine = Engine::new(Config::default());
        let sources = vec![SourceFile::new("src/lib.rs", "fn a() { b(); } fn b() {}")];

        let mut events = Vec::new();
        let report = engine
            .analyze_sources_with_progress(&sources, &mut |p| events.push(p))
            .unwrap();

        assert_eq!(report.graph.node_count(), 2);
        // Parsing precedes Parsed precedes Analyzing precedes Advising precedes Done.
        let parsing = events
            .iter()
            .position(|p| matches!(p, Progress::Parsing { .. }));
        let done = events
            .iter()
            .position(|p| matches!(p, Progress::Done { .. }));
        assert!(parsing.is_some() && done.is_some());
        assert!(parsing < done);
        assert!(matches!(
            events.last(),
            Some(Progress::Done { nodes: 2, .. })
        ));
    }
}
