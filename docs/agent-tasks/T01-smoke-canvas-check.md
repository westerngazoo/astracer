# T01: Make the UI smoke test's "blank canvas" check real

Size: S · Area: browser tests · Touches: `apps/desktop/frontend/tests/ui_smoke.mjs`

## Why
`ui_smoke.mjs` warns `[ui] graph canvas is completely transparent: nothing was
rendered` on every run, even when the graph is plainly drawn. Screenshots and the
other browser tests confirm the graph is there. The check (around line 142) copies
the WebGL canvas with `drawImage` in the page. That runs after the frame has been
composited, and since the canvas has no `preserveDrawingBuffer`, the browser has
already cleared the drawing buffer by then, so the copy is always empty. A check
that always fires hides a real blank canvas.

## What to do
Replace the in-page readback with a Playwright screenshot of the canvas element:
`await page.locator('canvas.graph').screenshot()`. Decide "blank" from that image.
Decode the PNG with Node's built-in `zlib` (no new npm dependencies) and count the
pixels that differ from the top-left pixel. A canvas that drew nothing is a single
colour. `crates_e2e.mjs` already compares canvas screenshots, which you can use as
a reference.

## Done when
- [ ] A normal run against this repository no longer reports the blank-canvas
      problem. The other three "problems" (WebGPU and integrity warnings from
      headless Chromium) may remain; they are out of scope.
- [ ] With rendering deliberately broken, the check *does* report it. To break it,
      temporarily `return` at the top of `WebViewer::render` in
      `crates/lcw-render/src/web.rs` and rebuild the UI. Revert this before
      committing, and say in the PR that you saw it fire.
- [ ] No new npm or Cargo dependencies.

## Verify
The test file's header shows how to serve the UI (`lcw-dev ui <repo> --no-open`)
and run it with Playwright.

## Out of scope
The other smoke checks; the headless-Chromium warnings.
