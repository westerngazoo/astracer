# T02: Bring the desktop README up to date with the UI

Size: S · Area: docs · Touches: `apps/desktop/README.md`

## Why
The README describes an Explorer that no longer exists:

- It still says "*Start here*: the entry points … Below it the **Outline**". The
  Explorer is now three tabs: **Run tree** (the default), **Outline** and
  **Entries**.
- The file tree lists `src/viewer.rs`, which is gone. The frontend's sources are
  `src/app.rs`, `src/main.rs` and `src/transport.rs`, plus `tests/` (browser tests
  and a fixture).

## What to do
Rewrite the "Explorer (left)" bullet and fix the file tree. The sources of truth
are:

- `ExplorerTab` and the tab buttons in `apps/desktop/frontend/src/app.rs`, which
  give the tab names and their tooltips.
- `apps/desktop/frontend/tests/run_tree_e2e.mjs`, which describes the Run tree's
  behaviour:
  - roots are the program entry, exported entry points and spawned threads;
  - `⇉` marks a branch that starts a thread;
  - `↺` marks recursion;
  - clicking a row opens it.

## Done when
- [ ] The Explorer bullet names the three tabs and says what each shows, in the
      README's existing style (short, concrete, no marketing).
- [ ] The file tree matches `ls apps/desktop/frontend apps/desktop/frontend/src`.
- [ ] Nothing else in the README changes, except facts you find to be wrong while
      checking (list them in the PR).

## Verify
Run the UI on the fixture (`lcw-dev ui apps/desktop/frontend/tests/fixtures/threads`)
and read the README against what you see.

## Out of scope
The top-level `README.md`; screenshots.
