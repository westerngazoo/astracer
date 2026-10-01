# T03: Run the GPU tests for real in CI

Size: S · Area: CI + tests · Touches: `.github/workflows/ci.yml`,
`crates/lcw-render/src/lib.rs`, `crates/lcw-layout/src/gpu.rs`

## Why
GitHub's Linux runners have no GPU, so two tests skip without failing:

- the offscreen render and GPU-picking test (`headless_tests` in
  `crates/lcw-render/src/lib.rs`), which prints "no GPU adapter; skipping";
- the GPU-compute layout test (`gpu_layout_matches_node_count_when_available` in
  `crates/lcw-layout/src/gpu.rs`), which falls back to the CPU.

Mesa's software Vulkan driver (lavapipe, package `mesa-vulkan-drivers`) gives
wgpu a real adapter on a CPU. Both tests have been run on it and pass. This is how
the wgpu 24 → 30 port was verified.

## What to do
1. In `ci.yml`, add a Linux-only step to the `test` job (`if: runner.os ==
   'Linux'`) and to the `features` (gpu layout) job:
   `sudo apt-get update && sudo apt-get install -y mesa-vulkan-drivers`.
2. Set `LCW_REQUIRE_GPU: "1"` on those Linux test steps. Make both tests **fail**
   instead of skipping when that variable is set and no adapter is found:
   - in `try_device()` / its caller in `lcw-render`;
   - in the `Err(GpuLayoutError::NoAdapter)` arm in `lcw-layout`.

   Without the variable, behaviour is unchanged, so laptops and the macOS and
   Windows jobs still skip quietly.
3. Fix the comment in the `features` job that says it "runs green on a headless
   runner with no GPU adapter present".

## Done when
- [ ] CI is green on the PR.
- [ ] The Linux `test` job's log has no "no GPU adapter; skipping" line. The PR
      links the log line where `renders_offscreen_when_gpu_available` passed.
- [ ] Both tests read `LCW_REQUIRE_GPU` the same way, through one small helper
      per crate, and each test has a doc comment saying why it exists.

## Verify
Locally (macOS has a Metal adapter):

```sh
LCW_REQUIRE_GPU=1 cargo test -p lcw-render --locked
LCW_REQUIRE_GPU=1 cargo test -p lcw-layout --features gpu --locked
```

## Out of scope
Golden-image tests; running the browser tests in CI (separate tasks).
