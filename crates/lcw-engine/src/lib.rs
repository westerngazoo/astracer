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

pub mod cache;
mod discover;
pub mod languages;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lcw_adapter_cpp::CppTreeSitterAdapter;
use lcw_adapter_go::GoTreeSitterAdapter;
use lcw_adapter_py::PythonTreeSitterAdapter;
use lcw_adapter_treesitter::RustTreeSitterAdapter;
use lcw_adapter_ts::TypeScriptTreeSitterAdapter;
use lcw_config::{AdapterMode, Config, Language};
use lcw_core::{AdapterError, AnalysisReport, CodeGraph, LanguageAdapter, SourceFile};

pub use discover::{discover_files, read_sources};
pub use languages::discover_languages;

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
    #[error("[graph_store] is disabled in configuration")]
    GraphStoreDisabled,
    #[error("no cached graph at {path} — run `lcw analyze` on this repository first")]
    NoCachedGraph { path: PathBuf },
    #[error("graph store error: {0}")]
    GraphStore(#[from] lcw_graph_store::StoreError),
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

/// How an incremental parse used the fragment cache. Returned by
/// [`Engine::analyze_incremental`] for tooling/tests that want to confirm only
/// changed files were re-parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// Files considered this run.
    pub total: usize,
    /// Files served from cache (content unchanged since last run).
    pub reused: usize,
    /// Files freshly parsed (new or changed).
    pub reparsed: usize,
}

impl IncrementalStats {
    /// Stats for a whole-batch (non-incremental) parse: everything re-parsed.
    fn all_parsed(total: usize) -> Self {
        IncrementalStats {
            total,
            reused: 0,
            reparsed: total,
        }
    }
}

/// `graph.sqlite` meta key: the repository root string the graph's file paths
/// were joined onto.
pub const META_ROOT: &str = "root";
/// `graph.sqlite` meta key: how many source files discovery found.
pub const META_FILE_COUNT: &str = "file_count";
/// `graph.sqlite` meta key: the adapter and extraction version that built the
/// graph, so a graph built by an older extractor is not served as fresh.
pub const META_EXTRACTOR: &str = "extractor";

/// Why a cached graph cannot be used as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// `[graph_store].enabled = false`.
    Disabled,
    /// No `graph.sqlite` yet.
    Missing,
    /// The store predates freshness metadata (or it is unreadable).
    NoMetadata,
    /// The store was written for a different root path string.
    RootChanged,
    /// The store was built by another adapter or extraction version.
    ExtractorChanged,
    /// Source files were added or removed since the store was written.
    FileSetChanged { before: usize, now: usize },
    /// This many source files were modified after the store was written.
    FilesModified(usize),
}

impl std::fmt::Display for Staleness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Staleness::Disabled => f.write_str("graph store disabled"),
            Staleness::Missing => f.write_str("no cached graph"),
            Staleness::NoMetadata => f.write_str("cached graph has no freshness metadata"),
            Staleness::RootChanged => f.write_str("cached graph was built for another root path"),
            Staleness::ExtractorChanged => {
                f.write_str("cached graph was built by another extractor version")
            }
            Staleness::FileSetChanged { before, now } => {
                write!(f, "source files changed ({before} -> {now})")
            }
            Staleness::FilesModified(n) => write!(f, "{n} source file(s) modified"),
        }
    }
}

/// Where [`Engine::load_or_analyze`] got its graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphSource {
    /// Loaded from a fresh `graph.sqlite` (no parsing; no diagnostics).
    Cache,
    /// Freshly analyzed (and the store rewritten), for this reason.
    Analyzed(Staleness),
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
    /// Build an engine for the given config, selecting the Layer 1 adapter and
    /// registering the opt-in extended Layer 2/3 stages when the config asks for
    /// them (`lenses.extended` / `suggestions.extended`). With the default
    /// config those vectors stay empty, so the pipeline output is unchanged.
    pub fn new(config: Config) -> Self {
        let adapter = select_adapter(&config);
        let mut engine = Engine {
            config,
            adapter,
            lenses: Vec::new(),
            advisors: Vec::new(),
        };
        if engine.config.lenses.extended {
            engine.lenses.push(Box::new(|cfg, report| {
                lcw_analysis::analyze_extended(cfg, &lcw_analysis::ExtLensConfig::all(), report);
            }));
        }
        if engine.config.suggestions.extended {
            engine.advisors.push(Box::new(|cfg, report| {
                lcw_suggest::advise_extended(cfg, &lcw_suggest::ExtAdvisorConfig::all(), report);
            }));
        }
        engine
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

    /// Load the Layer-1 graph from `graph.sqlite` without discovering or parsing
    /// sources. Diagnostics, metrics, and suggestions are **not** recomputed
    /// (graph walking and layout only). Returns [`EngineError::GraphStoreDisabled`]
    /// when `[graph_store].enabled = false`, or [`EngineError::NoCachedGraph`]
    /// when the file is missing.
    pub fn load_from_store(&self, root: &Path) -> Result<AnalysisReport, EngineError> {
        if !self.config.graph_store.enabled {
            return Err(EngineError::GraphStoreDisabled);
        }
        let path = cache::graph_db_file(root, &self.config.cache.dir, self.config.cache_variant())
            .ok_or_else(|| EngineError::Config("could not resolve graph store path".into()))?;
        if !path.is_file() {
            return Err(EngineError::NoCachedGraph { path });
        }
        let snapshot = lcw_graph_store::load_graph(&path)?;
        let graph = CodeGraph::from_snapshot(snapshot);
        lcw_telemetry::count("engine.nodes", graph.node_count() as u64);
        lcw_telemetry::count("engine.edges", graph.edge_count() as u64);
        Ok(AnalysisReport::new(graph))
    }

    /// Is the cached graph for `root` still valid? `Ok(None)` means fresh: it
    /// was written for this root string, discovery finds the same number of
    /// source files, and none was modified after the analysis that produced
    /// it started. Costs one directory walk plus a `stat` per file.
    pub fn staleness(&self, root: &Path) -> Result<Option<Staleness>, EngineError> {
        if !self.config.graph_store.enabled {
            return Ok(Some(Staleness::Disabled));
        }
        let Some(path) =
            cache::graph_db_file(root, &self.config.cache.dir, self.config.cache_variant())
        else {
            return Ok(Some(Staleness::Missing));
        };
        if !path.is_file() {
            return Ok(Some(Staleness::Missing));
        }
        let meta = |key: &str| lcw_graph_store::read_meta(&path, key).ok().flatten();
        let (Some(written), Some(stored_root), Some(count)) = (
            meta(lcw_graph_store::META_WRITTEN_AT_MS).and_then(|v| v.parse::<u64>().ok()),
            meta(META_ROOT),
            meta(META_FILE_COUNT).and_then(|v| v.parse::<usize>().ok()),
        ) else {
            return Ok(Some(Staleness::NoMetadata));
        };
        if stored_root != root.to_string_lossy() {
            return Ok(Some(Staleness::RootChanged));
        }
        if meta(META_EXTRACTOR).as_deref() != Some(self.extractor_id().as_str()) {
            return Ok(Some(Staleness::ExtractorChanged));
        }
        let files = discover_files(root, &self.config, self.adapter.extensions())?;
        if files.len() != count {
            return Ok(Some(Staleness::FileSetChanged {
                before: count,
                now: files.len(),
            }));
        }
        let modified = files
            .iter()
            .filter(|f| mtime_ms(f).map_or(true, |m| m > written))
            .count();
        Ok((modified > 0).then_some(Staleness::FilesModified(modified)))
    }

    /// The fast path for query front ends: load `graph.sqlite` when it is
    /// fresh (see [`staleness`](Self::staleness)), otherwise analyze (which
    /// rewrites the store). A cached report carries the graph only — no
    /// diagnostics or suggestions.
    pub fn load_or_analyze(
        &self,
        root: &Path,
    ) -> Result<(AnalysisReport, GraphSource), EngineError> {
        let reason = match self.staleness(root)? {
            None => match self.load_from_store(root) {
                Ok(report) => {
                    lcw_telemetry::incr("engine.graph_cache_hit");
                    return Ok((report, GraphSource::Cache));
                }
                Err(_) => Staleness::NoMetadata,
            },
            Some(reason) => reason,
        };
        lcw_telemetry::incr("engine.graph_cache_miss");
        let report = self.analyze(root)?;
        Ok((report, GraphSource::Analyzed(reason)))
    }

    /// Analyze `root`, streaming [`Progress`] events through `progress` as each
    /// stage begins/ends. This is the API front ends use for live status on big
    /// repositories (the Tauri UI forwards these to the webview); `analyze` is
    /// just this with a no-op sink.
    ///
    /// When `[cache]` is enabled and the adapter supports it, parsing is
    /// **incremental**: unchanged files are served from the fragment cache and
    /// only changed files are re-parsed.
    pub fn analyze_with_progress(
        &self,
        root: &Path,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<AnalysisReport, EngineError> {
        self.run(root, progress).map(|(report, _)| report)
    }

    /// Like [`analyze_with_progress`](Self::analyze_with_progress) but also
    /// returns [`IncrementalStats`] describing cache reuse.
    pub fn analyze_incremental(
        &self,
        root: &Path,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(AnalysisReport, IncrementalStats), EngineError> {
        self.run(root, progress)
    }

    /// Shared driver: discover → read → parse (incremental or whole-batch) →
    /// Layers 2/3.
    fn run(
        &self,
        root: &Path,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<(AnalysisReport, IncrementalStats), EngineError> {
        let _perf = lcw_telemetry::PerfGuard::new("engine.analyze");
        let started_ms = now_ms();

        if self.config.cache.enabled || self.config.graph_store.enabled {
            if let Err(e) = cache::ensure_repo_cache_dir(
                root,
                &self.config.cache.dir,
                self.config.cache_variant(),
            ) {
                lcw_telemetry::warn!(
                    target: "lcw::engine",
                    error = %e,
                    "failed to create cache directory (analysis still correct)"
                );
            }
        }

        progress(Progress::Discovering);
        let files = discover_files(root, &self.config, self.adapter.extensions())?;
        lcw_telemetry::count("engine.files_discovered", files.len() as u64);
        progress(Progress::Discovered { files: files.len() });
        if files.is_empty() {
            return Err(EngineError::NoFiles(root.to_path_buf()));
        }

        progress(Progress::Reading { files: files.len() });
        let sources = read_sources(&files);

        progress(Progress::Parsing {
            files: sources.len(),
        });
        let (graph, stats) = if self.config.cache.enabled && self.adapter.supports_incremental() {
            self.parse_incremental(root, &sources)?
        } else {
            let graph = lcw_telemetry::timed("engine.parse", || self.adapter.parse(&sources))?;
            (graph, IncrementalStats::all_parsed(sources.len()))
        };
        lcw_telemetry::count("engine.nodes", graph.node_count() as u64);
        lcw_telemetry::count("engine.edges", graph.edge_count() as u64);
        lcw_telemetry::count("engine.graph_bytes", graph.estimated_bytes() as u64);

        let report = self.finish_report(graph, progress);
        self.persist_graph_store(root, &report.graph, started_ms, files.len());
        Ok((report, stats))
    }

    /// Analyze an already-loaded set of sources (used by the UI and tests).
    /// Always a whole-batch parse: the disk cache is keyed by repo root, which
    /// in-memory sources don't have.
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
        lcw_telemetry::count("engine.graph_bytes", graph.estimated_bytes() as u64);
        Ok(self.finish_report(graph, progress))
    }

    /// Incremental Layer 1: reuse cached fragments for unchanged files, extract
    /// the rest, resolve the union into a graph, and persist the cache.
    fn parse_incremental(
        &self,
        root: &Path,
        sources: &[SourceFile],
    ) -> Result<(CodeGraph, IncrementalStats), EngineError> {
        let _perf = lcw_telemetry::PerfGuard::new("engine.parse_incremental");
        let adapter_name = self.adapter.name();
        let cache_path =
            cache::cache_file(root, &self.config.cache.dir, self.config.cache_variant());
        let mut store = match &cache_path {
            Some(p) => cache::FragmentCache::load(p, adapter_name),
            None => cache::FragmentCache::new(adapter_name),
        };

        let mut fragments = Vec::with_capacity(sources.len());
        let mut present = HashSet::with_capacity(sources.len());
        let mut stats = IncrementalStats {
            total: sources.len(),
            reused: 0,
            reparsed: 0,
        };
        for sf in sources {
            present.insert(sf.path.clone());
            let hash = cache::hash_text(&sf.text);
            if let Some(frag) = store.get(&sf.path, hash) {
                fragments.push(frag.clone());
                stats.reused += 1;
            } else {
                let frag = self.adapter.extract_fragment(sf)?;
                store.insert(sf.path.clone(), hash, frag.clone());
                fragments.push(frag);
                stats.reparsed += 1;
            }
        }
        store.retain(&present);

        lcw_telemetry::count("engine.files_reused", stats.reused as u64);
        lcw_telemetry::count("engine.files_reparsed", stats.reparsed as u64);

        let graph = self.adapter.resolve_fragments(&fragments)?;

        // Persist best-effort: a failed write must never fail the analysis.
        if let Some(path) = &cache_path {
            if store.is_dirty() {
                if let Err(e) = store.save(path) {
                    lcw_telemetry::warn!(
                        target: "lcw::engine",
                        error = %e,
                        path = %path.display(),
                        "failed to persist fragment cache (analysis still correct)"
                    );
                }
            }
        }

        Ok((graph, stats))
    }

    /// Turn a parsed graph into a full report: Layer 2 lenses + metrics, then
    /// Layer 3 vertical detection + suggestions, plus any registered stages.
    fn finish_report(
        &self,
        graph: CodeGraph,
        progress: &mut dyn FnMut(Progress),
    ) -> AnalysisReport {
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
        report
    }

    /// What [`META_EXTRACTOR`] records for this engine's adapter.
    fn extractor_id(&self) -> String {
        format!("{}/{}", self.adapter.name(), cache::CACHE_VERSION)
    }

    /// Best-effort write of the Layer-1 graph to `graph.sqlite` under the cache
    /// dir, with the freshness metadata [`staleness`](Self::staleness) checks.
    fn persist_graph_store(
        &self,
        root: &Path,
        graph: &CodeGraph,
        started_ms: u64,
        file_count: usize,
    ) {
        if !self.config.graph_store.enabled {
            return;
        }
        let Some(path) =
            cache::graph_db_file(root, &self.config.cache.dir, self.config.cache_variant())
        else {
            return;
        };
        let snapshot = graph.snapshot();
        let meta = [
            (lcw_graph_store::META_WRITTEN_AT_MS, started_ms.to_string()),
            (META_ROOT, root.to_string_lossy().into_owned()),
            (META_FILE_COUNT, file_count.to_string()),
            (META_EXTRACTOR, self.extractor_id()),
        ];
        if let Err(e) = lcw_graph_store::write_with_meta(&path, &snapshot, &meta) {
            lcw_telemetry::warn!(
                target: "lcw::engine",
                error = %e,
                path = %path.display(),
                "failed to persist graph store (analysis still correct)"
            );
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn select_adapter(config: &Config) -> Box<dyn LanguageAdapter> {
    match config.adapter.mode {
        // `semantic` is the Rust-only rust-analyzer path; language is ignored.
        AdapterMode::Semantic => select_semantic_adapter(),
        // `fast` picks the tree-sitter front end for the configured language.
        AdapterMode::Fast => match config.adapter.language {
            Language::Rust => Box::new(RustTreeSitterAdapter::new()),
            Language::Typescript => Box::new(TypeScriptTreeSitterAdapter::new()),
            Language::Python => Box::new(PythonTreeSitterAdapter::new()),
            Language::Go => Box::new(GoTreeSitterAdapter::new()),
            Language::Cpp => Box::new(CppTreeSitterAdapter::new()),
        },
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
    fn incremental_reuses_unchanged_and_reparses_changed() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lcw-inc-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.rs"), "fn a() { b(); }").unwrap();
        fs::write(src.join("b.rs"), "fn b() {}").unwrap();

        // Absolute cache dir outside the fake repo (mirrors default isolation, no env vars).
        let cache_dir =
            std::env::temp_dir().join(format!("lcw-inc-cache-{}-{nanos}", std::process::id()));
        let mut cfg = Config::default();
        cfg.cache.dir = cache_dir.to_string_lossy().into_owned();
        let engine = Engine::new(cfg);

        // Cold run: everything parsed.
        let (r1, s1) = engine.analyze_incremental(&root, &mut |_| {}).unwrap();
        assert_eq!((s1.total, s1.reused, s1.reparsed), (2, 0, 2));
        let (n1, e1) = (r1.graph.node_count(), r1.graph.edge_count());

        // Warm run, no changes: everything reused, identical graph.
        let (r2, s2) = engine.analyze_incremental(&root, &mut |_| {}).unwrap();
        assert_eq!((s2.reused, s2.reparsed), (2, 0));
        assert_eq!(r2.graph.node_count(), n1);
        assert_eq!(r2.graph.edge_count(), e1);

        // Change one file: only it is re-parsed; the graph reflects the change.
        fs::write(src.join("b.rs"), "fn b() { c(); } fn c() {}").unwrap();
        let (r3, s3) = engine.analyze_incremental(&root, &mut |_| {}).unwrap();
        assert_eq!(s3.reused, 1, "a.rs unchanged -> reused");
        assert_eq!(s3.reparsed, 1, "b.rs changed -> reparsed");
        assert!(r3.graph.node_count() > n1, "new fn c() should appear");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn analyze_does_not_create_artifacts_in_repo_root() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _lock = cache::CACHE_HOME_TEST_LOCK.lock().unwrap();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lcw-isolate-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn a() { b(); } pub fn b() {}").unwrap();

        let cache_home =
            std::env::temp_dir().join(format!("lcw-isolate-cache-{}-{nanos}", std::process::id()));
        let _cache_env = cache::CacheHomeGuard::set(&cache_home);

        let engine = Engine::new(Config::default());
        engine.analyze(&root).expect("analyze");
        engine.analyze(&root).expect("second analyze (warm cache)");

        let mut entries: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![std::ffi::OsStr::new("src")],
            "repo root must not gain cache or other analysis artifacts"
        );

        let cache_path = cache::cache_file(&root, "", "rust").expect("cache path");
        assert!(
            cache_path.starts_with(&cache_home),
            "cache file {:?} should live under {:?}",
            cache_path,
            cache_home
        );
        assert!(cache_path.is_file(), "fragment cache should be persisted");

        let graph_path = cache::graph_db_file(&root, "", "rust").expect("graph db path");
        assert!(
            graph_path.starts_with(&cache_home),
            "graph store {:?} should live under {:?}",
            graph_path,
            cache_home
        );
        assert!(graph_path.is_file(), "graph.sqlite should be persisted");
        let summary = lcw_graph_store::summary(&graph_path).expect("graph summary");
        assert!(summary.nodes >= 2, "expected at least a and b");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_home);
    }

    #[test]
    fn load_from_store_after_analyze() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _lock = cache::CACHE_HOME_TEST_LOCK.lock().unwrap();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lcw-load-store-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn a() { b(); } pub fn b() {}").unwrap();

        let cache_home = std::env::temp_dir().join(format!(
            "lcw-load-store-cache-{}-{nanos}",
            std::process::id()
        ));
        let _cache_env = cache::CacheHomeGuard::set(&cache_home);

        let engine = Engine::new(Config::default());
        let full = engine.analyze(&root).expect("analyze");
        let loaded = engine.load_from_store(&root).expect("load_from_store");
        assert_eq!(loaded.graph.node_count(), full.graph.node_count());
        assert_eq!(loaded.graph.edge_count(), full.graph.edge_count());
        assert!(
            loaded.diagnostics.is_empty(),
            "cached load skips Layer 2/3 for MVP"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_home);
    }

    #[test]
    fn load_or_analyze_uses_fresh_store_and_detects_staleness() {
        use std::fs;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let _lock = cache::CACHE_HOME_TEST_LOCK.lock().unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lcw-fresh-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn a() { b(); } pub fn b() {}").unwrap();
        let cache_home =
            std::env::temp_dir().join(format!("lcw-fresh-cache-{}-{nanos}", std::process::id()));
        let _cache_env = cache::CacheHomeGuard::set(&cache_home);

        let engine = Engine::new(Config::default());
        let (_, first) = engine.load_or_analyze(&root).unwrap();
        assert_eq!(first, GraphSource::Analyzed(Staleness::Missing));
        assert_eq!(engine.staleness(&root).unwrap(), None);
        let (cached, second) = engine.load_or_analyze(&root).unwrap();
        assert_eq!(second, GraphSource::Cache);
        assert_eq!(cached.graph.node_count(), 2);

        // A file touched after the store was written makes it stale.
        let f = fs::File::options()
            .write(true)
            .open(src.join("lib.rs"))
            .unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert_eq!(
            engine.staleness(&root).unwrap(),
            Some(Staleness::FilesModified(1))
        );
        let (_, third) = engine.load_or_analyze(&root).unwrap();
        assert!(matches!(
            third,
            GraphSource::Analyzed(Staleness::FilesModified(1))
        ));

        // Re-analysis stamps the start time, which is before the future mtime,
        // so it stays stale until the file is back in the past; adding a file
        // is caught by the count check.
        fs::write(src.join("c.rs"), "pub fn c() {}").unwrap();
        assert_eq!(
            engine.staleness(&root).unwrap(),
            Some(Staleness::FileSetChanged { before: 1, now: 2 })
        );

        let other = root.join(".").join("src").join("..");
        assert_eq!(
            engine.staleness(&other).unwrap(),
            Some(Staleness::RootChanged)
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_home);
    }

    #[test]
    fn load_from_store_errors_when_missing() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _lock = cache::CACHE_HOME_TEST_LOCK.lock().unwrap();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lcw-load-miss-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let cache_home = std::env::temp_dir().join(format!(
            "lcw-load-miss-cache-{}-{nanos}",
            std::process::id()
        ));
        let _cache_env = cache::CacheHomeGuard::set(&cache_home);

        let engine = Engine::new(Config::default());
        assert!(matches!(
            engine.load_from_store(&root),
            Err(EngineError::NoCachedGraph { .. })
        ));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_home);
    }

    #[test]
    fn graph_store_roundtrip_matches_memory_graph() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _lock = cache::CACHE_HOME_TEST_LOCK.lock().unwrap();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lcw-graph-rt-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn a() { b(); } pub fn b() {}").unwrap();

        let cache_home =
            std::env::temp_dir().join(format!("lcw-graph-rt-cache-{}-{nanos}", std::process::id()));
        let _cache_env = cache::CacheHomeGuard::set(&cache_home);

        let report = Engine::new(Config::default())
            .analyze(&root)
            .expect("analyze");
        let graph_path = cache::graph_db_file(&root, "", "rust").expect("path");
        let loaded = lcw_graph_store::read(&graph_path).expect("read store");
        let from_disk = CodeGraph::from_snapshot(loaded);
        assert_eq!(from_disk.node_count(), report.graph.node_count());
        assert_eq!(from_disk.edge_count(), report.graph.edge_count());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&cache_home);
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

    #[test]
    fn fast_mode_selects_adapter_by_language() {
        use lcw_config::Language;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lcw-lang-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        // A Python file only the Python front end discovers (`.py`) and parses.
        fs::write(
            src.join("m.py"),
            "def a():\n    b()\n\ndef b():\n    pass\n",
        )
        .unwrap();

        // Default (Rust) discovers only `.rs`, so there is nothing to analyze.
        let rust = Engine::new(Config::default());
        assert!(matches!(rust.analyze(&root), Err(EngineError::NoFiles(_))));

        // Selecting Python discovers and parses the file into a graph.
        let mut cfg = Config::default();
        cfg.adapter.language = Language::Python;
        cfg.cache.enabled = false; // the python adapter is whole-batch anyway
        let py = Engine::new(cfg);
        let report = py.analyze(&root).unwrap();
        assert!(report.graph.node_count() >= 2, "def a + def b");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fast_mode_selects_cpp_adapter() {
        use lcw_config::Language;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("lcw-cpp-{}-{nanos}", std::process::id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.cpp"), "void a() { b(); }\nvoid b() {}\n").unwrap();

        let rust = Engine::new(Config::default());
        assert!(matches!(rust.analyze(&root), Err(EngineError::NoFiles(_))));

        let mut cfg = Config::default();
        cfg.adapter.language = Language::Cpp;
        cfg.cache.enabled = false;
        let cpp = Engine::new(cfg);
        let report = cpp.analyze(&root).unwrap();
        assert!(report.graph.node_count() >= 2, "a + b");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extended_stages_run_only_when_configured() {
        let sources = vec![SourceFile::new(
            "src/lib.rs",
            "fn a() { b(); } fn b() { a(); }", // a <-> b mutual recursion
        )];

        // Opt in: the extended recursion lens + advisor are registered and fire.
        let mut cfg = Config::default();
        cfg.lenses.extended = true;
        cfg.suggestions.extended = true;
        let report = Engine::new(cfg).analyze_sources(&sources).unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "recursion_cycle"),
            "extended recursion lens should be registered and fire"
        );
        assert!(
            report
                .suggestions
                .iter()
                .any(|s| s.title.contains("call cycles")),
            "matching extended advisor should turn the finding into advice"
        );

        // Default config leaves them off (baseline output is unchanged).
        let base = Engine::new(Config::default())
            .analyze_sources(&sources)
            .unwrap();
        assert!(base.diagnostics.iter().all(|d| d.code != "recursion_cycle"));
    }
}
