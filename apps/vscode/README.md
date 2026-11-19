# Livewalk for VS Code / Cursor

The Live Code Walk call graph inside the editor: the same Explorer, canvas,
Code and Flow views as the desktop app, in a webview panel, plus jump-to-source
and a few editor integrations. All answers come from the `lcw` CLI; the
extension never traverses the graph itself.

## How it fits together

```
webview (Leptos/wasm frontend, VsCodeTransport)
   │  postMessage {type:"request", id, method, params}
   ▼
webview/bridge.js  ──►  extension host (src/panel.ts)
                          │  spawns lcw … --json
                          ▼
                        lcw view-data | snippet | languages | explain-prompt
                          │  (lcw_desktop_api::host — the same code the
                          │   Tauri commands call)
                          ▼
                        {type:"response", id, ok, result | resultJson | error}
                        {type:"progress", payload:{phase, message}}
                        {type:"focus", file, line?}   (editor → graph selection)
```

* The panel loads the Trunk build of `apps/desktop/frontend` from `media/`.
  Asset URLs are rewritten with `asWebviewUri`; the CSP allows only nonce'd
  scripts, webview resources and `'wasm-unsafe-eval'`.
* `bridge.js` installs `window.livewalkVsCode` before the wasm loads. The
  frontend detects it (`transport::host()` → `Host::VsCode`) and routes every
  `EngineTransport` call through it: `analyze` (→ `lcw view-data --json
  --progress`), `discoverLanguages` (→ `lcw languages`), `readSourceSnippet`
  (→ `lcw snippet`), `explainFunction` (→ `lcw explain-prompt`, then Ollama or
  Gemini from the extension host), `pickRepoFolder` (→ the editor's folder
  dialog) and `reveal` (open the file and select the function's span).
* There is one panel per project root (titled `Livewalk: <folder>`). It opens
  from the saved graph only when `lcw graph --json` reports it fresh (no
  source file newer than the store); otherwise it parses.
* While a load runs, the panel shows an overlay over the canvas (animated
  graph, project path, the current stage and a progress bar) and the editor
  shows a cancellable *Livewalk: loading <folder>* notification with the same
  stages. Cancel kills `lcw`; the overlay then says *Cancelled* and offers
  **Retry**. Errors show in the same place.
* `focus` messages select a function: `lcw_query::focus_target` picks the
  innermost function whose span contains the line (else the file's first
  function), and the panel selects it, centers the camera on it and opens its
  **Code** tab.

## Open Graph Here (right-click)

**Livewalk: Open Graph Here** is in the editor context menu, the editor tab
context menu and the Explorer context menu (files and folders), and in the
command palette.

1. The project root is resolved from what you clicked: the nearest folder with
   `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `CMakeLists.txt`,
   `compile_commands.json` or `.git`, never above the workspace folder. A
   member crate's `Cargo.toml` yields to the enclosing `[workspace]` one. A
   folder that has a marker is used as is.
2. If the project has several languages (`lcw languages`), the clicked file's
   language is used (`.rs` → Rust, `.cpp`/`.h`/`.hpp` → C++, `.ts` →
   TypeScript, `.py` → Python, `.go` → Go); otherwise you pick one.
3. A dialog asks *Load project graph for <root>?*, saying when a fresh saved
   graph makes it instant. Buttons: **Load**, **Choose another folder…** (a
   folder picker), **Load (don't ask again)** (turns off
   `livewalk.confirmBeforeLoad`) and Cancel.
4. The panel opens (or the existing one for that root comes forward), loads,
   and then shows what you were looking at: from the editor, the function
   under the cursor; from a file in the Explorer, the file's first function;
   from a folder, just the graph.

**Livewalk: Reveal in Graph** (editor context menu and palette) selects the
function under the cursor in its project's panel, opening the panel first if
needed. With `livewalk.followCursor` on, the open panel follows the cursor
(after a short pause) without stealing focus.

## Build and install

Needs Rust (with the `wasm32-unknown-unknown` target), `trunk`, Node 20+ and npm.
From the repository root, one step builds `lcw`, the frontend bundle and the
`.vsix`:

```sh
scripts/livewalk-dev.sh vscode
```

Or by hand:

```sh
cargo build -p lcw-cli --bin lcw --release --locked
cd apps/desktop/frontend && trunk build --release && cd ../../vscode
npm ci
npm run media
npm run package
```

Install the result (`apps/vscode/livewalk-0.2.0.vsix`) into Cursor or VS Code:

```sh
cursor --install-extension apps/vscode/livewalk-0.2.0.vsix
```

```sh
code --install-extension apps/vscode/livewalk-0.2.0.vsix
```

If the `cursor` or `code` command is not on your PATH, the binaries are inside
the app bundles:

```sh
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension apps/vscode/livewalk-0.2.0.vsix
```

```sh
"/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code" --install-extension apps/vscode/livewalk-0.2.0.vsix
```

Then reload the window (*Developer: Reload Window*).

Then make `lcw` reachable: put `target/release` on your PATH, or set
`livewalk.lcwPath` (example: `/Users/goose/projects/livewalk/target/release/lcw`).

## Commands

| Command | What it does |
| --- | --- |
| **Livewalk: Open Graph** | Palette: *Open Graph Here* for the active editor's file (else the first workspace folder). The `↗` button in the node card opens that function in the editor. |
| **Livewalk: Open Graph Here** | Right-click a file, folder, editor or tab: resolve its project, confirm, load and show the code you were looking at (see above). |
| **Livewalk: Reveal in Graph** | Select the function under the cursor in the graph panel (opens it if needed). Also in the editor context menu. |
| **Livewalk: Show Flow to Here** | Shortest call path from an entry point to the function under the cursor (`lcw flow`), as a quick pick; picking a hop jumps to it. Also in the editor context menu for Rust. |
| **Livewalk: Show Callers of Function at Cursor** | Direct callers with every call site (`lcw callers`); picking one jumps to the call. |
| **Livewalk: Toggle Caller CodeLens** | Turns `livewalk.codeLens` on or off. |

With CodeLens on, every function gets `N callers · impact` above it (one
`lcw symbols` call per file, about 40 ms on a cached 1300-function repo).
*callers* opens the callers quick pick; *impact* lists the transitive callers
and entry points within `livewalk.impactDepth` hops (`lcw impact`).

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `livewalk.lcwPath` | `lcw` | The `lcw` binary. A bare `lcw` is looked up on PATH, then in `~/.cargo/bin`. |
| `livewalk.confirmBeforeLoad` | `true` | Ask before *Open Graph Here* loads a project. **Load (don't ask again)** sets it to `false`. |
| `livewalk.followCursor` | `false` | Keep the open panel's selection on the function under the cursor (debounced, 350 ms). |
| `livewalk.codeLens` | `false` | Show `N callers · impact` above functions. |
| `livewalk.codeLensLanguages` | `["rust"]` | Language ids that get the CodeLens. |
| `livewalk.impactDepth` | `3` | Caller hops for the impact lens. |
| `livewalk.ollamaModel` | `""` | Ollama model for **Explain** (falls back to `$OLLAMA_MODEL`, then `qwen2.5-coder:7b`). |

**Explain** with Gemini reads `GEMINI_API_KEY` from the environment the editor
was started from; Ollama is expected at `localhost:11434`. **Copy for Claude**
copies the markdown context to the clipboard, as on the desktop.

## Development

```sh
npm ci
npm test
```

`npm test` type-checks and runs the unit tests for the message protocol
(request → `lcw` argument mapping, progress parsing and stage fractions,
reveal ranges, focus messages, language mapping, the load prompt), project-root
resolution (markers, cargo workspaces, workspace-folder bounds) and the webview
HTML rewrite (asset URIs, nonces, CSP, initial language / cache / focus).

To work on the webview without an editor, serve it in a browser with a mocked
`acquireVsCodeApi` that answers through `lcw`, exactly like the extension
(reveal and folder-pick requests are printed instead):

```sh
npm run build
node scripts/webview-harness.js /path/to/repo 8790
```

Then open <http://127.0.0.1:8790/>. Set `LCW=/path/to/lcw` if `lcw` is not on
PATH. Progress streams to the page as it would from the extension. To look at
the loading overlay and the editor focus, hold each analyze for a few seconds
and start with a focused function:

```sh
DELAY_MS=4000 FOCUS=src/main.rs:20 node scripts/webview-harness.js /path/to/repo 8790
```

Push a focus message like *Reveal in Graph* would, or make the next analyze
fail or cancel (then press **Analyze**):

```sh
curl "http://127.0.0.1:8790/focus?file=src/lib.rs&line=42"
```

```sh
curl http://127.0.0.1:8790/fail-next
```

```sh
curl http://127.0.0.1:8790/cancel-next
```

## Known gaps

* Clicking a node on the canvas selects it in the panel; jumping to source is
  the `↗` button (no automatic reveal on every click).
* Freshness compares file modification times with the saved graph; a change
  that keeps mtimes (some checkouts) is not detected. **Analyze** always
  re-parses.
* Cancel is in the editor's progress notification only; the overlay has no
  cancel button of its own, and the desktop app cannot cancel an analyze.
* Fast mode is Rust-first; other languages work in the panel (language chips),
  but the CodeLens defaults to Rust only.
