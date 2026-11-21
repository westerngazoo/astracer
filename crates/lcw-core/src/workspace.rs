//! The crate dependency graph of a Cargo workspace: which crates exist, where
//! their sources live, and which crates each one may call into.
//!
//! The engine builds it from `Cargo.toml` files; Layer 1 uses it to refuse a
//! call binding to a crate the caller cannot see (a method name shared by two
//! crates is not evidence when the caller does not depend on one of them),
//! and `lcw-query` reads it for the crate-level view. Pure data, no I/O.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Index of a crate in [`CrateGraph::crates`].
pub type CrateIdx = u32;

/// One crate: a workspace member, or a path dependency inside the repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateInfo {
    /// Package name as written in `Cargo.toml` (`akademos-server`).
    pub name: String,
    /// The crate's directory (the one holding its `Cargo.toml`).
    pub dir: PathBuf,
    /// Listed in `[workspace] members` (or the root package itself).
    pub member: bool,
    /// Listed in `[workspace] default-members`; when that list is absent,
    /// every member is a default member.
    pub default_member: bool,
    /// `[dependencies]` (and `[build-dependencies]`) on other crates here.
    pub deps: Vec<CrateIdx>,
    /// `[dev-dependencies]` on other crates here: visible to tests, benches
    /// and examples only.
    pub dev_deps: Vec<CrateIdx>,
    /// Has a binary target (`src/main.rs`, `src/bin/`, `[[bin]]`).
    pub has_bin: bool,
    /// Builds a `cdylib` / `staticlib` (a wasm module, a C library): an
    /// artifact something outside the workspace loads.
    pub is_dylib: bool,
    pub has_tests: bool,
    pub has_examples: bool,
    pub has_benches: bool,
}

impl CrateInfo {
    /// The name Rust code uses for the crate (`akademos_server`).
    pub fn ident(&self) -> String {
        self.name.replace('-', "_")
    }

    /// A final artifact: something is built *from* this crate rather than
    /// only linked into another one.
    pub fn is_root(&self) -> bool {
        self.has_bin || self.is_dylib
    }
}

/// The crates of a workspace and their dependency edges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateGraph {
    pub crates: Vec<CrateInfo>,
    /// The workspace declares `default-members`.
    pub has_default_members: bool,
}

impl CrateGraph {
    pub fn is_empty(&self) -> bool {
        self.crates.is_empty()
    }

    pub fn get(&self, idx: CrateIdx) -> Option<&CrateInfo> {
        self.crates.get(idx as usize)
    }

    /// The crate owning `path`: the one whose directory is the longest prefix
    /// of it (so a nested crate wins over the workspace root package).
    pub fn crate_of(&self, path: &Path) -> Option<CrateIdx> {
        let path: PathBuf = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect();
        self.crates
            .iter()
            .enumerate()
            .filter(|(_, c)| path.starts_with(&c.dir))
            .max_by_key(|(_, c)| c.dir.components().count())
            .map(|(i, _)| i as CrateIdx)
    }

    pub fn index_of(&self, name: &str) -> Option<CrateIdx> {
        self.crates
            .iter()
            .position(|c| c.name == name)
            .map(|i| i as CrateIdx)
    }

    /// Every crate `from` can reach through dependencies (itself included),
    /// as a membership mask. `dev` adds `from`'s own dev-dependencies (not
    /// theirs: a dev-dependency's dev-dependencies are never built).
    pub fn reach_mask(&self, from: CrateIdx, dev: bool) -> Vec<bool> {
        let mut seen = vec![false; self.crates.len()];
        let Some(start) = self.get(from) else {
            return seen;
        };
        seen[from as usize] = true;
        let mut stack: Vec<CrateIdx> = start.deps.clone();
        if dev {
            stack.extend(start.dev_deps.iter().copied());
        }
        while let Some(c) = stack.pop() {
            let Some(slot) = seen.get_mut(c as usize) else {
                continue;
            };
            if *slot {
                continue;
            }
            *slot = true;
            if let Some(info) = self.get(c) {
                stack.extend(info.deps.iter().copied());
            }
        }
        seen
    }

    /// Crates that depend on `target` directly or transitively through
    /// normal dependencies: the ones that recompile when it changes
    /// (`target` itself excluded).
    pub fn rebuild_set(&self, target: CrateIdx) -> Vec<CrateIdx> {
        let n = self.crates.len();
        let mut hit = vec![false; n];
        let mut stack = vec![target];
        while let Some(t) = stack.pop() {
            for (i, c) in self.crates.iter().enumerate() {
                if !hit[i] && c.deps.contains(&t) {
                    hit[i] = true;
                    stack.push(i as CrateIdx);
                }
            }
        }
        (0..n as CrateIdx)
            .filter(|&i| hit[i as usize] && i != target)
            .collect()
    }

    /// Precomputed "may `from` call into `to`" answers for Layer 1.
    pub fn visibility(&self) -> Visibility {
        let n = self.crates.len();
        let normal = (0..n as CrateIdx)
            .map(|c| self.reach_mask(c, false))
            .collect();
        let dev = (0..n as CrateIdx)
            .map(|c| self.reach_mask(c, true))
            .collect();
        Visibility { normal, dev }
    }
}

/// Dense reachability tables built by [`CrateGraph::visibility`].
#[derive(Debug, Clone, Default)]
pub struct Visibility {
    normal: Vec<Vec<bool>>,
    dev: Vec<Vec<bool>>,
}

impl Visibility {
    /// Whether code in crate `from` may name an item of crate `to`. `dev`
    /// is for test, bench and example code, which also sees dev-dependencies.
    /// Unknown crates (`None`) are not restricted: no evidence either way.
    pub fn allows(&self, from: Option<CrateIdx>, to: Option<CrateIdx>, dev: bool) -> bool {
        let (Some(f), Some(t)) = (from, to) else {
            return true;
        };
        let table = if dev { &self.dev } else { &self.normal };
        table
            .get(f as usize)
            .and_then(|row| row.get(t as usize))
            .copied()
            .unwrap_or(true)
    }
}

/// What part of a project a source file belongs to, judged from its path
/// relative to the repository root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRole {
    Normal,
    /// Test-only code: a `tests/` directory, `*_test.go`, `test_*.py`,
    /// `*.test.ts`, `__tests__/` ...
    Test,
    /// An `examples/` or `benches/` directory.
    Example,
}

/// Classify `rel` (a path relative to the repository root). A `tests`,
/// `examples` or `benches` directory only counts outside a `src` directory,
/// where it is a Cargo target directory rather than an ordinary module.
pub fn path_role(rel: &Path) -> PathRole {
    let comps: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let Some((file, dirs)) = comps.split_last() else {
        return PathRole::Normal;
    };
    for dir in dirs {
        match dir.as_str() {
            "src" | "lib" => break,
            "tests" | "test" | "__tests__" | "testdata" => return PathRole::Test,
            "examples" | "benches" => return PathRole::Example,
            _ => {}
        }
    }
    if dirs.iter().any(|d| d == "__tests__") || is_test_file_name(file) {
        return PathRole::Test;
    }
    PathRole::Normal
}

/// Language conventions for test files that live next to the code.
fn is_test_file_name(file: &str) -> bool {
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    match ext {
        "go" => stem.ends_with("_test"),
        "py" => stem.starts_with("test_") || stem.ends_with("_test") || stem == "conftest",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            stem.ends_with(".test") || stem.ends_with(".spec")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn krate(name: &str, deps: &[CrateIdx], dev: &[CrateIdx]) -> CrateInfo {
        CrateInfo {
            name: name.into(),
            dir: PathBuf::from(format!("crates/{name}")),
            member: true,
            default_member: true,
            deps: deps.to_vec(),
            dev_deps: dev.to_vec(),
            has_bin: false,
            is_dylib: false,
            has_tests: false,
            has_examples: false,
            has_benches: false,
        }
    }

    /// server -> store -> content, cli -> content, cli dev-> server.
    fn graph() -> CrateGraph {
        CrateGraph {
            crates: vec![
                krate("content", &[], &[]),
                krate("store", &[0], &[]),
                krate("server", &[1], &[]),
                krate("cli", &[0], &[2]),
            ],
            has_default_members: false,
        }
    }

    #[test]
    fn visibility_follows_transitive_deps_only() {
        let v = graph().visibility();
        assert!(
            v.allows(Some(2), Some(0), false),
            "server sees content via store"
        );
        assert!(
            !v.allows(Some(2), Some(3), false),
            "server does not see cli"
        );
        assert!(!v.allows(Some(0), Some(1), false), "deps point one way");
        assert!(
            !v.allows(Some(3), Some(2), false),
            "a dev-dep is not normal"
        );
        assert!(v.allows(Some(3), Some(2), true), "tests see dev-deps");
        assert!(v.allows(Some(3), Some(1), true), "...and their deps");
        assert!(
            v.allows(None, Some(3), false),
            "unknown crates are unrestricted"
        );
    }

    #[test]
    fn crate_of_picks_the_longest_dir_prefix() {
        let mut g = graph();
        g.crates.push(krate("root", &[], &[]));
        g.crates[4].dir = PathBuf::new();
        assert_eq!(g.crate_of(Path::new("crates/store/src/lib.rs")), Some(1));
        assert_eq!(g.crate_of(Path::new("build.rs")), Some(4));
    }

    #[test]
    fn rebuild_set_is_transitive_reverse_deps() {
        let g = graph();
        assert_eq!(g.rebuild_set(0), vec![1, 2, 3]);
        assert_eq!(
            g.rebuild_set(2),
            Vec::<CrateIdx>::new(),
            "dev-deps do not rebuild"
        );
    }

    #[test]
    fn path_roles() {
        let role = |p: &str| path_role(Path::new(p));
        assert_eq!(role("crates/a/tests/smoke.rs"), PathRole::Test);
        assert_eq!(role("crates/a/examples/demo.rs"), PathRole::Example);
        assert_eq!(role("benches/parse.rs"), PathRole::Example);
        assert_eq!(role("crates/a/src/tests/mod.rs"), PathRole::Normal);
        assert_eq!(role("crates/a/src/lib.rs"), PathRole::Normal);
        assert_eq!(role("pkg/server/handler_test.go"), PathRole::Test);
        assert_eq!(role("app/test_models.py"), PathRole::Test);
        assert_eq!(role("web/src/App.test.tsx"), PathRole::Test);
        assert_eq!(role("web/src/__tests__/x.ts"), PathRole::Test);
        assert_eq!(role("web/src/contest.ts"), PathRole::Normal);
    }
}
