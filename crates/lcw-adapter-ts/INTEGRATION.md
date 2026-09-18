# Wiring the TypeScript / Python / Go adapters into the engine

These three crates — `lcw-adapter-ts`, `lcw-adapter-py`, `lcw-adapter-go` — are
new Layer 1 [`LanguageAdapter`] implementations that mirror the default Rust
front end (`lcw-adapter-treesitter`). They are **not** yet referenced by the
engine: adapter selection is owned by the engine and is intentionally left to
the main agent so this branch stays a clean, additive set of crates.

This document names the exact hook to change and the recommended
extension → adapter mapping. Nothing in `lcw-engine` (or any other existing
crate) was modified by this branch.

## The hook: `select_adapter`

The single place the engine picks a Layer 1 adapter is:

```rust
// crates/lcw-engine/src/lib.rs
fn select_adapter(config: &Config) -> Box<dyn LanguageAdapter> {
    match config.adapter.mode {
        AdapterMode::Fast => Box::new(RustTreeSitterAdapter::new()),
        AdapterMode::Semantic => select_semantic_adapter(),
    }
}
```

It is called once from `Engine::new` (`crates/lcw-engine/src/lib.rs`) and the
returned adapter is stored on the `Engine`. Two engine sites then consume it:

- `Engine::analyze` → `discover_files(root, &self.config, self.adapter.extensions())`
  uses the adapter's [`LanguageAdapter::extensions`] to decide which files to
  read (see `crates/lcw-engine/src/discover.rs`, which filters by
  `path.extension()`).
- `Engine::analyze_sources_with_progress` calls `self.adapter.parse(sources)`.

So whatever `select_adapter` returns must report **all** the extensions it wants
discovered, and must be able to `parse` that whole batch.

## Adapter → constructor → extensions

| Language   | Crate                    | Constructor                              | `name()`                 | Extensions              |
|------------|--------------------------|------------------------------------------|--------------------------|-------------------------|
| Rust       | `lcw-adapter-treesitter` | `RustTreeSitterAdapter::new()`           | `treesitter-rust`        | `rs`                    |
| TypeScript | `lcw-adapter-ts`         | `TypeScriptTreeSitterAdapter::new()`     | `treesitter-typescript`  | `ts`, `tsx`, `mts`, `cts` |
| Python     | `lcw-adapter-py`         | `PythonTreeSitterAdapter::new()`         | `treesitter-python`      | `py`, `pyi`             |
| Go         | `lcw-adapter-go`         | `GoTreeSitterAdapter::new()`             | `treesitter-go`          | `go`                    |

Each extension is unique across adapters, so an extension → adapter map is
unambiguous.

## Recommended wiring

There are two clean shapes; pick based on how multi-language you want a single
run to be. Both keep the fast path lean — these grammars are lightweight C
parsers, unlike the feature-gated `semantic` (rust-analyzer) backend.

### Option A — one language per run (smallest change)

Select a single adapter from a language hint (a new `config.adapter.language`
field, a CLI `--lang`, or "the dominant extension under `root`"). This fits the
current single-adapter `Engine` with no other changes:

```rust
fn select_adapter(config: &Config) -> Box<dyn LanguageAdapter> {
    if config.adapter.mode == AdapterMode::Semantic {
        return select_semantic_adapter();
    }
    match config.adapter.language {
        Language::Rust       => Box::new(RustTreeSitterAdapter::new()),
        Language::TypeScript => Box::new(TypeScriptTreeSitterAdapter::new()),
        Language::Python     => Box::new(PythonTreeSitterAdapter::new()),
        Language::Go         => Box::new(GoTreeSitterAdapter::new()),
    }
}
```

### Option B — a multiplexing adapter (recommended for mixed repos)

Introduce a composite adapter (e.g. in `lcw-engine`, or a small new
`lcw-adapter-multi` crate) that owns one instance of each front end. This keeps
`Engine` unchanged — it still holds a single `Box<dyn LanguageAdapter>`:

```rust
struct MultiLanguageAdapter { adapters: Vec<Box<dyn LanguageAdapter>> }

impl LanguageAdapter for MultiLanguageAdapter {
    fn name(&self) -> &'static str { "treesitter-multi" }

    // Union of every sub-adapter's extensions, so discovery reads them all.
    fn extensions(&self) -> &'static [&'static str] {
        &["rs", "ts", "tsx", "mts", "cts", "py", "pyi", "go"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        // Group `files` by extension, dispatch each group to its adapter, then
        // merge the resulting subgraphs into one CodeGraph.
        // ...
    }
}
```

Dispatch each file to the adapter whose `extensions()` contains its extension
(build the reverse map once from the adapters themselves — do not hardcode it
twice). Because each adapter's `parse` returns an independent `CodeGraph` with
its own `NodeId`/`FileId` space, the merge step must re-intern nodes and files
into a single graph (offset or re-key ids) rather than concatenating them. Node
qualified names are already namespaced per language (`crate::…`, `app::…`,
`pkg::…`, `pkg::Type::…`), so cross-language name collisions are unlikely, but
keep resolution within each sub-adapter — these are heuristic, name-based
resolvers and should not link a Go call to a Python def.

## Notes for the integrator

- `discover_files` skips `node_modules`, `dist`, `target`, `.git` already
  (`DEFAULT_EXCLUDES`), which is convenient for TS/Go repos.
- If a `language`/`adapter` enum is added to `lcw-config`, keep `AdapterMode` as
  the fast/semantic axis; language is an orthogonal dimension.
- All three adapters are `Send + Sync`, `Default`, and zero-sized, so they are
  cheap to construct and safe to hold behind the boxed trait object.

[`LanguageAdapter`]: ../lcw-core/src/adapter.rs
[`LanguageAdapter::extensions`]: ../lcw-core/src/adapter.rs
