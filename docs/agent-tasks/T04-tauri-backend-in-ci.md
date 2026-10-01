# T04: Compile the Tauri backend in CI

Size: S · Area: CI · Touches: `.github/workflows/ci.yml`

## Why
`apps/desktop/src-tauri` is outside the workspace and has never been compiled in
CI. The `lockfile (desktop backend)` job only checks that its `Cargo.lock` is
current. Changes to it (for example `GraphView` gaining `groups`) have been
reviewed by eye only, because compiling it needs GTK and WebKitGTK headers.

## What to do
Add a job `desktop backend (cargo check)` on `ubuntu-latest`:

1. Install Tauri v2's Linux build prerequisites. Start from Tauri's documented
   list: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`,
   `librsvg2-dev`, `libxdo-dev`, `libssl-dev`, `build-essential`. Add or drop
   packages until it builds, and list the final set in the PR.
2. Use `Swatinem/rust-cache@v2` with `workspaces: apps/desktop/src-tauri`.
3. Run `cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --locked`.

No frontend build is needed: `apps/desktop/src-tauri/build.rs` writes a placeholder
`dist/index.html` when the real bundle is missing.

## Done when
- [ ] The new job is green on the PR, and its log shows the `livewalk_desktop_lib`
      crate being checked.
- [ ] If `cargo clippy` (same flags plus `-- -D warnings`) is also clean, the job
      runs that instead of `check`. If not, the PR lists the warnings and keeps
      `check`; do not fix them in this task.

## Verify
The CI run on the PR is the test. On macOS, the same `cargo check` command works
locally without the apt packages.

## Out of scope
Building a bundle (`cargo tauri build`); the macOS and Windows desktop builds.
