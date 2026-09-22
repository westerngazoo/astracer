# Live Code Walk — Desktop app

A Tauri v2 desktop shell around the reusable analysis crates. It is deliberately
thin: all analysis lives in the workspace crates (`lcw-engine` and below), all
navigation logic in `lcw-query`, and this app only handles windowing,
transport, and rendering.

```
apps/desktop
├── src-tauri/        # Rust backend: #[tauri::command]s over lcw-engine
│   ├── src/lib.rs    #   analyze_repo command + progress events + GraphView DTO
│   ├── src/main.rs   #   thin entry point -> livewalk_desktop_lib::run()
│   ├── tauri.conf.json
│   └── capabilities/ #   ACL: core:default for the main window
└── frontend/         # Leptos (WASM) UI
    ├── src/app.rs        #   Explorer sidebar, canvas wiring, detail pane
    ├── src/transport.rs  #   EngineTransport trait + TauriTransport
    ├── src/viewer.rs     #   CPU hit-testing
    ├── index.html        #   Trunk entry + styling
    └── Trunk.toml
```

## What you see

Three columns:

* **Explorer (left)** — *Start here*: the entry points, ranked by how much code
  each drives — `main`/`_start`, then **exported** (symbols a bootloader,
  hardware trap vector, WASM host or FFI caller enters through, which is where
  a kernel or WASM module actually starts), then public roots, private roots
  and tests. Below it
  the **Outline**: crate ▸ module ▸ type ▸ function, collapsible, with a
  filter box, a cyclomatic-complexity badge per function and entry badges.
  Clicking a function selects it and centers the canvas on it.
* **Canvas (middle)** — the wgpu call graph: drag to pan, scroll to zoom, click
  a node to select it, click the background to clear. The selection is bright
  yellow, the flow source orange, nodes on the traced path blue, everything
  else dimmed. A minimap and the label overlay track the camera.
* **Detail (right)** — the selected node's card: location, entry kind, flags,
  metrics; **Inputs** (callers) and **Outputs** (callees), each with the edge
  kind, multiplicity and call-site line, and each clickable to navigate;
  **Flow**: *trace from here* marks the node as the source, then any click
  (canvas, outline, a callee) shows the shortest call path as clickable hops
  and highlights it on the canvas; **Calls below**: a bounded call tree in
  source order (`↺` = already expanded / cycle). `◀` goes back, `⌖` recenters,
  `✕` clears.

The toolbar's **Main** button jumps to the program's `main`; **Fit** frames
the whole graph; the search box highlights matching nodes without moving the
view.

## Architecture

- **Backend (`src-tauri`)** exposes one command, `analyze_repo(path, semantic)`,
  which runs `lcw-engine` on a blocking thread and returns a `GraphView`
  (an owned `lcw_core::ReportSnapshot` + one `[x, y]` layout position per node).
  Progress is streamed via the `analyze-progress` event.
- **Frontend (`frontend`)** is a Leptos CSR app. It rebuilds the `CodeGraph`
  from the snapshot (`CodeGraph::from_snapshot`), turns it into GPU buffers with
  `lcw-render`'s `build_scene`, and draws it on a `<canvas>` via the
  `WebViewer`. Every navigation question — outline, entry points, node cards,
  flow paths, call trees — is answered client-side by `lcw-query` on that same
  graph, so there is no second IPC round trip and no traversal code in the UI.
- **`EngineTransport`** is the seam that decouples the UI from the host. Today
  `TauriTransport` uses `invoke`/`listen`; a future VS Code webview would add a
  `VsCodeTransport` over `postMessage` without touching the components.

The graph crosses the process boundary as a serde snapshot, so the backend
(native, `rayon` layout) and the frontend (wasm, `wgpu`) never share memory or a
non-portable dependency (Principle I).

## Browser dev mode (no Tauri, no GPU hardware)

The UI detects whether it is hosted by Tauri (`window.__TAURI__`). When it is
not, `transport::FixtureTransport` takes over: **Analyze** fetches a JSON graph
view instead of calling the engine, so the whole interface can be developed,
screenshotted and tested in a plain browser.

```bash
apps/desktop/frontend/browser-dev.sh /path/to/repo
```

That builds the analyzer and the wasm UI, analyzes the repository into a graph
fixture, and serves the result on <http://127.0.0.1:8765/>; type `fixture.json`
in the path box and press **Analyze**. `just ui /path/to/repo` does the same.

The bundle goes to `frontend/target/browser-dev/`, not `frontend/dist/`.
`dist/index.html` is a *tracked placeholder* that has to exist for the backend's
`generate_context!` to compile: building into `dist` overwrites it and dirties
the working tree, and serving `dist` before a build serves the placeholder,
whose page reads "Run `trunk build` ...". Keeping the bundle under the
git-ignored `target/` avoids both.

`tests/ui_smoke.mjs` drives exactly that setup in headless Chromium
(`node tests/ui_smoke.mjs http://127.0.0.1:8765/ /tmp/shots`, needs Playwright)
and walks the whole interface: analyze, jump to main, follow a callee, trace a
flow, go back, toggle and filter the outline, click the canvas. It reports
console errors and takes screenshots at each step. It is not in CI, which would
mean adding Node to a pure-Rust workspace; the `wasm` CI job type-checks the
same code.

> A canvas hands out one kind of drawing context for its lifetime. Where a
> browser advertises WebGPU but yields no adapter, the failed attempt makes that
> element unusable for WebGL2, so `WebViewer` retries on a **replacement
> canvas** and the app re-attaches its pointer listeners
> (`WebViewer::canvas()`). This is what makes the graph render under WebKitGTK
> and in headless browsers.

## Why the build hooks set a working directory

Tauri runs `beforeDevCommand` / `beforeBuildCommand` in what it calls the
*frontend directory*, which it takes to be the parent of `src-tauri` — the
usual layout, where `package.json` sits next to it. Here the frontend is a
sibling folder (`apps/desktop/frontend`), so a bare `trunk serve` would start in
`apps/desktop`, find no `Trunk.toml`, and fail with "Unable to find any Trunk
configuration". Both hooks therefore name the directory explicitly:

```json
"beforeDevCommand": { "script": "trunk serve", "cwd": "../frontend" }
```

A relative `cwd` resolves against `src-tauri` (the CLI chdirs there before
running the hook), which is also what `frontendDist` is relative to.

## Prerequisites

```bash
# wasm toolchain + Trunk (frontend bundler) + Tauri CLI
rustup target add wasm32-unknown-unknown
cargo install trunk
cargo install tauri-cli --version "^2"
```

## Develop

From `apps/desktop/src-tauri`:

```bash
cargo tauri dev
```

`tauri dev` starts `trunk serve` (frontend on `http://localhost:1420`) and opens
the window. Enter a path to a repository and press **Analyze**.

Without the webview toolchain you can still type-check the UI:

```bash
cd apps/desktop/frontend && cargo check --target wasm32-unknown-unknown
```

## Build

```bash
cargo tauri build
```

This runs `trunk build` (emitting `frontend/dist/`) and bundles the app.

> The checked-in `frontend/dist/index.html` is only a placeholder so the backend
> compiles standalone (`generate_context!` embeds `frontendDist`); `trunk build`
> overwrites it with the real bundle.

## Optional features

The backend mirrors the workspace's optional features (both off by default):

```bash
# Precise call graph via rust-analyzer. The UI's "semantic" toggle only takes
# effect when the backend is compiled with this feature; otherwise it falls back
# to the fast tree-sitter adapter.
cargo tauri dev -- --features semantic

# GPU-compute layout (wgpu). Falls back to the CPU layout if no adapter exists.
cargo tauri dev -- --features gpu
```
