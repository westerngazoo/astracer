# AGENTS.md - Engineering Manifesto for Live Code Walk

This repository is both a tool *and* a reference implementation of its own
Manifesto. Any agent (human or AI) writing or reviewing code here MUST follow
the directives below. They exist to keep the architecture from degrading across
long editing sessions.

## The three universal principles

### Principle I - Strict modularity
- Every component is a self-contained black box behind a **closed interface**.
- Concrete seams in this repo: `LanguageAdapter` (Layer 1), `Lens` (Layer 2),
  `Advisor` (Layer 3), `EngineTransport` (frontend <-> engine).
- Crate dependencies flow **one way only** (see the diagram in `README.md`).
  Never add an upward or cyclic dependency. `lcw-core` depends on nothing heavy.
- Prefer isolating failures (small blast radius) over sharing mutable state.

### Principle II - Data-driven approach
- Favor flat, cache-friendly data structures over deep abstraction.
- Ids are plain `u32` newtypes; graphs are stored in contiguous arenas.
- Minimize heap allocation on hot paths; prefer `Copy` types and slices.
- In performance-critical subsystems (parsing giant repos, rendering) reach for
  Data-Oriented Design before adding indirection.

### Principle III - Native observability
- Instrumentation ships with the code, via `lcw-telemetry` (`tracing` + counters).
- It must stay *lightweight*: **no network exporters**, no added latency.
- Add spans/counters at layer boundaries, not in tight inner loops.

## Quality bar (Manifesto section 4)
When adding or changing code, keep an eye on:
- **Cyclomatic complexity** - prefer early returns / small functions; defaults
  flag functions above `metrics.cyclomatic_max` (10).
- **Test coverage** - cover critical paths and edge cases. Every crate has unit
  tests; the parser has golden (`insta`) tests.
- **Memory footprint** - avoid needless heap allocation; prefer the stack.
- **Clock-cycle latency** - matters most for the renderer and giant-repo runs.

## Coding conventions
- Rust 2021, `cargo fmt` clean, `cargo clippy` warning-free.
- `unsafe` requires a `// SAFETY:` comment explaining the invariant it upholds.
- Public items get doc comments; comments explain *why*, not *what*.
- Do not introduce a new third-party dependency without a clear reason.
- Heavy or API-unstable dependencies stay **off the fast path**: gate them behind
  an off-by-default Cargo feature with a graceful fallback (e.g. `semantic` →
  rust-analyzer in `lcw-adapter-ra`, `gpu` → wgpu compute in `lcw-layout`). The
  default `cargo build`/`cargo test` must never pull them in.

## Layout
- `crates/` - the library crates (core, config, telemetry, adapters, analysis,
  suggest, layout, render, engine).
- `bins/lcw-cli` - the standalone CLI.
- `apps/desktop` - the Tauri v2 shell + Rust/WASM UI.
