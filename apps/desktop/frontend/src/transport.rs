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
//! * [`VsCodeTransport`] runs inside a VS Code / Cursor webview: the
//!   extension installs `window.livewalkVsCode` (see `apps/vscode/webview/
//!   bridge.js`), which turns each call into an id-tagged
//!   `acquireVsCodeApi().postMessage` request and resolves it from the
//!   extension's `response` message. The extension answers by running `lcw`.
//!
//! [`host`] detects which one applies at startup; [`active`] picks it.

use lcw_core::SourceSnippet;
use lcw_desktop_api::{
    AnalyzeArgs, DiscoverLanguagesArgs, GraphView, LanguageChoice, Progress, ReadSourceSnippetArgs,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

pub use lcw_desktop_api::{ExplainContext, ExplainMetrics};

/// Request/response boundary between the UI and the analysis engine.
#[allow(async_fn_in_trait)] // single-host, never used behind `dyn`.
pub trait EngineTransport {
    /// Analyze the repository at `path`. `semantic` requests the precise
    /// (rust-analyzer) backend where available. `use_cache` loads `graph.sqlite`
    /// instead of parsing (Tauri host only). `language` overrides fast-mode
    /// adapter (`rust`, `cpp`, …) without editing `livewalk.toml`.
    async fn analyze(
        &self,
        path: String,
        semantic: bool,
        use_cache: bool,
        language: Option<String>,
    ) -> Result<GraphView, String>;

    /// Scan `path` for source extensions and list matching adapters (Tauri only).
    async fn discover_languages(&self, path: String) -> Result<Vec<LanguageChoice>, String>;

    /// Read source lines for a node (Tauri host only).
    async fn read_source_snippet(
        &self,
        file: String,
        start_line: u32,
        end_line: u32,
        repo_root: String,
        whole_file: bool,
    ) -> Result<SourceSnippet, String>;

    /// Ask an LLM to explain a function. `provider` is `ollama` or `gemini`.
    async fn explain_function(
        &self,
        context: ExplainContext,
        provider: String,
    ) -> Result<String, String>;
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

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "dialog"], js_name = open, catch)]
    async fn tauri_dialog_open(options: JsValue) -> Result<JsValue, JsValue>;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "livewalkVsCode"], js_name = request, catch)]
    async fn vscode_request(method: &str, params: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "livewalkVsCode"], js_name = onProgress)]
    fn vscode_on_progress(handler: &Closure<dyn FnMut(JsValue)>);

    #[wasm_bindgen(js_namespace = ["window", "livewalkVsCode"], js_name = onFocus)]
    fn vscode_on_focus(handler: &Closure<dyn FnMut(JsValue)>);
}

/// The editor asks the graph to select the function at `file:line` (VS Code
/// `focus` message): `file` is absolute, `line` 1-based; without a line, the
/// file's first function.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FocusRequest {
    pub file: String,
    pub line: Option<u32>,
}

/// Error string of an analyze the user cancelled in the host (VS Code's
/// progress notification).
pub const CANCELLED: &str = "cancelled";

/// Which process hosts the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// The Tauri desktop shell (`window.__TAURI__`).
    Tauri,
    /// A VS Code / Cursor webview with the Livewalk bridge installed.
    VsCode,
    /// A plain browser: browser dev mode over a fixture.
    Browser,
}

/// Global the VS Code extension's bridge script installs before the wasm loads.
const VSCODE_BRIDGE: &str = "livewalkVsCode";

fn window_has(key: &str) -> bool {
    web_sys::window()
        .map(|w| js_sys::Reflect::has(&w, &JsValue::from_str(key)).unwrap_or(false))
        .unwrap_or(false)
}

/// Detect the host from the globals each one injects.
pub fn host() -> Host {
    if window_has("__TAURI__") {
        Host::Tauri
    } else if window_has(VSCODE_BRIDGE) {
        Host::VsCode
    } else {
        Host::Browser
    }
}

/// Whether a real engine answers requests (Tauri or VS Code), as opposed to
/// browser dev mode's pre-baked fixture.
pub fn has_engine() -> bool {
    host() != Host::Browser
}

/// The repository the host wants analyzed on load (VS Code: the workspace
/// folder), if it named one.
pub fn initial_repo_path() -> Option<String> {
    bridge_get("initialRepo")?
        .as_string()
        .filter(|s| !s.trim().is_empty())
}

/// The language the host chose for the initial load (VS Code), if any.
pub fn initial_language() -> Option<String> {
    bridge_get("initialLanguage")?
        .as_string()
        .filter(|s| !s.trim().is_empty())
}

/// Whether the initial load should open the saved graph (the host found it
/// fresh) rather than parse.
pub fn initial_use_cache() -> bool {
    bridge_get("initialCached").is_some_and(|v| v.is_truthy())
}

/// The function the host wants selected once the initial load finishes.
pub fn initial_focus() -> Option<FocusRequest> {
    let v = bridge_get("initialFocus")?;
    if v.is_null() || v.is_undefined() {
        return None;
    }
    serde_wasm_bindgen::from_value(v).ok()
}

fn bridge_get(key: &str) -> Option<JsValue> {
    if host() != Host::VsCode {
        return None;
    }
    let window = web_sys::window()?;
    let bridge = js_sys::Reflect::get(&window, &JsValue::from_str(VSCODE_BRIDGE)).ok()?;
    js_sys::Reflect::get(&bridge, &JsValue::from_str(key)).ok()
}

/// Subscribe to the host's focus requests (VS Code only; a no-op elsewhere).
/// The closure lives as long as the app, like [`on_progress`]'s.
pub fn on_focus(mut handler: impl FnMut(FocusRequest) + 'static) {
    if host() != Host::VsCode {
        return;
    }
    let closure = Closure::wrap(Box::new(move |payload: JsValue| {
        if let Ok(req) = serde_wasm_bindgen::from_value::<FocusRequest>(payload) {
            handler(req);
        }
    }) as Box<dyn FnMut(JsValue)>);
    vscode_on_focus(&closure);
    closure.forget();
}

/// The transport for the detected [`host`].
pub fn active() -> ActiveTransport {
    match host() {
        Host::Tauri => ActiveTransport::Tauri(TauriTransport),
        Host::VsCode => ActiveTransport::VsCode(VsCodeTransport),
        Host::Browser => ActiveTransport::Fixture(FixtureTransport),
    }
}

/// Static dispatch over the hosts, so components call one transport.
pub enum ActiveTransport {
    Tauri(TauriTransport),
    VsCode(VsCodeTransport),
    Fixture(FixtureTransport),
}

impl EngineTransport for ActiveTransport {
    async fn analyze(
        &self,
        path: String,
        semantic: bool,
        use_cache: bool,
        language: Option<String>,
    ) -> Result<GraphView, String> {
        match self {
            Self::Tauri(t) => t.analyze(path, semantic, use_cache, language).await,
            Self::VsCode(t) => t.analyze(path, semantic, use_cache, language).await,
            Self::Fixture(t) => t.analyze(path, semantic, use_cache, language).await,
        }
    }

    async fn discover_languages(&self, path: String) -> Result<Vec<LanguageChoice>, String> {
        match self {
            Self::Tauri(t) => t.discover_languages(path).await,
            Self::VsCode(t) => t.discover_languages(path).await,
            Self::Fixture(t) => t.discover_languages(path).await,
        }
    }

    async fn read_source_snippet(
        &self,
        file: String,
        start_line: u32,
        end_line: u32,
        repo_root: String,
        whole_file: bool,
    ) -> Result<SourceSnippet, String> {
        match self {
            Self::Tauri(t) => {
                t.read_source_snippet(file, start_line, end_line, repo_root, whole_file)
                    .await
            }
            Self::VsCode(t) => {
                t.read_source_snippet(file, start_line, end_line, repo_root, whole_file)
                    .await
            }
            Self::Fixture(t) => {
                t.read_source_snippet(file, start_line, end_line, repo_root, whole_file)
                    .await
            }
        }
    }

    async fn explain_function(
        &self,
        context: ExplainContext,
        provider: String,
    ) -> Result<String, String> {
        match self {
            Self::Tauri(t) => t.explain_function(context, provider).await,
            Self::VsCode(t) => t.explain_function(context, provider).await,
            Self::Fixture(t) => t.explain_function(context, provider).await,
        }
    }
}

/// The Tauri host transport.
pub struct TauriTransport;

/// Arguments for the `explain_function` command.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExplainArgs {
    context: ExplainContext,
    provider: String,
}

impl EngineTransport for TauriTransport {
    async fn analyze(
        &self,
        path: String,
        semantic: bool,
        use_cache: bool,
        language: Option<String>,
    ) -> Result<GraphView, String> {
        let args = serde_wasm_bindgen::to_value(&AnalyzeArgs {
            path,
            semantic,
            use_cache,
            language,
        })
        .map_err(|e| format!("serializing args: {e}"))?;
        let value = tauri_invoke("analyze_repo", args)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding result: {e}"))
    }

    async fn discover_languages(&self, path: String) -> Result<Vec<LanguageChoice>, String> {
        let args = serde_wasm_bindgen::to_value(&DiscoverLanguagesArgs { path })
            .map_err(|e| format!("serializing args: {e}"))?;
        let value = tauri_invoke("discover_repo_languages", args)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding result: {e}"))
    }

    async fn read_source_snippet(
        &self,
        file: String,
        start_line: u32,
        end_line: u32,
        repo_root: String,
        whole_file: bool,
    ) -> Result<SourceSnippet, String> {
        let args = serde_wasm_bindgen::to_value(&ReadSourceSnippetArgs {
            file,
            start_line,
            end_line,
            repo_root,
            whole_file,
        })
        .map_err(|e| format!("serializing args: {e}"))?;
        let value = tauri_invoke("read_source_snippet", args)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding snippet: {e}"))
    }

    async fn explain_function(
        &self,
        context: ExplainContext,
        provider: String,
    ) -> Result<String, String> {
        let args = serde_wasm_bindgen::to_value(&ExplainArgs { context, provider })
            .map_err(|e| format!("serializing args: {e}"))?;
        let value = tauri_invoke("explain_function", args)
            .await
            .map_err(js_error_to_string)?;
        value
            .as_string()
            .ok_or_else(|| format!("unexpected explain result: {value:?}"))
    }
}

/// The VS Code / Cursor webview transport: every call is one request through
/// the extension's bridge (`window.livewalkVsCode.request(method, params)`),
/// answered by the extension host running `lcw`.
pub struct VsCodeTransport;

/// Arguments for the VS Code `reveal` request: open `file` in an editor and
/// select `line..=end_line` (1-based).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevealArgs {
    file: String,
    line: u32,
    end_line: u32,
    repo_root: String,
    qualified_name: String,
}

/// Arguments for the VS Code `pickRepoFolder` request.
#[derive(Serialize)]
struct PickFolderArgs {
    current: String,
}

impl VsCodeTransport {
    async fn call<A: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        args: &A,
    ) -> Result<R, String> {
        let params = args
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|e| format!("serializing {method} args: {e}"))?;
        let value = vscode_request(method, params)
            .await
            .map_err(js_error_to_string)?;
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decoding {method} result: {e}"))
    }
}

impl EngineTransport for VsCodeTransport {
    async fn analyze(
        &self,
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
        self.call("analyze", &args).await
    }

    async fn discover_languages(&self, path: String) -> Result<Vec<LanguageChoice>, String> {
        self.call("discoverLanguages", &DiscoverLanguagesArgs { path })
            .await
    }

    async fn read_source_snippet(
        &self,
        file: String,
        start_line: u32,
        end_line: u32,
        repo_root: String,
        whole_file: bool,
    ) -> Result<SourceSnippet, String> {
        let args = ReadSourceSnippetArgs {
            file,
            start_line,
            end_line,
            repo_root,
            whole_file,
        };
        self.call("readSourceSnippet", &args).await
    }

    async fn explain_function(
        &self,
        context: ExplainContext,
        provider: String,
    ) -> Result<String, String> {
        self.call("explainFunction", &ExplainArgs { context, provider })
            .await
    }
}

/// Whether "open in editor" is available (VS Code / Cursor only).
pub fn can_reveal() -> bool {
    host() == Host::VsCode
}

/// Ask the editor host to open `file` and select the definition span.
pub async fn reveal_in_editor(
    file: String,
    line: u32,
    end_line: u32,
    repo_root: String,
    qualified_name: String,
) -> Result<(), String> {
    if !can_reveal() {
        return Err("open in editor requires the VS Code / Cursor extension".into());
    }
    let args = RevealArgs {
        file,
        line,
        end_line,
        repo_root,
        qualified_name,
    };
    let _: serde::de::IgnoredAny = VsCodeTransport.call("reveal", &args).await?;
    Ok(())
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
    has_engine()
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
    async fn read_source_snippet(
        &self,
        _file: String,
        _start_line: u32,
        _end_line: u32,
        _repo_root: String,
        _whole_file: bool,
    ) -> Result<SourceSnippet, String> {
        Err(
            "source viewing requires the Tauri desktop app (browser dev mode cannot read arbitrary files)"
                .into(),
        )
    }

    async fn explain_function(
        &self,
        _context: ExplainContext,
        _provider: String,
    ) -> Result<String, String> {
        Err(
            "LLM explanation requires the Tauri desktop app (Ollama/Gemini cannot run in the browser)"
                .into(),
        )
    }

    async fn discover_languages(&self, _path: String) -> Result<Vec<LanguageChoice>, String> {
        Ok(vec![LanguageChoice {
            id: "rust".into(),
            label: "Rust".into(),
        }])
    }

    async fn analyze(
        &self,
        path: String,
        semantic: bool,
        use_cache: bool,
        _language: Option<String>,
    ) -> Result<GraphView, String> {
        if use_cache {
            return Err(
                "cached load requires the Tauri desktop app (run a prior `lcw analyze` first)"
                    .into(),
            );
        }
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

/// Open a native folder picker (Tauri) or a `prompt()` fallback in browser dev
/// mode. Returns `Ok(None)` when the user cancels.
pub async fn pick_repo_folder(current: &str) -> Result<Option<String>, String> {
    if host() == Host::VsCode {
        let picked: Option<String> = VsCodeTransport
            .call(
                "pickRepoFolder",
                &PickFolderArgs {
                    current: current.trim().to_string(),
                },
            )
            .await?;
        return Ok(picked);
    }
    if has_tauri() {
        let options = js_sys::Object::new();
        js_sys::Reflect::set(&options, &JsValue::from_str("directory"), &JsValue::TRUE)
            .map_err(|_| "building dialog options".to_string())?;
        js_sys::Reflect::set(&options, &JsValue::from_str("multiple"), &JsValue::FALSE)
            .map_err(|_| "building dialog options".to_string())?;
        js_sys::Reflect::set(
            &options,
            &JsValue::from_str("title"),
            &JsValue::from_str("Select repository"),
        )
        .map_err(|_| "building dialog options".to_string())?;
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            js_sys::Reflect::set(
                &options,
                &JsValue::from_str("defaultPath"),
                &JsValue::from_str(trimmed),
            )
            .map_err(|_| "building dialog options".to_string())?;
        }
        let value = tauri_dialog_open(options.into())
            .await
            .map_err(js_error_to_string)?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        value
            .as_string()
            .ok_or_else(|| format!("unexpected folder pick result: {value:?}"))
            .map(Some)
    } else {
        let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
        let default = if current.trim().is_empty() {
            DEFAULT_FIXTURE
        } else {
            current.trim()
        };
        match window.prompt_with_message_and_default(
            "Repository path or fixture URL (browser dev mode has no native folder picker):",
            default,
        ) {
            Ok(Some(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
            Ok(_) => Ok(None),
            Err(_) => Ok(None),
        }
    }
}

/// Host-aware language discovery for the toolbar chips.
pub async fn discover_repo_languages(path: String) -> Result<Vec<LanguageChoice>, String> {
    active().discover_languages(path).await
}

/// Host-aware entry point for source reads (delegates to [`EngineTransport`]).
pub async fn read_source_snippet(
    file: String,
    start_line: u32,
    end_line: u32,
    repo_root: String,
    whole_file: bool,
) -> Result<SourceSnippet, String> {
    active()
        .read_source_snippet(file, start_line, end_line, repo_root, whole_file)
        .await
}

/// Subscribe to backend progress events. The handler runs for the lifetime of
/// the app (the closure is intentionally leaked, matching the listener's
/// lifetime). A no-op in browser dev mode.
pub fn on_progress(mut handler: impl FnMut(Progress) + 'static) {
    if host() == Host::VsCode {
        // The bridge hands over the `progress` message payload directly.
        let closure = Closure::wrap(Box::new(move |payload: JsValue| {
            if let Ok(progress) = serde_wasm_bindgen::from_value::<Progress>(payload) {
                handler(progress);
            }
        }) as Box<dyn FnMut(JsValue)>);
        vscode_on_progress(&closure);
        closure.forget();
        return;
    }
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

/// Tauri (and the VS Code bridge) reject with the `Err` string; surface it verbatim, falling
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
