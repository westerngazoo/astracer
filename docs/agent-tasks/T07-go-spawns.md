# T07: Go goroutines (`go f()`) as thread roots

Size: M · Area: Go adapter · Touches: `crates/lcw-adapter-go/src/extract.rs`,
`crates/lcw-adapter-go/src/lib.rs` (tests)

## Why
The Run tree (`lcw_query::run_roots`) shows a function as a thread root only when
an `EdgeKind::Spawn` edge reaches it. The Rust adapter emits these for
`thread::spawn` and similar calls. The Go adapter treats `go worker(ch)` as an
ordinary call, so a Go program's goroutines never appear as roots, and the calls
inside `go func() { … }()` are credited to the enclosing function.

## What to do
In `scan_calls` (Go adapter), handle `go_statement`:

1. **`go f(…)` / `go x.M(…)`:** resolve the call exactly as today. If it resolves
   to a definition, add an `EdgeKind::Spawn` edge instead of the call edge. If it
   does not resolve, add **no edge**: an unresolvable thread root would be an
   invented one.
2. **`go func() { … }()`:**
   - Create a synthetic node with kind `Closure`, name `<spawned@L{line}>`,
     qualified name `<caller qualified name>::<spawned@L{line}>`, `module_path`
     set to the caller's qualified name, and the literal's span.
   - Add a Spawn edge caller → closure.
   - Scan the literal's body with the **closure as the caller**.
   - If two closures start on one line, the second is `<spawned@L{line}>#2`.

Follow the Rust implementation: `scan_spawn`, `spawn_closure` and
`resolve_spawn` in `crates/lcw-adapter-treesitter/src/extract.rs`, and its tests
in `crates/lcw-adapter-treesitter/src/lib.rs`. The bare-name rule from `AGENTS.md`
still holds: a bare `go f()` never binds to a method.

## Done when
New tests in `crates/lcw-adapter-go/src/lib.rs`, each failing without the change:
- [ ] `go worker(ch)` produces a Spawn edge `main → worker`, and no call edge
      between them.
- [ ] `go func() { crunch() }()` produces a closure node, a Spawn edge
      `main → closure` and a call edge `closure → crunch`, and **no**
      `main → crunch` edge.
- [ ] `go missing()`, with `missing` undefined, adds no edge at all.
- [ ] `lcw_query::run_roots` on the first program lists `worker` as a
      `RootKind::Spawned` root. `lcw-query` is already a dependency of the
      engine; add it as a dev-dependency of the Go adapter if needed, and say so
      in the PR.

## Verify
`cargo test -p lcw-adapter-go --locked`, plus the standard checks in `AGENTS.md`.

## Out of scope
Python and TypeScript spawns (later tasks); channels and `sync.WaitGroup`.
