# Live Code Walk & Analysis (`livewalk`)

A 100% Rust tool for **static analysis and interactive call-graph
visualization** of code repositories (Rust first). It is built as a **modular
monolith** (a Cargo workspace) that embodies its own engineering Manifesto -
see [`AGENTS.md`](AGENTS.md).

The tool has three layers:

1. **Parser & graph extraction** - build a call graph from source (tree-sitter
   by default; an optional rust-analyzer semantic backend for high precision).
2. **Engineering lenses** - static analysis: complexity, purity, memory/thread
   hazards, and architecture (Clean / Onion / MVC) & paradigm checks.
3. **Intelligent suggestions** - recommendations adapted to the detected
   industry vertical (embedded, game engine, backend, full-stack), aimed at
   targets of scalability, maintainability, robustness, latency and performance.

## Architecture

```
                 +-----------+
                 |  lcw-core |  domain types (API boundary)
                 +-----------+
                   ^   ^   ^
        +----------+   |   +-------------------+
        |              |                       |
+---------------+  +--------------+     +--------------+
| adapters      |  | analysis     |     | suggest      |
| (tree-sitter, |  | (Layer 2     |     | (Layer 3     |
|  rust-analyzer)|  |  lenses)     |     |  advisors)   |
+---------------+  +--------------+     +--------------+
        \              |                       /
         \             v                      /
          \        +-----------+             /
           +------>| lcw-engine |<-----------+
                   +-----------+
                    ^         ^
              +-----+         +------+
              |                      |
        +-----------+         +--------------------+
        |  lcw-cli  |         | apps/desktop (Tauri|
        +-----------+         |  + Leptos + wgpu)  |
                              +--------------------+
```

Dependencies flow one way only (Principle I). `lcw-layout` and `lcw-render`
support the renderer; `lcw-telemetry` provides observability everywhere.

## Workspace layout

| Crate | Layer | Responsibility |
|-------|-------|----------------|
| `lcw-core` | - | Shared domain types (graph, metrics, diagnostics, suggestions). |
| `lcw-config` | - | The Manifesto as machine-readable TOML config. |
| `lcw-telemetry` | - | Lightweight `tracing` + counters (Principle III). |
| `lcw-adapter-treesitter` | 1 | Default parser: tree-sitter + heuristic resolution. |
| `lcw-adapter-ra` | 1 | Optional **semantic** parser (rust-analyzer), behind the `semantic` feature. |
| `lcw-analysis` | 2 | Engineering lenses & quality metrics. |
| `lcw-suggest` | 3 | Vertical detection & target-driven suggestions. |
| `lcw-layout` | - | Force-directed graph layout (CPU by default; GPU-compute behind the `gpu` feature). |
| `lcw-render` | - | `wgpu` renderer (native + WASM). |
| `lcw-engine` | - | Orchestrator facade tying the layers together. |
| `lcw-cli` | - | Standalone CLI (`lcw`). |
| `apps/desktop` | - | Tauri v2 shell + Rust/WASM UI. |

## Building

```bash
# Everything that doesn't need a GPU/webview:
cargo build

# Run the CLI against a repo:
cargo run -p lcw-cli -- analyze /path/to/repo --format summary

# Tests:
cargo test

# Native interactive viewer (winit + wgpu power mode):
cargo run -p lcw-cli --features viewer -- view /path/to/repo
```

### Optional features

```bash
# Precise semantic call graph via rust-analyzer (heavy; loads the Cargo
# workspace). Falls back to tree-sitter automatically when not compiled in.
cargo build -p lcw-engine --features semantic

# GPU-compute layout (wgpu) for large graphs; falls back to CPU with no adapter.
cargo test -p lcw-layout --features gpu

# Performance benchmarks (Criterion) for parse + layout:
cargo bench -p lcw-adapter-treesitter -p lcw-layout
```

Both features are additive and off by default, so the fast path never pays for
`ra_ap_*` or `wgpu`. The desktop app exposes matching `semantic` / `gpu`
features (`cargo tauri dev -- --features gpu`).

The **desktop app** (Tauri v2 + Leptos/WASM) lives in `apps/desktop` and is kept
out of the main workspace (it has its own wasm/bundler build). See
[`apps/desktop/README.md`](apps/desktop/README.md):

```bash
cd apps/desktop/src-tauri
cargo tauri dev
```

Configuration lives in [`livewalk.toml`](livewalk.toml).

## Status

Built phase by phase (see the plan):

1. Workspace, core types, config, telemetry — **done**
2. Layer 1 (tree-sitter adapter, engine, CLI: JSON/DOT/GraphML) — **done**
3. `lcw-layout` (CPU force-directed) + `lcw-render` (`wgpu`) native window — **done**
4. Tauri desktop app: Leptos shell + `lcw-render` in the webview — **done**
5. Layer 2 engineering lenses & metrics — **done**
6. Layer 3 suggestions & vertical detection — **done**
7. Hardening: semantic (rust-analyzer) backend (`semantic`), GPU-compute layout
   (`gpu`), streaming progress API, and Criterion benchmarks — **done**
