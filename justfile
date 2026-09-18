# Live Code Walk - common developer commands.
#
# Mirrors the CI jobs (.github/workflows/ci.yml) and the README so that what you
# run locally matches what CI enforces. Install `just`: https://github.com/casey/just
#
# The default (fast) path never pulls the heavy optional features (`semantic` ->
# rust-analyzer, `gpu` -> wgpu compute); use the `features` recipe for those.

# List available recipes.
default:
    @just --list

# Build the default-feature workspace (no GPU / no rust-analyzer).
build:
    cargo build --workspace --locked

# Run the full workspace test suite (mirrors the `test` CI job).
test:
    cargo test --workspace --locked

# Format check + clippy exactly as CI runs them (mirrors `fmt` + `clippy`).
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Auto-format the whole tree.
fmt:
    cargo fmt --all

# Criterion benchmarks: tree-sitter parse + force-directed layout.
bench:
    cargo bench -p lcw-adapter-treesitter -p lcw-layout

# Optional-feature checks (mirror the `features` + `semantic` CI jobs).
# gpu has a CPU fallback; semantic is check-only because `ra_ap_*` is heavy.
features:
    cargo test -p lcw-layout --features gpu --locked
    cargo check -p lcw-engine --features semantic --locked

# Analyze a repo with the CLI. Usage: `just analyze /path/to/repo`
analyze path=".":
    cargo run -p lcw-cli -- analyze {{path}} --format summary

# Native interactive viewer (winit + wgpu "power mode").
view path=".":
    cargo run -p lcw-cli --features viewer -- view {{path}}

# Desktop app (Tauri v2 + Leptos/WASM) dev server. Needs trunk + tauri-cli:
#   rustup target add wasm32-unknown-unknown
#   cargo install trunk tauri-cli --locked
tauri-dev:
    cd apps/desktop/src-tauri && cargo tauri dev

# Desktop app release bundle (runs `trunk build`, then bundles).
tauri-build:
    cd apps/desktop/src-tauri && cargo tauri build

# Build the release `lcw` binary for the current host.
release-cli:
    cargo build -p lcw-cli --bin lcw --release --locked
