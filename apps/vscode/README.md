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
* The panel opens on the workspace folder with **cached** on, so it shows the
  saved graph immediately; uncheck it and press **Analyze** for a fresh parse.

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

Install the result (`apps/vscode/livewalk-0.1.0.vsix`) into Cursor or VS Code:

```sh
cursor --install-extension apps/vscode/livewalk-0.1.0.vsix
```

```sh
code --install-extension apps/vscode/livewalk-0.1.0.vsix
```

If the `cursor` command is not on your PATH (Cursor: *Shell Command: Install
'cursor' command*), the binary is inside the app bundle:

```sh
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension apps/vscode/livewalk-0.1.0.vsix
```

Then make `lcw` reachable: put `target/release` on your PATH, or set
`livewalk.lcwPath` (example: `/Users/goose/projects/livewalk/target/release/lcw`).

## Commands

| Command | What it does |
| --- | --- |
| **Livewalk: Open Graph** | Opens the graph panel on the workspace folder. The `↗` button in the node card opens that function in the editor. |
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
(request → `lcw` argument mapping, progress parsing, reveal ranges) and the
webview HTML rewrite (asset URIs, nonces, CSP).

To work on the webview without an editor, serve it in a browser with a mocked
`acquireVsCodeApi` that answers through `lcw`, exactly like the extension
(reveal and folder-pick requests are printed instead):

```sh
npm run build
node scripts/webview-harness.js /path/to/repo 8790
```

Then open <http://127.0.0.1:8790/>. Set `LCW=/path/to/lcw` if `lcw` is not on
PATH.

## Known gaps

* Clicking a node on the canvas selects it in the panel; jumping to source is
  the `↗` button (no automatic reveal on every click), and the editor does not
  yet tell the panel to select the function under the cursor.
* Fast mode is Rust-first; other languages work in the panel (language chips),
  but the CodeLens defaults to Rust only.
* The panel's graph is the cached one by default and can be stale until you
  press **Analyze**; CodeLens and the quick picks re-analyze automatically
  when sources are newer than the saved graph.
