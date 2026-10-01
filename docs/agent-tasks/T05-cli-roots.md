# T05: `lcw roots`, the Run tree in the terminal

Size: M · Area: CLI · Touches: `bins/lcw-cli/src/main.rs` (+ a new module if it
helps), `README.md` (CLI section)

## Why
The web UI's Run tree is the best way into an unfamiliar codebase: it starts at
`main` and each spawned thread. There is no CLI equivalent, so it can't be used
from a terminal or by an agent. The queries already exist in `lcw-query`:
`run_roots`, `branches`, `RootKind` and `Branch`. Only the front end is missing.

## What to do
Add a `Roots` subcommand, modelled on `run_entries` in `main.rs`:

```text
lcw roots [REPO] [--depth N] [--external]
```

- One line per root, in `run_roots` order:
  - a badge: `main` for the primary entry, `export`, `thread` or `test thread`;
  - the qualified name and `file:line`;
  - `reaches N`;
  - for threads, `spawned by <caller>`.
- With `--depth N > 0`, print `branches(graph, node, path, external)` under each
  root, indented two spaces per level, down to depth N, in the order `branches`
  returns:
  - prefix `⇉ ` when `via` is a spawn;
  - suffix ` ↺` when `recursive` is set, and do not descend further.
- No graph traversal in the CLI ("front ends render, they never traverse"). Only
  call `lcw-query`.

## Done when
- [ ] Running `lcw roots apps/desktop/frontend/tests/fixtures/threads` lists
      `threads::main` first with the `main` badge, then
      `threads::main::<spawned@L10>` and `threads::listener` as `thread`. That is
      the same order `run_tree_e2e.mjs` checks.
- [ ] With `--depth 1`, `main`'s branches are `setup`, `⇉ <spawned@L10>`,
      `⇉ listener`, `report`, in that order.
- [ ] A test asserts both. Analyze the fixture directory from the test, as the
      other CLI commands analyze a path.
- [ ] The README's CLI section shows the command once.

## Verify
`cargo run -p lcw-cli -- roots apps/desktop/frontend/tests/fixtures/threads --depth 2`

## Out of scope
JSON output; changes to `lcw-query`. If something is missing there, stop and say
so in a draft PR.
