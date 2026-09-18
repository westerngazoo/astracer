# Live Code Walk — Desktop app

A Tauri v2 desktop shell around the reusable analysis crates. It is deliberately
thin: all analysis lives in the workspace crates (`lcw-engine` and below), and
this app only handles windowing, transport, and rendering.

```
apps/desktop
├── src-tauri/        # Rust backend: #[tauri::command]s over lcw-engine
│   ├── src/lib.rs    #   analyze_repo command + progress events + GraphView DTO
│   ├── src/main.rs   #   thin entry point -> livewalk_desktop_lib::run()
│   ├── tauri.conf.json
│   └── capabilities/ #   ACL: core:default for the main window
└── frontend/         # Leptos (WASM) UI
    ├── src/app.rs        #   components + canvas wiring (pan/zoom/pick)
    ├── src/transport.rs  #   EngineTransport trait + TauriTransport
    ├── src/viewer.rs     #   CPU hit-testing + node detail
    ├── index.html        #   Trunk entry + styling
    └── Trunk.toml
```

## Architecture

- **Backend (`src-tauri`)** exposes one command, `analyze_repo(path, semantic)`,
  which runs `lcw-engine` on a blocking thread and returns a `GraphView`
  (an owned `lcw_core::ReportSnapshot` + one `[x, y]` layout position per node).
  Progress is streamed via the `analyze-progress` event.
- **Frontend (`frontend`)** is a Leptos CSR app. It rebuilds the `CodeGraph`
  from the snapshot (`CodeGraph::from_snapshot`), turns it into GPU buffers with
  `lcw-render`'s `build_scene`, and draws it on a `<canvas>` via the
  `WebViewer` (the same `wgpu` renderer used by the native window, now on
  WebGPU/WebGL2).
- **`EngineTransport`** is the seam that decouples the UI from the host. Today
  `TauriTransport` uses `invoke`/`listen`; a future VS Code webview would add a
  `VsCodeTransport` over `postMessage` without touching the components.

The graph crosses the process boundary as a serde snapshot, so the backend
(native, `rayon` layout) and the frontend (wasm, `wgpu`) never share memory or a
non-portable dependency (Principle I).

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
the window. Enter a path to a Rust repository and press **Analyze**.

- Drag to pan · scroll to zoom · click a node for its metrics.
- Node size grows with fan-in; color warms with cyclomatic complexity.

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
