//! Native host operations behind the UI's `EngineTransport` (feature `host`).
//!
//! Every host — the Tauri shell, and the VS Code extension through
//! `lcw view-data` / `lcw snippet` / `lcw languages` — answers the frontend
//! with these functions, so the UI sees byte-for-byte the same `GraphView`
//! whichever process serves it. They only orchestrate: analysis is
//! `lcw-engine`'s, layout `lcw-layout`'s.

use std::path::{Path, PathBuf};

use lcw_config::{AdapterMode, Config, Language};
use lcw_core::{AnalysisReport, SourceSnippet};
use lcw_engine::{Engine, EngineError, Progress as EngineProgress};
use lcw_layout::LayoutParams;

use crate::{AnalyzeArgs, GraphView, LanguageChoice, Progress, ReadSourceSnippetArgs};

/// Context lines around a snippet's span, and the cap on returned lines.
const SNIPPET_CONTEXT: u32 = 5;
const SNIPPET_MAX_LINES: u32 = 500;

fn progress(phase: &str, message: impl Into<String>) -> Progress {
    Progress {
        phase: phase.to_string(),
        message: message.into(),
    }
}

fn canonical_root(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("repository path is empty".into());
    }
    PathBuf::from(trimmed)
        .canonicalize()
        .map_err(|e| format!("repository path not found or not accessible: {e}"))
}

/// Analyze (or, with `use_cache`, load) the repository at `args.path` and lay
/// it out one box per crate. `on_progress` receives the same phases the
/// desktop status line shows.
pub fn analyze_view(
    args: &AnalyzeArgs,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<GraphView, String> {
    let root = canonical_root(&args.path)?;

    on_progress(progress("config", "loading configuration"));
    let mut config = Config::resolve(None, &root).map_err(|e| e.to_string())?;
    if args.semantic && !args.use_cache {
        config.adapter.mode = AdapterMode::Semantic;
    }
    if let Some(lang) = &args.language {
        let lang = lang
            .parse::<Language>()
            .map_err(|e| format!("invalid language: {e}"))?;
        config.adapter.language = lang;
    }

    let engine = Engine::new(config);
    let report = if args.use_cache {
        load_cached_or_analyze(&engine, &root, on_progress)?
    } else {
        run_full_analyze(&engine, &root, on_progress)?
    };

    on_progress(progress(
        "layout",
        format!("laying out {} nodes", report.graph.node_count()),
    ));
    // One box per crate, crates in rows by call order. Each crate is its own
    // (smaller) simulation, so this is also cheaper than one over everything.
    let laid = lcw_layout::layout_by_crate(&report.graph, &LayoutParams::default());

    on_progress(progress("done", "analysis complete"));
    Ok(GraphView {
        report: report.snapshot(),
        positions: laid.positions,
        groups: laid.groups,
        repo_root: root.to_string_lossy().into_owned(),
    })
}

/// Try `graph.sqlite` first; if there is no saved graph yet (common when
/// "cached" is checked before the first successful analyze), fall back to a
/// full parse instead of failing with `NoCachedGraph`.
fn load_cached_or_analyze(
    engine: &Engine,
    root: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<AnalysisReport, String> {
    on_progress(progress(
        "load",
        "loading cached graph (no parse; may be stale if sources changed)",
    ));
    match engine.load_from_store(root) {
        Ok(report) => Ok(report),
        Err(EngineError::NoCachedGraph { path }) => {
            on_progress(progress(
                "analyze",
                format!(
                    "no saved graph at {} — running full analysis (saved graph appears after this finishes)",
                    path.display()
                ),
            ));
            run_full_analyze(engine, root, on_progress)
        }
        Err(EngineError::GraphStoreDisabled) => {
            on_progress(progress(
                "analyze",
                "graph store disabled in livewalk.toml — running full analysis",
            ));
            run_full_analyze(engine, root, on_progress)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn run_full_analyze(
    engine: &Engine,
    root: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<AnalysisReport, String> {
    engine
        .analyze_with_progress(root, &mut |p| on_progress(engine_progress(p)))
        .map_err(|e| e.to_string())
}

/// Translate an engine progress event into a UI progress event.
fn engine_progress(p: EngineProgress) -> Progress {
    let (phase, message) = match p {
        EngineProgress::Discovering => ("discover", "scanning repository".to_string()),
        EngineProgress::Discovered { files } => ("discover", format!("found {files} files")),
        EngineProgress::Reading { files } => ("read", format!("reading {files} files")),
        EngineProgress::Parsing { files } => ("parse", format!("parsing {files} files")),
        EngineProgress::Parsed { nodes, edges } => {
            ("parse", format!("parsed {nodes} nodes / {edges} edges"))
        }
        EngineProgress::Analyzing => ("analyze", "running lenses & metrics".to_string()),
        EngineProgress::Advising => ("advise", "detecting vertical & suggestions".to_string()),
        EngineProgress::Done { nodes, .. } => ("graph", format!("graph ready ({nodes} nodes)")),
    };
    progress(phase, message)
}

/// Fast-mode languages detected under `path` (by source file extensions), or
/// the configured default when none are found.
pub fn discover_language_choices(path: &str) -> Result<Vec<LanguageChoice>, String> {
    let root = canonical_root(path)?;
    let mut langs = lcw_engine::discover_languages(&root).map_err(|e| e.to_string())?;
    if langs.is_empty() {
        let cfg = Config::resolve(None, &root).map_err(|e| e.to_string())?;
        langs.push(cfg.adapter.language);
    }
    Ok(langs
        .into_iter()
        .map(|l| LanguageChoice {
            id: l.cache_suffix().into(),
            label: l.label().into(),
        })
        .collect())
}

/// Read a source snippet for the detail pane's Code tab: the node's span
/// (1-based, inclusive) plus a few context lines, capped so the webview stays
/// responsive; or the whole file (same cap) with `whole_file`.
pub fn read_source_snippet(args: &ReadSourceSnippetArgs) -> Result<SourceSnippet, String> {
    let resolved = lcw_core::resolve_source_path(&args.repo_root, &args.file);
    let path = resolved.to_string_lossy().into_owned();
    let (start, end) = if args.whole_file {
        (1, u32::MAX)
    } else {
        (args.start_line, args.end_line)
    };
    lcw_core::read_snippet(&path, start, end, SNIPPET_CONTEXT, SNIPPET_MAX_LINES).ok_or_else(|| {
        if args.file.is_empty() || path.is_empty() {
            "no source file for this node".into()
        } else if !args.repo_root.is_empty() && !PathBuf::from(&path).exists() {
            format!(
                "could not read {} (resolved from {} under repo root {})",
                args.file, path, args.repo_root
            )
        } else {
            format!("could not read {path} (from {})", args.file)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lcw-host-{tag}-{nanos}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() { b(); }\nfn b() {}\n").unwrap();
        dir
    }

    #[test]
    fn snippet_reads_repo_relative_file() {
        let dir = temp_repo("snip");
        let snip = read_source_snippet(&ReadSourceSnippetArgs {
            file: "src/lib.rs".into(),
            start_line: 2,
            end_line: 2,
            repo_root: dir.to_string_lossy().into_owned(),
            whole_file: false,
        })
        .unwrap();
        assert_eq!(snip.highlight_start, 2);
        assert_eq!(snip.lines[1].text, "fn b() {}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn snippet_reports_missing_file() {
        let err = read_source_snippet(&ReadSourceSnippetArgs {
            file: "nope.rs".into(),
            start_line: 1,
            end_line: 1,
            repo_root: "/definitely/not/here".into(),
            whole_file: false,
        })
        .unwrap_err();
        assert!(err.contains("could not read nope.rs"), "{err}");
    }

    #[test]
    fn empty_path_is_rejected() {
        let err = analyze_view(
            &AnalyzeArgs {
                path: "  ".into(),
                semantic: false,
                use_cache: false,
                language: None,
            },
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err, "repository path is empty");
    }

    #[test]
    fn languages_found_by_extension() {
        let dir = temp_repo("langs");
        let langs = discover_language_choices(&dir.to_string_lossy()).unwrap();
        assert!(langs.iter().any(|l| l.id == "rust"), "{langs:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
