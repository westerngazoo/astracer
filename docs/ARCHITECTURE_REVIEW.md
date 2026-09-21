# Architecture review — Live Code Walk (`livewalk`)

A review of the codebase as a *tool for understanding other people's (or an
AI's) code*: what is in place, what was missing for that job, what was changed
in this pass, and where the architecture should go next. Written for an
engineer reading it as a study object, so each recommendation carries the
reasoning behind it, not just the verdict.

## 1. The shape of the system in one picture

Think of a repository as a **circuit board**. Every function is a component,
every call is a wire, `main` is the reset vector, and a `pub` function nobody
in the repo calls is a connector on the board edge — something *outside* plugs
into it. Understanding an unfamiliar board means: find the power-on path, follow
the signal, and at any component ask "what drives this pin, what does it drive".

The workspace maps onto that job in three stages plus a query layer:

```
source files ──▶ Layer 1  adapters  (tree-sitter / rust-analyzer)  ──▶ CodeGraph
                                                                        │
              Layer 2  lenses    (metrics + diagnostics)  ◀──────────────┤
              Layer 3  advisors  (vertical + suggestions) ◀──────────────┤
                                                                        │
              lcw-query  (entry points, outline, node cards, flows,  ◀──┘
                          call trees, reachability)  ── pure, wasm-safe
                    │                │                     │
                 lcw-cli        native viewer        Tauri/Leptos desktop
```

`lcw-core` is the netlist (`CodeGraph` on `petgraph`, dense `u32` ids, an
interned file table). Everything else is a *reader* of that netlist. The
one-way dependency rule (Principle I) is respected everywhere, including by the
new crate: `lcw-query` depends on `lcw-core` only.

## 2. What was missing for "walk an unfamiliar codebase"

The previous state already had a call graph, an aggregated module view, a
`flow` command and a native HUD with inputs/outputs. What it lacked was the
**navigation layer** a reader actually uses, and the pieces that existed were
in the wrong places:

| Need | Before | Now |
|------|--------|-----|
| A tree of *where code lives* (crate ▸ module ▸ type ▸ fn) | none | `lcw_query::outline` → `lcw outline`, Explorer sidebar |
| "Where do I start?" | grep for `main` | `lcw_query::entry_points` (main / public roots / private roots / tests) → `lcw entries`, "Start here" list, `m` key in the native viewer |
| Click a node → what comes in / goes out, with the edge kind, multiplicity and call site | native HUD only, names only | `lcw_query::node_card` → `lcw explain`, native HUD, desktop detail pane (clickable) |
| "From main, what gets triggered?" | none | `lcw_query::call_tree` → `lcw calls`, "Calls below" tree in the desktop |
| Trace a flow between two nodes in the desktop app | none (CLI + native only) | shared `shortest_paths`, "trace from here" + highlighted path |
| One implementation of traversal | BFS in `bins/lcw-cli/src/flow.rs` **and** a second, weaker BFS in `lcw-render/native.rs`; symbol resolution in the CLI binary; module aggregation in the renderer | one crate, unit-tested, used by all three front ends |

The composability point is the important one. A query implemented inside a
binary is a dead end: the native window could not call the CLI's BFS, and the
wasm frontend could call neither, so it had no flow feature at all. Moving the
logic into a pure library crate is the same move as pulling a signal-processing
routine out of `main.c` into a library so the bench tool, the firmware and the
simulator all run *the same* filter. Now `explain`, the HUD panel and the
desktop pane are three renderings of one `NodeCard`, and they cannot disagree.

## 3. Findings from reading the code

Ordered by how much they distort what a reader sees.

### F1 — Qualified calls were bound to arbitrary same-named functions (fixed)

In fast mode, `Cli::parse()` inside the CLI's `main` resolved to
`lcw_adapter_go::GoTreeSitterAdapter::parse`, and every `Vec::new()` /
`Config::default()` in the repo was glued to *some* `new` / `default` defined
somewhere. The resolver matched by short name, then by "same module", then
took the first candidate — the qualifier (`Cli::`, `Vec::`) was only consulted
as a tiebreak.

In netlist terms: two nets labelled `CLK` on different sheets were being
shorted together because the sheet qualifier was ignored. The fix
(`crates/lcw-adapter-treesitter/src/extract.rs`, `resolve_target`) requires
every qualifier segment of the call path to appear in the candidate's own path;
if none matches, the call stays *external* rather than inventing an edge.
Relative heads (`Self::`, `self::`, `super::`, `crate::`) carry no scope
information and keep resolving like a plain call. Among several eligible
candidates the order is now same module → same crate → first declared.

Measured on this repository: `lcw_cli::main` went from a 12-callee tree that
included the Go adapter to exactly its nine `run_*` functions; `build::main`'s
"reach" dropped from 12 functions to the honest 0.

### F2 — Method calls have no receiver type (open; the next precision win)

`engine.analyze(path)` is a `field_expression` call: the adapter sees only the
method name `analyze`. Candidates are `lcw_analysis::analyze`,
`Engine::analyze`, `EngineTransport::analyze`… and with no module/crate match
it takes the first, which is in the *frontend* crate. That is why
`lcw flow "Engine::analyze_with_progress" --from lcw_cli::main` finds no path
in fast mode.

Recommendation: **receiver-type hints**, a tiny local inference inside one
function body. Track `let x = Type::new(..)`, `let x: Type = ..`, `fn f(x: &Type)`
and `self` inside `impl Type`; when the receiver of `x.m()` has a known type,
resolve `Type::m` with the qualifier rule from F1. This catches the large
majority of method calls in idiomatic Rust at a cost of one hash map per
function, with no cross-file state (so it stays cacheable and incremental). It
does not replace the semantic backend, which remains the answer for trait
dispatch and generics.

### F3 — Merged edges keep only the first call site

`CodeGraph::add_edge` merges identical `(from, to, kind)` edges by bumping
`count`, keeping the first `call_site`. The node card can therefore say
"`run` calls `parse` ×2 at line 22" but cannot list line 23. For a
jump-to-every-call-site feature, store the sites: a `SmallVec<[SourceSpan; 2]>`
on the edge, or a side table `edge_sites: Vec<(EdgeIndex, SourceSpan)>` kept
flat (Principle II) and indexed on demand.

### F4 — Entry-point and dead-code logic were two copies of one idea

`DeadCodeLens` decides "root-ness" with its own `is_dead_code_candidate`; the
new `classify_entry` decides it for navigation. They agree today, but two
definitions of "who is a root" drift. Make the lens consume
`lcw_query::classify_entry` (or move `classify_entry` into `lcw-core` if the
analysis crate must not depend on the query crate — it is 20 lines and has no
dependencies).

### F5 — Language-specific entry rules live in the wrong layer

`classify_entry` sniffs the file extension to treat Go's `init` as an entry.
That is a smell: the *adapter* knows the language. Add a trait method with a
default, e.g. `LanguageAdapter::entry_kinds(&self, node: &Node) -> Option<EntryKind>`,
or have adapters set a `NodeFlags::is_entry` bit at extraction time (Python's
`if __name__ == "__main__":` block, TypeScript default exports, Go `init`,
Rust `#[entry]` / `#[tokio::main]`). The query crate then only reads flags.

### F6 — `Config` cannot be extended by other crates

`Config` is `#[serde(deny_unknown_fields)]`, so the extended lenses had to
invent a parallel `ExtLensConfig` that the TOML file cannot express (the file
only has a single `extended = true` switch). For a plugin-style lens/advisor
system, add one escape hatch: `[extensions]` as a `toml::Table` (or
`HashMap<String, toml::Value>`) that each lens deserializes its own section
from. The core config stays strict; extensions get a namespace.

### F7 — Engine hooks erase identity

`Engine::with_lens_stage(Box<dyn Fn(&Config, &mut AnalysisReport)>)` accepts an
anonymous closure, so a registered stage has no name, cannot be listed, toggled
from config, or reported in telemetry. The `Lens` and `Advisor` traits already
exist; accept them directly: `with_lens(Box<dyn Lens>)`,
`with_advisor(Box<dyn Advisor>)`. Keep the closure form only as a convenience
wrapper.

### F8 — One language per run

A Tauri app is Rust *and* TypeScript; a Python service has Go sidecars. The
engine selects exactly one adapter per run, so such a repo can never be one
graph. A `CompositeAdapter` that dispatches each file by extension to a
registered adapter and merges the fragments is mechanical to write because the
fragment/resolve split already exists; cross-language edges (FFI, IPC
commands) would stay external, which is honest.

### F9 — No CI check for the wasm frontend

`apps/desktop/*` is excluded from the workspace and only built best-effort at
release time, so a change to `lcw-render` or `lcw-core` that breaks the
frontend is invisible until someone runs `trunk`. A cheap job —
`cargo check --target wasm32-unknown-unknown` in `apps/desktop/frontend` —
takes minutes, needs no GPU or webview, and would have caught the API drift
this pass had to work around.

### F10 — Memory: qualified names are stored twice

`CodeGraph.qualified_index: HashMap<String, NodeId>` duplicates every
`qualified_name`, and `module_path` repeats the prefix of `qualified_name` for
every node (`estimated_bytes` already accounts for this). For giant repos,
intern path segments into one arena (`Vec<u8>` + `(u32 offset, u16 len)`
handles) and store paths as small segment-id slices: the classic linker
symbol-table layout. The API can stay string-based via accessors.

### F11 — Rendering and layout have no level of detail

The desktop app lays out the full function graph once and draws every node.
Above ~10⁴ nodes the result is a hairball and the O(n) CPU hit-test per click
starts to show. Two standard remedies, both composable with what exists:

* **Coarse-to-fine (multigrid) layout**: lay out the module graph (already
  computed by `module_view`), then place each module's functions inside its
  cell. It is the same idea as solving a field on a coarse mesh and refining
  only where you look.
* **Warm-started incremental layout**: seed the next layout from the previous
  positions (a cache next to the fragment cache) instead of the sunflower
  spiral, so re-analysis after an edit continues the simulation from its last
  state rather than restarting from a random configuration. Nodes stop
  jumping between runs, which matters more for orientation than the absolute
  quality of the layout.

### F12 — Transport is one JSON blob

`analyze_repo` returns the whole `ReportSnapshot` plus positions through
`serde_wasm_bindgen`. Fine to ~10⁵ nodes; beyond that, JSON encoding/decoding
dominates. Options in increasing effort: send `positions` as a `Float32Array`
(typed arrays cross the bridge without parsing); split the payload (summary
first, graph second, so the UI paints early); or a binary snapshot
(`bincode`-style) behind the same `EngineTransport` trait — the seam is
already there, so this is a local change.

## 4. Recommendations by axis

### Engineering (correctness, precision, trust)

1. Receiver-type hints (F2) — the single biggest precision gain in fast mode.
2. Keep every call site (F3).
3. Add the wasm `cargo check` CI job (F9) and a `cargo test -p lcw-query`
   golden test on a multi-crate fixture, so cross-crate resolution is pinned
   the way single-file extraction already is.
4. Cache the semantic backend's graph like the fragment cache (keyed by
   `Cargo.lock` + file hashes): rust-analyzer loads are the reason nobody
   reaches for `--semantic` casually.

### Composability

1. `lcw-query` is now the contract for any front end. A VS Code extension
   would implement `EngineTransport` and render `NodeCard` / `Outline` /
   `CallTreeNode` — no traversal code on the TypeScript side. Keep it that way:
   *front ends render, they never traverse.*
2. The `--json` outputs of `explain`, `flow`, `outline`, `entries`, `calls`
   are scoped slices an LLM or script can consume; keep their shapes stable and
   version them (`"schema": 1`) once external tools depend on them.
3. Unify root detection (F4) and push language knowledge down to adapters (F5).

### Scalability

1. String interning and segment-id paths (F10).
2. Level-of-detail rendering and warm-started layout (F11).
3. Typed-array / chunked transport (F12).
4. Run `lcw-query` off the UI thread for huge graphs: it is pure and
   `Send`-free, so a web worker (wasm) or a `spawn_blocking` (Tauri) can host
   it unchanged; only the `EngineTransport` grows an async `query` call.

### Extensibility

1. `[extensions]` namespace in `Config` (F6).
2. `Engine::with_lens(Box<dyn Lens>)` / `with_advisor` (F7), plus a registry so
   `livewalk.toml` can enable lenses by name.
3. `CompositeAdapter` and per-extension adapter registration (F8).
4. Entry-point classification as an adapter responsibility (F5).

## 5. How the new pieces work (for the reader who wants the mechanism)

**Shortest flow = wavefront propagation.** `shortest_paths` is a breadth-first
search: the queue holds the current wavefront, every pop expands it by one hop,
so nodes are discovered in non-decreasing distance. The first pop of a target
is therefore minimal. The search then finishes *that layer* before stopping and
records every predecessor that reached a node at the same distance, so
alternate routes of equal length can be enumerated afterwards by walking the
predecessor lists backward — all rays arriving in phase, not just the first
one. Cost is O(n + e) per trace.

**Call tree = expand once.** A naive "callees of callees" expansion is
exponential on a graph with shared subroutines (every path to a shared helper
is re-expanded). `call_tree` expands each node the first time it is met and
shows later encounters as leaves marked `↺`, which bounds the tree by the
number of nodes. Children are ordered by first call site, i.e. in the order the
code runs them, which is what you want when reading `main`.

**Entry points = the vector table.** `classify_entry` combines a name rule
(`main`, Go `init`) with graph shape: zero in-degree means nothing in the
analyzed code calls it. A public such node is an API surface (a connector on
the board edge); a private one is reached dynamically or is dead. `entries
--reach` measures how much code each root drives, which ranks entries by
importance without any heuristics about names.

**Outline = a trie over paths.** Splitting every `module_path` on `::` and
inserting the function as a leaf yields the crate ▸ module ▸ type ▸ fn tree
with no extra data from the adapters. Pre-order ids give the UI a stable key
for "expanded" state; `prune(query)` keeps matching functions and their
ancestors so a search never shows an orphaned leaf.

## 6. Verification performed in this pass

* `cargo test --workspace`: green, including the tree-sitter golden snapshot
  (unchanged by the resolver fix) and the engine's self-analysis.
* `cargo clippy --workspace --all-targets -- -D warnings`: clean.
* `cargo check --target wasm32-unknown-unknown` for `apps/desktop/frontend`:
  green (no `trunk`/webview available here, so the UI was type-checked, not
  run).
* The CLI was run against this repository (`outline`, `entries`, `calls`,
  `explain`, `flow`); the F1 numbers above come from those runs.
