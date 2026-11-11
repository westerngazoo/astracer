//! # lcw-agent
//!
//! The **agent-facing contract** shared by `lcw … --json` and the `lcw-mcp`
//! server, so an AI coding agent gets byte-for-byte the same answer from
//! either:
//!
//! * [`load`] — resolve config, then serve the graph from a fresh
//!   `graph.sqlite` (no parse) or re-analyze when sources changed; paths are
//!   rewritten once for display (relative to the caller's cwd, or absolute).
//! * [`json`] — the versioned (`"schema": 1`) JSON views of every query.
//! * [`fit_json`] / [`fit_text`] — token budgets: shrink list limits until the
//!   output fits, marking cut lists with `<list>_more: N` / `+N more`.
//!
//! Like every front end this crate renders; all traversal is `lcw-query`'s.

pub mod json;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lcw_config::{Config, Language};
use lcw_core::CodeGraph;
use lcw_engine::{Engine, GraphSource};

/// Version of the JSON contract; every object this crate emits carries it as
/// a top-level `"schema"` field. Bump on breaking shape changes.
pub const SCHEMA: u32 = 1;

/// How many candidates to offer when a symbol is ambiguous or not found.
pub const MAX_CANDIDATES: usize = 8;

/// How file paths in the loaded graph are displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStyle {
    /// Absolute paths (the MCP server: its cwd is unrelated to the repo).
    Absolute,
    /// Relative to this directory when under it, absolute otherwise (the CLI:
    /// short paths that still open from the shell the agent is in).
    RelativeTo(PathBuf),
}

/// Where and how to load a repository.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Explicit config file; otherwise `livewalk.toml` is searched upward.
    pub config: Option<PathBuf>,
    pub semantic: bool,
    pub language: Option<Language>,
    /// Serve a fresh `graph.sqlite` instead of re-analyzing (default on).
    pub use_cache: bool,
    pub paths: PathStyle,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            config: None,
            semantic: false,
            language: None,
            use_cache: true,
            paths: PathStyle::Absolute,
        }
    }
}

/// A loaded repository: its call graph, ready for `lcw-query`.
pub struct Repo {
    /// Canonical repository root.
    pub root: PathBuf,
    pub config: Config,
    pub graph: CodeGraph,
    /// Cache hit, or why the repo was re-analyzed.
    pub source: GraphSource,
}

impl Repo {
    /// An engine for this repo's config (cheap; used to re-check freshness).
    pub fn engine(&self) -> Engine {
        Engine::new(self.config.clone())
    }
}

/// Resolve the config for `path` (with the language / semantic overrides) the
/// same way every `lcw` command does.
pub fn resolve_config(path: &Path, opts: &LoadOptions) -> Result<Config> {
    let search_dir = config_search_dir(path);
    let mut config =
        Config::resolve(opts.config.as_deref(), &search_dir).context("resolving configuration")?;
    if opts.semantic {
        config.adapter.mode = lcw_config::AdapterMode::Semantic;
    }
    if let Some(lang) = opts.language {
        config.adapter.language = lang;
    }
    Ok(config)
}

/// Load `path`'s graph: from a fresh store when `use_cache`, else (or when
/// stale) by analyzing, which rewrites the store. The root is canonicalized
/// first so the CLI and the MCP server share one cache entry.
pub fn load(path: &Path, opts: &LoadOptions) -> Result<Repo> {
    let root = path
        .canonicalize()
        .with_context(|| format!("repository path {} does not exist", path.display()))?;
    let config = resolve_config(&root, opts)?;
    let engine = Engine::new(config.clone());
    let (report, source) = if opts.use_cache {
        engine
            .load_or_analyze(&root)
            .with_context(|| format!("loading {}", root.display()))?
    } else {
        let report = engine
            .analyze(&root)
            .with_context(|| format!("analyzing {}", root.display()))?;
        (
            report,
            GraphSource::Analyzed(lcw_engine::Staleness::Missing),
        )
    };
    let graph = display_paths(report.graph, &opts.paths);
    Ok(Repo {
        root,
        config,
        graph,
        source,
    })
}

/// Rewrite the graph's file table for display (node ids are preserved).
fn display_paths(graph: CodeGraph, style: &PathStyle) -> CodeGraph {
    let PathStyle::RelativeTo(base) = style else {
        return graph;
    };
    let base = base.canonicalize().unwrap_or_else(|_| base.clone());
    let mut snap = graph.snapshot();
    let mut changed = false;
    for f in &mut snap.files {
        if let Ok(rel) = f.strip_prefix(&base) {
            *f = rel.to_path_buf();
            changed = true;
        }
    }
    if changed {
        CodeGraph::from_snapshot(snap)
    } else {
        graph
    }
}

/// Directory from which to search upward for a config file.
pub fn config_search_dir(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Rough token estimate for budget fitting: ~4 bytes per token.
pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// Render text with list limit `start`, halving it until the result fits
/// `budget` tokens (or the limit reaches 1). No budget: one render at `start`.
pub fn fit_text(
    budget: Option<usize>,
    start: usize,
    mut render: impl FnMut(usize) -> String,
) -> String {
    let mut limit = start.max(1);
    loop {
        let out = render(limit);
        match budget {
            Some(b) if estimate_tokens(&out) > b && limit > 1 => limit /= 2,
            _ => return out,
        }
    }
}

/// Build JSON with no list limit, then with shrinking limits (64, 32, … 1)
/// until its compact serialization fits `budget` tokens. No budget: unlimited.
pub fn fit_json(
    budget: Option<usize>,
    mut build: impl FnMut(usize) -> serde_json::Value,
) -> serde_json::Value {
    let full = build(usize::MAX);
    let Some(b) = budget else {
        return full;
    };
    if estimate_tokens(&full.to_string()) <= b {
        return full;
    }
    let mut limit = 64;
    loop {
        let v = build(limit);
        if limit == 1 || estimate_tokens(&v.to_string()) <= b {
            return v;
        }
        limit /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_text_halves_until_it_fits() {
        let mut seen = Vec::new();
        let out = fit_text(Some(10), 16, |n| {
            seen.push(n);
            "x".repeat(n * 8)
        });
        assert_eq!(seen, vec![16, 8, 4]);
        assert_eq!(out.len(), 32);
        assert_eq!(fit_text(None, 16, |n| n.to_string()), "16");
    }

    #[test]
    fn fit_json_tries_full_then_shrinks() {
        let build = |n: usize| serde_json::json!({ "items": vec![1; n.min(100)] });
        assert_eq!(
            fit_json(None, build)["items"].as_array().unwrap().len(),
            100
        );
        let v = fit_json(Some(20), build);
        let len = v["items"].as_array().unwrap().len();
        assert!(len < 100 && estimate_tokens(&v.to_string()) <= 20, "{len}");
        // Never below one item.
        assert_eq!(
            fit_json(Some(1), build)["items"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn display_paths_strips_the_base() {
        let mut g = CodeGraph::new();
        let base = std::env::temp_dir().canonicalize().unwrap();
        g.intern_file(base.join("src/lib.rs"));
        g.intern_file("/elsewhere/x.rs");
        let g = display_paths(g, &PathStyle::RelativeTo(base));
        let files: Vec<String> = g.files().map(|(_, p)| p.display().to_string()).collect();
        assert_eq!(files, vec!["src/lib.rs", "/elsewhere/x.rs"]);
    }
}
