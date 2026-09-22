# Live Code Walk & Analysis (`livewalk`)

A 100% Rust tool for **static analysis, interactive call-graph visualization
and code navigation** of repositories (Rust first; TypeScript, Python and Go
front ends). It is built as a **modular monolith** (a Cargo workspace) that
embodies its own engineering Manifesto - see [`AGENTS.md`](AGENTS.md).

The tool has three analysis layers plus a navigation layer:

1. **Parser & graph extraction** - build a call graph from source (tree-sitter
   by default; an optional rust-analyzer semantic backend for high precision).
2. **Engineering lenses** - static analysis: complexity, purity, memory/thread
   hazards, and architecture (Clean / Onion / MVC) & paradigm checks.
3. **Intelligent suggestions** - recommendations adapted to the detected
   industry vertical (embedded, game engine, backend, full-stack), aimed at
   targets of scalability, maintainability, robustness, latency and performance.
4. **Navigation** (`lcw-query`) - the questions you ask while walking an
   unfamiliar codebase: where does it start, how is it organized, what calls
   this and what does it call, how does control get from here to there.

## Architecture

```
                 +-----------+
                 |  lcw-core |  domain types (API boundary)
                 +-----------+
                   ^   ^   ^   ^
        +----------+   |   |   +-------------------+
        |              |   |                       |
+---------------+  +--------------+  +-----------+  +--------------+
| adapters      |  | analysis     |  | lcw-query |  | suggest      |
| (tree-sitter, |  | (Layer 2     |  | (naviga-  |  | (Layer 3     |
|  rust-analyzer)|  |  lenses)     |  |  tion)    |  |  advisors)   |
+---------------+  +--------------+  +-----------+  +--------------+
        \              |                 |   |  \          /
         \             v                 |   |   \        /
          \        +-----------+         |   |    +------+
           +------>| lcw-engine |<-------|---|-----------+
                   +-----------+         |   |
                    ^         ^          |   |
              +-----+         +------+   |   |
              |                      |   |   |
        +-----------+         +--------------------+
        |  lcw-cli  |<--------| apps/desktop (Tauri|<--+
        +-----------+         |  + Leptos + wgpu)  |
              ^               +--------------------+
              |                       ^
        +------------+                |
        | lcw-render |----------------+   (viewer feature / wasm)
        +------------+
```

Dependencies flow one way only (Principle I). `lcw-query` is pure (no I/O, no
GPU) so the CLI, the native viewer and the wasm webview all run the *same*
navigation code; `lcw-layout` and `lcw-render` support the renderer;
`lcw-telemetry` provides observability everywhere.

## Workspace layout

| Crate | Layer | Responsibility |
|-------|-------|----------------|
| `lcw-core` | - | Shared domain types (graph, metrics, diagnostics, suggestions). |
| `lcw-config` | - | The Manifesto as machine-readable TOML config. |
| `lcw-telemetry` | - | Lightweight `tracing` + counters (Principle III). |
| `lcw-adapter-treesitter` | 1 | Default Rust parser: tree-sitter + heuristic resolution. |
| `lcw-adapter-ts` / `-py` / `-go` | 1 | tree-sitter front ends for TypeScript, Python, Go. |
| `lcw-adapter-ra` | 1 | Optional **semantic** parser (rust-analyzer), behind the `semantic` feature. |
| `lcw-analysis` | 2 | Engineering lenses & quality metrics. |
| `lcw-suggest` | 3 | Vertical detection & target-driven suggestions. |
| `lcw-query` | 4 | Navigation: entry points, outline tree, node cards, flows, call trees, reachability. |
| `lcw-layout` | - | Force-directed graph layout (CPU by default; GPU-compute behind the `gpu` feature). |
| `lcw-render` | - | `wgpu` renderer (native + WASM), module/flow views, highlighting. |
| `lcw-engine` | - | Orchestrator facade tying the layers together. |
| `lcw-cli` | - | Standalone CLI (`lcw`). |
| `apps/desktop` | - | Tauri v2 shell + Rust/WASM UI with the Explorer sidebar. |

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

## Walking a codebase from the CLI

Every navigation command takes `--path <repo>` (default `.`), `--json` for a
machine-readable slice, and `--semantic` for the rust-analyzer backend.

```bash
# Where does it start? Ranked by how much code each entry drives.
lcw entries                    # main/_start, foreign-ABI exports, public roots, tests
lcw entries --kind exported    # kernel/WASM/FFI entries only
lcw entries --reach            # also measure reach for the public/private roots

# How is it organized? crate ▸ module ▸ type ▸ function, with cc / fan-in / fan-out.
lcw outline --depth 1          # collapse below top-level modules
lcw outline --filter parse     # only functions whose path contains "parse"

# What does main trigger, in source order? (↺ marks an already-expanded node)
lcw calls main --depth 3
lcw calls "Engine::analyze" --callers     # who can trigger this?

# One function: metrics, callers (inputs) and callees (outputs) with edge kind,
# multiplicity and call site.
lcw explain "Engine::analyze"

# How does control get from here to there?
lcw flow "parse_incremental" --from main
lcw flow "parse_incremental" --from main --snippets   # JSON with each hop's source
```

Fast mode (tree-sitter) links calls by name with qualifier checking; method
calls whose receiver type is unknown may resolve to a same-named method in
another crate or stay external. `--semantic` resolves them precisely (needs a
`--features semantic` build). See
[`docs/ARCHITECTURE_REVIEW.md`](docs/ARCHITECTURE_REVIEW.md) for the
precision rules and the roadmap.

## Desktop app

The Tauri app (`apps/desktop`) shows the graph with an **Explorer** sidebar:
a "Start here" list of entry points, the collapsible outline tree with a
filter, and a detail pane for the selected node with clickable callers and
callees, a "trace from here" flow (highlighted on the canvas), the bounded
call tree below the node, and back navigation. See
[`apps/desktop/README.md`](apps/desktop/README.md).

In the native viewer (`lcw view`): click to inspect, `f` to set a flow source
then click a target, `m` to jump to `main`, `o` to open the code in an editor.

`lcw analyze --format view` (viewer build) writes the laid-out graph the
frontend consumes, which lets the UI run in a plain browser against a fixture —
see the desktop README's *Browser dev mode*.

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
out of the main workspace (it has its own wasm/bundler build):

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
8. Code navigation: `lcw-query`, `outline`/`entries`/`calls`/`explain`, the
   desktop Explorer + detail pane, qualifier-checked call resolution — **done**
