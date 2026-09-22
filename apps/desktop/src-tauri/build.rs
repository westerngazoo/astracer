//! Build script for the Tauri backend.
//!
//! `generate_context!` embeds `frontendDist` (`../frontend/dist`) at compile
//! time, so that directory has to exist before the macro expands. That used to
//! be arranged by checking a placeholder page into git, which is a trap: git
//! owns the file, so `trunk build` writing the real bundle over it dirties the
//! working tree, and any later `checkout`, `pull`, `stash` or `restore`
//! silently puts the placeholder *back* over a bundle that was already built.
//! The app or a static server then shows "the bundle has not been built" with
//! no build step anywhere in sight, which is a confusing way to learn that a
//! branch switch undid your work.
//!
//! Generating it here instead lets `dist/` stay entirely git-ignored: git can
//! never clobber a real bundle, and a fresh checkout still compiles.

use std::path::Path;

fn main() {
    ensure_frontend_dist(Path::new("../frontend/dist"));
    tauri_build::build();
}

/// Write the placeholder only when `index.html` is missing. Once `trunk build`
/// has produced the real bundle this must leave it alone.
fn ensure_frontend_dist(dist: &Path) {
    let index = dist.join("index.html");
    if index.exists() {
        return;
    }
    std::fs::create_dir_all(dist).unwrap_or_else(|e| panic!("create {}: {e}", dist.display()));
    std::fs::write(&index, PLACEHOLDER)
        .unwrap_or_else(|e| panic!("write {}: {e}", index.display()));
}

const PLACEHOLDER: &str = include_str!("placeholder.html");
