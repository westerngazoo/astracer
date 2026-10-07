//! The `EngineTransport` seam (Manifesto Principle I / plan "interfaces
//! cerradas"). The UI talks to the analysis engine only through this trait, so
//! the exact host — Tauri today, a VS Code webview tomorrow — is swappable
//! without touching component code.
//!
//! * [`TauriTransport`] calls `window.__TAURI__.core.invoke` and subscribes to
//!   backend events via `window.__TAURI__.event.listen`.
//! * [`FixtureTransport`] is the **browser dev mode**: when the page is not
//!   hosted by Tauri (no `window.__TAURI__`), it fetches a JSON produced by
//!   `lcw analyze --format view`, so the UI can be developed, tested and
//!   screenshotted in a plain browser without the desktop shell.
//! * A future `VsCodeTransport` would implement the same trait over
//!   `acquireVsCodeApi().postMessage` + `window.addEventListener("message")`.

use lcw_core::SourceSnippet;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// A laid-out analysis result received from the backend. Mirrors the backend's
/// `GraphView`; both sides share `lcw_core::ReportSnapshot`, so the shape can't
/// drift.
#[derive(Debug, Deserialize)]
pub struct GraphView {
    pub report: lcw_core::ReportSnapshot,
    pub positions: Vec<[f32; 2]>,
    /// One box per crate. Defaults to none, so a `fixture.json` written before
    /// the layout was grouped still loads.
    #[serde(default)]
    pub groups: Vec<lcw_core::GroupBox>,
}

/// A streaming progress update emitted by the backend during analysis.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub message: String,
}

/// Request/response boundary between the UI and the analysis engine.
#[allow(async_fn_in_trait)] // single-host, never used behind `dyn`.
pub trait EngineTransport {
    /// Analyze the repository at `path`. `semantic` requests the precise
    /// (rust-analyzer) backend where available.
    async fn analyze(&self, path: String, semantic: bool) -> Result<GraphView, String>;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn tauri_listen(
        event: &str,
        handler: &Closure<dyn FnMut(JsValue)>,
    ) -> Result<JsValue, JsValue>;
}

/// Arguments for the `analyze_repo` command (keys map 1:1 to the Rust command
/// parameters; Tauri handles the JS<->Rust bridging).
#[derive(Serialize)]
struct AnalyzeArgs {
    path: String,
    semantic: bool,
}

/// The Tauri host transport.
pub struct TauriTransport;

impl EngineTransport for TauriTransport {
    async fn analyze(&self, path: String, semantic: bool) -> Result<GraphView, String> {
        let args = serde_wasm_bindgen::to_value(&AnalyzeArgs { path, semantic })
            .map_err(|e| format!("serializing args: {e}"))?;
        let value = tauri_invoke("analyze_repo", args)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding result: {e}"))
    }
}

/// Whether the page is hosted by the Tauri shell (`withGlobalTauri` injects
/// `window.__TAURI__`). When it is not, the app runs in browser dev mode and
/// analysis results come from [`FixtureTransport`].
pub fn has_tauri() -> bool {
    web_sys::window()
        .map(|w| js_sys::Reflect::has(&w, &JsValue::from_str("__TAURI__")).unwrap_or(false))
        .unwrap_or(false)
}

/// Default fixture URL for browser dev mode, relative to the served page.
pub const DEFAULT_FIXTURE: &str = "fixture.json";

/// Whether the UI can run rust-analyzer-backed analysis. Only the native Tauri
/// host runs the engine; browser dev mode reloads a pre-baked fixture.
pub fn semantic_available() -> bool {
    has_tauri()
}

/// Whether `path` names a host filesystem location rather than a URL the page
/// can fetch. Browser dev mode cannot read arbitrary paths — only HTTP(S)
/// resources served alongside the wasm bundle (typically [`DEFAULT_FIXTURE`]).
pub fn looks_like_filesystem_path(path: &str) -> bool {
    let p = path.trim();
    if p.is_empty() {
        return false;
    }
    p.starts_with('/')
        || p.starts_with('\\')
        || p.starts_with("~/")
        || (p.len() >= 2 && p.as_bytes().get(1) == Some(&b':'))
}

/// Resolve the fetch target for browser dev mode.
pub fn fixture_fetch_url(path: &str) -> Result<String, String> {
    let p = path.trim();
    if p.is_empty() || p == DEFAULT_FIXTURE {
        return Ok(DEFAULT_FIXTURE.to_string());
    }
    if looks_like_filesystem_path(p) {
        return Err(format!(
            "browser dev mode cannot analyze a repository path ({p}). \
             Leave the path empty to reload fixture.json, or run `lcw-dev ui <repo>` \
             to refresh the fixture."
        ));
    }
    Ok(p.to_string())
}

/// Browser dev mode transport: `analyze(path)` fetches `path` (or
/// [`DEFAULT_FIXTURE`] when empty) as a `GraphView` JSON, i.e. the output of
/// `lcw analyze --format view`. No engine runs in the browser; this is a
/// stand-in for the host so the UI can be exercised end to end.
pub struct FixtureTransport;

impl EngineTransport for FixtureTransport {
    async fn analyze(&self, path: String, semantic: bool) -> Result<GraphView, String> {
        if semantic {
            return Err(
                "semantic analysis requires the Tauri desktop app (rust-analyzer cannot run in \
                 the browser). Run `cargo tauri dev -- --features semantic` from \
                 apps/desktop/src-tauri, or pre-bake a fixture with `lcw-dev ui --semantic <repo>`."
                    .into(),
            );
        }
        let url = fixture_fetch_url(&path)?;
        let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
        let response = JsFuture::from(window.fetch_with_str(&url))
            .await
            .map_err(|_| {
                format!(
                    "could not load {url}. Browser dev mode only serves files from the lcw-dev \
                     ui server — leave the path empty to reload fixture.json, or run \
                     `lcw-dev ui <repo>` to refresh the analysis."
                )
            })?;
        let response: web_sys::Response = response
            .dyn_into()
            .map_err(|_| "fetch did not return a Response".to_string())?;
        if !response.ok() {
            return Err(format!("fetching {url}: HTTP {}", response.status()));
        }
        let json = JsFuture::from(response.json().map_err(js_error_to_string)?)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(json).map_err(|e| format!("decoding {url}: {e}"))
    }
}

/// Subscribe to backend progress events. The handler runs for the lifetime of
/// the app (the closure is intentionally leaked, matching the listener's
/// lifetime). A no-op outside the Tauri shell.
pub fn on_progress(mut handler: impl FnMut(Progress) + 'static) {
    if !has_tauri() {
        return;
    }
    let closure = Closure::wrap(Box::new(move |event: JsValue| {
        // The Tauri event payload lives under `event.payload`.
        if let Ok(payload) = js_sys::Reflect::get(&event, &JsValue::from_str("payload")) {
            if let Ok(progress) = serde_wasm_bindgen::from_value::<Progress>(payload) {
                handler(progress);
            }
        }
    }) as Box<dyn FnMut(JsValue)>);

    wasm_bindgen_futures::spawn_local(async move {
        // Registration resolves once the listener is attached; keep the closure
        // alive afterwards so the callback stays valid.
        let _ = tauri_listen("analyze-progress", &closure).await;
        closure.forget();
    });
}

/// Arguments for the `read_source_snippet` command.
#[derive(Serialize)]
struct ReadSourceArgs {
    file: String,
    start_line: u32,
    end_line: u32,
}

/// Read source lines for a node. Only available in the Tauri desktop shell.
pub async fn read_source_snippet(
    file: String,
    start_line: u32,
    end_line: u32,
) -> Result<SourceSnippet, String> {
    if !has_tauri() {
        return Err(
            "source viewing requires the Tauri desktop app (browser dev mode cannot read arbitrary files)"
                .into(),
        );
    }
    let args = serde_wasm_bindgen::to_value(&ReadSourceArgs {
        file,
        start_line,
        end_line,
    })
    .map_err(|e| format!("serializing args: {e}"))?;
    let value = tauri_invoke("read_source_snippet", args)
        .await
        .map_err(js_error_to_string)?;
    serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding snippet: {e}"))
}

/// Tauri rejects a command with the `Err` string; surface it verbatim, falling
/// back to a debug rendering for non-string rejections.
fn js_error_to_string(value: JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| format!("engine error: {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_paths_are_detected() {
        assert!(!looks_like_filesystem_path(""));
        assert!(!looks_like_filesystem_path("fixture.json"));
        assert!(looks_like_filesystem_path("/Users/me/repo"));
        assert!(looks_like_filesystem_path("C:\\repo"));
        assert!(looks_like_filesystem_path("~/projects/livewalk"));
    }

    #[test]
    fn fixture_url_defaults_and_rejects_host_paths() {
        assert_eq!(fixture_fetch_url("").unwrap(), DEFAULT_FIXTURE);
        assert_eq!(fixture_fetch_url("fixture.json").unwrap(), DEFAULT_FIXTURE);
        assert_eq!(fixture_fetch_url("other.json").unwrap(), "other.json");
        assert!(fixture_fetch_url("/tmp/repo").is_err());
    }
}
