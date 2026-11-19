//! # livewalk-desktop (backend)
//!
//! A deliberately *thin* Tauri v2 shell (Manifesto Principle I: the UI is a
//! front end, not a second home for logic). It exposes a couple of
//! `#[tauri::command]`s that delegate straight to [`lcw_engine`], plus progress
//! events, and hands the webview a laid-out, serializable [`GraphView`].
//!
//! All the real work — parsing, graph building, layout — lives in the reusable
//! crates; the commands call `lcw_desktop_api::host`, the same functions the
//! VS Code extension reaches through `lcw view-data` / `lcw snippet`.

mod llm;

use lcw_core::SourceSnippet;
use lcw_desktop_api::{
    host, AnalyzeArgs, GraphView, LanguageChoice, Progress, ReadSourceSnippetArgs,
};
use tauri::{Emitter, Window};

/// Event channel the frontend subscribes to for streaming progress.
const PROGRESS_EVENT: &str = "analyze-progress";

fn emit(window: &Window, progress: Progress) {
    // A dropped event is non-fatal (e.g. the window closed mid-analysis).
    let _ = window.emit(PROGRESS_EVENT, progress);
}

/// Analyze the repository at `path`, returning a laid-out graph.
///
/// `semantic` requests the (feature-gated) rust-analyzer backend; the engine
/// transparently falls back to the fast tree-sitter adapter when it isn't
/// compiled in. The work itself is [`host::analyze_view`], shared with
/// `lcw view-data` (the VS Code host).
#[tauri::command]
async fn analyze_repo(
    window: Window,
    path: String,
    semantic: bool,
    use_cache: bool,
    language: Option<String>,
) -> Result<GraphView, String> {
    let args = AnalyzeArgs {
        path,
        semantic,
        use_cache,
        language,
    };
    // Analysis is CPU-bound (parsing + O(n^2) layout), so run it on the blocking
    // pool to keep the webview responsive and progress events flowing.
    tauri::async_runtime::spawn_blocking(move || {
        host::analyze_view(&args, &mut |p| emit(&window, p))
    })
    .await
    .map_err(|e| format!("analysis task panicked: {e}"))?
}

/// List fast-mode languages detected under `path` (by source file extensions).
#[tauri::command]
fn discover_repo_languages(path: String) -> Result<Vec<LanguageChoice>, String> {
    host::discover_language_choices(&path)
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
    host::read_source_snippet(&ReadSourceSnippetArgs {
        file,
        start_line,
        end_line,
        repo_root,
        whole_file,
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
        let snip = host::read_source_snippet(&ReadSourceSnippetArgs {
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
