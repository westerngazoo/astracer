//! # livewalk-desktop (backend)
//!
//! A deliberately *thin* Tauri v2 shell (Manifesto Principle I: the UI is a
//! front end, not a second home for logic). It exposes a couple of
//! `#[tauri::command]`s that delegate straight to [`lcw_engine`], plus progress
//! events, and hands the webview a laid-out, serializable [`GraphView`].
//!
//! All the real work — parsing, graph building, layout — lives in the reusable
//! crates. Swapping this shell for a VS Code extension host later means
//! re-implementing only the transport, not the analysis.

use std::path::PathBuf;

mod llm;

use lcw_config::{AdapterMode, Config, Language};
use lcw_core::SourceSnippet;
use lcw_desktop_api::{GraphView, LanguageChoice, Progress, ReadSourceSnippetArgs};
use lcw_engine::{discover_languages, Engine, EngineError, Progress as EngineProgress};
use lcw_layout::LayoutParams;
use tauri::{Emitter, Window};

/// Event channel the frontend subscribes to for streaming progress.
const PROGRESS_EVENT: &str = "analyze-progress";

fn emit(window: &Window, phase: &str, message: impl Into<String>) {
    let progress = Progress {
        phase: phase.to_string(),
        message: message.into(),
    };
    // A dropped event is non-fatal (e.g. the window closed mid-analysis).
    let _ = window.emit(PROGRESS_EVENT, progress);
}

/// Analyze the repository at `path`, returning a laid-out graph.
///
/// `semantic` requests the (feature-gated) rust-analyzer backend; the engine
/// transparently falls back to the fast tree-sitter adapter when it isn't
/// compiled in.
#[tauri::command]
async fn analyze_repo(
    window: Window,
    path: String,
    semantic: bool,
    use_cache: bool,
    language: Option<String>,
) -> Result<GraphView, String> {
    // Analysis is CPU-bound (parsing + O(n^2) layout), so run it on the blocking
    // pool to keep the webview responsive and progress events flowing.
    tauri::async_runtime::spawn_blocking(move || {
        analyze_blocking(window, path, semantic, use_cache, language)
    })
    .await
    .map_err(|e| format!("analysis task panicked: {e}"))?
}

/// List fast-mode languages detected under `path` (by source file extensions).
#[tauri::command]
fn discover_repo_languages(path: String) -> Result<Vec<LanguageChoice>, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("repository path is empty".into());
    }
    let root = PathBuf::from(trimmed);
    let root = root
        .canonicalize()
        .map_err(|e| format!("repository path not found or not accessible: {e}"))?;

    let mut langs = discover_languages(&root).map_err(|e| e.to_string())?;
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

fn analyze_blocking(
    window: Window,
    path: String,
    semantic: bool,
    use_cache: bool,
    language: Option<String>,
) -> Result<GraphView, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("repository path is empty".into());
    }
    let root = PathBuf::from(trimmed);
    let root = root
        .canonicalize()
        .map_err(|e| format!("repository path not found or not accessible: {e}"))?;

    emit(&window, "config", "loading configuration");
    let mut config = Config::resolve(None, &root).map_err(|e| e.to_string())?;
    if semantic && !use_cache {
        config.adapter.mode = AdapterMode::Semantic;
    }
    if let Some(lang) = language {
        let lang = lang
            .parse::<Language>()
            .map_err(|e| format!("invalid language: {e}"))?;
        config.adapter.language = lang;
    }

    let engine = Engine::new(config);
    let report = if use_cache {
        load_cached_or_analyze(&window, &engine, &root)?
    } else {
        run_full_analyze(&window, &engine, &root)?
    };

    emit(
        &window,
        "layout",
        format!("laying out {} nodes", report.graph.node_count()),
    );
    // One box per crate, crates in rows by call order. Each crate is its own
    // (smaller) simulation, so this is also cheaper than one over everything.
    let laid = lcw_layout::layout_by_crate(&report.graph, &LayoutParams::default());

    emit(&window, "done", "analysis complete");
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
    window: &Window,
    engine: &Engine,
    root: &std::path::Path,
) -> Result<lcw_core::AnalysisReport, String> {
    emit(
        window,
        "load",
        "loading cached graph (no parse; may be stale if sources changed)",
    );
    match engine.load_from_store(root) {
        Ok(report) => Ok(report),
        Err(EngineError::NoCachedGraph { path }) => {
            emit(
                window,
                "analyze",
                format!(
                    "no saved graph at {} — running full analysis (saved graph appears after this finishes)",
                    path.display()
                ),
            );
            run_full_analyze(window, engine, root)
        }
        Err(EngineError::GraphStoreDisabled) => {
            emit(
                window,
                "analyze",
                "graph store disabled in livewalk.toml — running full analysis",
            );
            run_full_analyze(window, engine, root)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn run_full_analyze(
    window: &Window,
    engine: &Engine,
    root: &std::path::Path,
) -> Result<lcw_core::AnalysisReport, String> {
    engine
        .analyze_with_progress(root, &mut |p| emit_progress(window, p))
        .map_err(|e| e.to_string())
}

/// Translate an engine progress event into a UI progress event.
fn emit_progress(window: &Window, progress: EngineProgress) {
    let (phase, message) = match progress {
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
    emit(window, phase, message);
}

/// Read a source snippet for the detail pane's Code tab.
///
/// `start_line`/`end_line` are the node's span (1-based, inclusive). A few
/// context lines are included and the result is capped so the webview stays
/// responsive.
#[tauri::command]
fn read_source_snippet(
    file: String,
    start_line: u32,
    end_line: u32,
    repo_root: String,
    whole_file: bool,
) -> Result<SourceSnippet, String> {
    read_source_snippet_impl(ReadSourceSnippetArgs {
        file,
        start_line,
        end_line,
        repo_root,
        whole_file,
    })
}

fn read_source_snippet_impl(args: ReadSourceSnippetArgs) -> Result<SourceSnippet, String> {
    const CONTEXT: u32 = 5;
    const MAX_LINES: u32 = 500;
    let resolved = lcw_core::resolve_source_path(&args.repo_root, &args.file);
    let path = resolved.to_string_lossy().into_owned();
    let (start, end) = if args.whole_file {
        (1, u32::MAX)
    } else {
        (args.start_line, args.end_line)
    };
    lcw_core::read_snippet(&path, start, end, CONTEXT, MAX_LINES).ok_or_else(|| {
        if args.file.is_empty() {
            "no source file for this node".into()
        } else if path.is_empty() {
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

/// Ask an LLM to explain a function from the assembled [`llm::ExplainContext`].
///
/// `provider` is `ollama` or `gemini`. Runs on the blocking pool so the webview
/// stays responsive during the (up to 60s) HTTP call.
#[tauri::command]
async fn explain_function(
    context: lcw_desktop_api::ExplainContext,
    provider: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || llm::explain_function(context, &provider))
        .await
        .map_err(|e| format!("explanation task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_source_snippet_command_reads_file() {
        let dir = std::env::temp_dir().join("lcw_tauri_snippet");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cmd.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "fn main() {{}}").unwrap();
        }
        let p = path.to_string_lossy().into_owned();
        let snip = read_source_snippet_impl(ReadSourceSnippetArgs {
            file: p,
            start_line: 1,
            end_line: 1,
            repo_root: String::new(),
            whole_file: false,
        })
        .unwrap();
        assert_eq!(snip.lines.len(), 1);
        assert_eq!(snip.lines[0].text, "fn main() {}");
    }
}

/// Build and run the Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            analyze_repo,
            discover_repo_languages,
            read_source_snippet,
            explain_function
        ])
        .run(tauri::generate_context!())
        .expect("error while running Live Code Walk");
}
