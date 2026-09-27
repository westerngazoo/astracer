# CI / CD & packaging

This repo ships its automation as plain GitHub Actions workflows plus a
Dependabot config. Everything lives under [`.github/`](../.github); a root
[`justfile`](../justfile) mirrors the same commands for local use.

## Shared choices

| Thing | Choice | Why |
|-------|--------|-----|
| Toolchain | [`dtolnay/rust-toolchain@stable`](https://github.com/dtolnay/rust-toolchain) | Fast, no extra rustup churn; `rust-version` in `Cargo.toml` is 1.80 (1.82 for the desktop app), so stable is comfortably ahead. |
| Cache | [`Swatinem/rust-cache@v2`](https://github.com/Swatinem/rust-cache) | Caches `~/.cargo` + `target/` keyed on the lockfile. |
| Checkout | `actions/checkout@v4` | — |
| Releases | [`softprops/action-gh-release@v2`](https://github.com/softprops/action-gh-release) | Creates the tag release and uploads assets. |
| Lockfile | every `cargo` call uses `--locked` | CI must build the committed `Cargo.lock` and fail loudly rather than rewrite it. |

`apps/desktop/*` is **excluded** from the Cargo workspace (`[workspace].exclude`
in `Cargo.toml`), so `--workspace` never builds the Tauri/wasm app. It is only
built in the dedicated, best-effort desktop jobs.

## CI — `.github/workflows/ci.yml`

Triggers on pushes to `main` and on all pull requests. In-progress runs for the
same ref are cancelled (`concurrency`).

| Job | Runner(s) | Command |
|-----|-----------|---------|
| `fmt` | `ubuntu-latest` | `cargo fmt --all -- --check` |
| `clippy` | `ubuntu-latest` | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| `test` | `ubuntu-latest`, `macos-latest` | `cargo test --workspace --locked` |
| `features` | `ubuntu-latest` | `cargo test -p lcw-layout --features gpu --locked` |
| `semantic` | `ubuntu-latest` | `cargo check -p lcw-engine --features semantic --locked` |
| `wasm` | `ubuntu-latest` | `cargo check --target wasm32-unknown-unknown --locked` in `apps/desktop/frontend` |

- The workspace sets clippy lints to `warn`; CI promotes them to errors with
  `-D warnings`.
- `features` exercises `lcw-layout`'s `gpu` (wgpu-compute) feature. It has a CPU
  fallback, so it runs green headless with no GPU adapter.
- `wasm` type-checks the Leptos frontend for `wasm32-unknown-unknown`. It is
  the only job that compiles `apps/desktop/frontend`, so it is what catches a
  change in `lcw-core` / `lcw-query` / `lcw-render` that breaks the UI. No
  `trunk`, webview or GPU is needed for a `cargo check`.
- `semantic` is kept **separate and heavy**: `lcw-engine`'s `semantic` feature
  pulls the rust-analyzer `ra_ap_*` crates. It only `cargo check`s (no link) and
  uses a dedicated, reusable cache key (`shared-key: semantic`,
  `cache-on-failure: true`).

## Release — `.github/workflows/release.yml`

Triggers on tags matching `v*` (e.g. `v0.1.0`). Needs `contents: write`.

### `cli` — the `lcw` binary

Builds `cargo build -p lcw-cli --bin lcw --release --locked --target <triple>`
and uploads a per-target archive to the GitHub Release:

| Target | Runner | Notes |
|--------|--------|-------|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | native |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` | cross-compiled; installs `gcc-aarch64-linux-gnu` (linker + `cc` for tree-sitter C) |
| `aarch64-apple-darwin` | `macos-latest` | native (Apple Silicon) |
| `x86_64-apple-darwin` | `macos-latest` | cross-compiled from Apple Silicon (Apple clang targets both arches) |
| `x86_64-pc-windows-msvc` | `windows-latest` | native |

Archives are `lcw-<tag>-<target>.tar.gz` (Unix) / `.zip` (Windows) and include
the binary + `README.md`. The release build uses default features only, so the
CLI stays analysis-only (no `viewer`/wgpu, no rust-analyzer).

### `desktop` — Tauri bundle (best-effort)

`continue-on-error: true`. Runs on `ubuntu-latest`, installs the Tauri v2 Linux
system deps + `wasm32-unknown-unknown` + `trunk` + `tauri-cli@^2`, then:

```bash
cd apps/desktop/src-tauri && cargo tauri build
```

`cargo tauri build` runs the frontend's `beforeBuildCommand` (`trunk build`) and
bundles the app. Any produced `.AppImage`/`.deb`/`.rpm` are uploaded to the
release. Because it is best-effort, a bundler failure never blocks the CLI
release.

### Cutting a release

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Dependabot — `.github/dependabot.yml`

Weekly updates, grouped to reduce PR noise:

- `cargo` at `/` (the workspace + all members, one `Cargo.lock`).
- `cargo` at `/apps/desktop/src-tauri` and `/apps/desktop/frontend` (each is out
  of the workspace with its own lockfile).
- `github-actions` at `/` (keeps the actions above current).

## Local mirror — `justfile`

`just build` / `test` / `lint` / `bench` / `features` / `wasm-check` / `analyze` /
`view` / `tauri-dev` / `tauri-build` mirror the CI commands so local runs match CI.
