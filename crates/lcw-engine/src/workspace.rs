//! Build the [`CrateGraph`] of a Cargo workspace from its `Cargo.toml` files:
//! the members (globs expanded, `exclude` honored), `default-members`, and
//! every dependency that points at another crate in the repository — a
//! `path = ".."` dependency or a `workspace = true` one whose
//! `[workspace.dependencies]` entry has a path. Registry and git
//! dependencies are not crates in the analyzed tree and are left out.
//!
//! No `cargo metadata`: it needs a toolchain, may hit the network and costs
//! seconds; the manifests alone carry everything Layer 1 needs.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use lcw_core::{CrateGraph, CrateIdx, CrateInfo};
use toml::{Table, Value};

/// The crate graph of the workspace rooted at `root`, or `None` when `root`
/// has no readable `Cargo.toml`.
pub fn discover_crates(root: &Path) -> Option<CrateGraph> {
    let root_manifest = read_manifest(&root.join("Cargo.toml"))?;
    let mut b = Builder {
        root: normalize(root),
        index: HashMap::new(),
        crates: Vec::new(),
        manifests: Vec::new(),
        workspace_paths: BTreeMap::new(),
    };
    let ws = root_manifest.get("workspace").and_then(Value::as_table);
    if let Some(deps) = ws
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
    {
        for (name, spec) in deps {
            if let Some(p) = spec.get("path").and_then(Value::as_str) {
                b.workspace_paths
                    .insert(name.clone(), normalize(&root.join(p)));
            }
        }
    }
    let excluded: Vec<PathBuf> = string_list(ws, "exclude")
        .iter()
        .map(|e| normalize(&root.join(e)))
        .collect();
    let mut members: Vec<PathBuf> = string_list(ws, "members")
        .iter()
        .flat_map(|m| expand_glob(root, m))
        .filter(|d| !excluded.iter().any(|e| d.starts_with(e)))
        .filter(|d| d.join("Cargo.toml").is_file())
        .collect();
    if root_manifest.contains_key("package") {
        members.insert(0, root.to_path_buf());
    }
    let defaults: Vec<PathBuf> = string_list(ws, "default-members")
        .iter()
        .flat_map(|m| expand_glob(root, m))
        .collect();
    for dir in &members {
        if let Some(idx) = b.add(dir) {
            b.crates[idx as usize].member = true;
        }
    }
    // Dependencies may add non-member path crates while being resolved.
    let mut next = 0;
    while next < b.crates.len() {
        b.link(next);
        next += 1;
    }
    let has_default_members = !defaults.is_empty();
    for c in &mut b.crates {
        c.default_member = c.member && (!has_default_members || defaults.contains(&c.dir));
    }
    (!b.crates.is_empty()).then_some(CrateGraph {
        crates: b.crates,
        has_default_members,
    })
}

struct Builder {
    root: PathBuf,
    index: HashMap<PathBuf, CrateIdx>,
    crates: Vec<CrateInfo>,
    manifests: Vec<Table>,
    /// `[workspace.dependencies]` entries that are paths, by dependency name.
    workspace_paths: BTreeMap<String, PathBuf>,
}

impl Builder {
    /// Register the crate in `dir` (once), reading its manifest.
    fn add(&mut self, dir: &Path) -> Option<CrateIdx> {
        let dir = normalize(dir);
        if let Some(&i) = self.index.get(&dir) {
            return Some(i);
        }
        // A crate outside the repository has no sources in the graph.
        if !dir.starts_with(&self.root) {
            return None;
        }
        let manifest = read_manifest(&dir.join("Cargo.toml"))?;
        let package = manifest.get("package").and_then(Value::as_table)?;
        let name = package.get("name").and_then(Value::as_str)?.to_string();
        let lib = manifest.get("lib").and_then(Value::as_table);
        let crate_types = lib
            .map(|l| string_list(Some(l), "crate-type"))
            .unwrap_or_default();
        let info = CrateInfo {
            name,
            dir: dir.clone(),
            member: false,
            default_member: false,
            deps: Vec::new(),
            dev_deps: Vec::new(),
            has_bin: dir.join("src/main.rs").is_file()
                || dir.join("src/bin").is_dir()
                || manifest.get("bin").is_some(),
            is_dylib: crate_types
                .iter()
                .any(|t| t == "cdylib" || t == "staticlib"),
            has_tests: dir.join("tests").is_dir(),
            has_examples: dir.join("examples").is_dir() || manifest.get("example").is_some(),
            has_benches: dir.join("benches").is_dir() || manifest.get("bench").is_some(),
        };
        let idx = self.crates.len() as CrateIdx;
        self.crates.push(info);
        self.manifests.push(manifest);
        self.index.insert(dir, idx);
        Some(idx)
    }

    /// Fill in crate `i`'s dependency edges.
    fn link(&mut self, i: usize) {
        let manifest = self.manifests[i].clone();
        let dir = self.crates[i].dir.clone();
        let mut deps = Vec::new();
        let mut dev = Vec::new();
        let mut tables: Vec<(&str, &Table)> = Vec::new();
        for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
            if let Some(t) = manifest.get(key).and_then(Value::as_table) {
                tables.push((key, t));
            }
        }
        // `[target.'cfg(..)'.dependencies]` count like the plain tables.
        if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
            for spec in targets.values().filter_map(Value::as_table) {
                for key in ["dependencies", "build-dependencies", "dev-dependencies"] {
                    if let Some(t) = spec.get(key).and_then(Value::as_table) {
                        tables.push((key, t));
                    }
                }
            }
        }
        for (key, table) in tables {
            for (name, spec) in table {
                let Some(target) = self.dep_dir(&dir, name, spec) else {
                    continue;
                };
                let Some(idx) = self.add(&target) else {
                    continue;
                };
                if idx as usize == i {
                    continue;
                }
                let list = if key == "dev-dependencies" {
                    &mut dev
                } else {
                    &mut deps
                };
                if !list.contains(&idx) {
                    list.push(idx);
                }
            }
        }
        dev.retain(|d| !deps.contains(d));
        self.crates[i].deps = deps;
        self.crates[i].dev_deps = dev;
    }

    /// The directory a dependency points at, when it is a crate in the repo.
    fn dep_dir(&self, crate_dir: &Path, name: &str, spec: &Value) -> Option<PathBuf> {
        if let Some(p) = spec.get("path").and_then(Value::as_str) {
            return Some(normalize(&crate_dir.join(p)));
        }
        if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
            let key = spec.get("package").and_then(Value::as_str).unwrap_or(name);
            return self
                .workspace_paths
                .get(name)
                .or_else(|| self.workspace_paths.get(key))
                .cloned();
        }
        None
    }
}

fn read_manifest(path: &Path) -> Option<Table> {
    let text = std::fs::read_to_string(path).ok()?;
    text.parse::<Table>().ok()
}

fn string_list(table: Option<&Table>, key: &str) -> Vec<String> {
    table
        .and_then(|t| t.get(key))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Expand a workspace member pattern (`crates/*`, `libs/foo-?`) relative to
/// `root` into existing directories. Patterns without wildcards pass through.
fn expand_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    for seg in pattern.split('/').filter(|s| !s.is_empty() && *s != ".") {
        let mut next = Vec::new();
        for d in &dirs {
            if seg.contains(['*', '?']) {
                let Ok(entries) = std::fs::read_dir(d) else {
                    continue;
                };
                let mut found: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .filter(|e| e.path().is_dir())
                    .filter(|e| wildcard(seg, &e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .collect();
                found.sort();
                next.extend(found);
            } else {
                next.push(d.join(seg));
            }
        }
        dirs = next;
    }
    dirs.iter().map(|d| normalize(d)).collect()
}

/// `*` / `?` wildcard match of a single path segment.
fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|k| go(rest, &n[k..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// Lexically resolve `.` and `..` so `crates/a/../b` and `crates/b` agree.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A short stable fingerprint of a crate graph, recorded with the cached
/// graph so a dependency change (no source edited) still invalidates it.
pub fn fingerprint(graph: Option<&CrateGraph>) -> String {
    let Some(g) = graph else {
        return "none".into();
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |s: &str| {
        for b in s.bytes().chain([0xff]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for c in &g.crates {
        feed(&c.name);
        feed(&c.dir.to_string_lossy());
        feed(&format!(
            "{:?}{:?}{}{}",
            c.deps, c.dev_deps, c.member, c.default_member
        ));
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lcw-ws-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn reads_members_globs_workspace_and_path_deps() {
        let root = temp_root("members");
        write(
            &root,
            "Cargo.toml",
            r#"[workspace]
members = ["crates/*", "tools/gen"]
exclude = ["crates/skip"]
default-members = ["crates/server"]
[workspace.dependencies]
store = { path = "crates/store" }
serde = "1"
"#,
        );
        write(
            &root,
            "crates/store/Cargo.toml",
            "[package]\nname = \"store\"\n[dependencies]\nserde = { workspace = true }\n",
        );
        write(
            &root,
            "crates/server/Cargo.toml",
            "[package]\nname = \"server\"\n[dependencies]\nstore = { workspace = true }\n[dev-dependencies]\ncli = { path = \"../cli\" }\n",
        );
        write(&root, "crates/server/src/main.rs", "fn main() {}");
        write(&root, "crates/cli/Cargo.toml", "[package]\nname = \"cli\"\n[target.'cfg(unix)'.dependencies]\nvendored = { path = \"../../vendor/v\" }\n");
        write(&root, "crates/cli/examples/demo.rs", "fn main() {}");
        write(
            &root,
            "crates/skip/Cargo.toml",
            "[package]\nname = \"skip\"\n",
        );
        write(
            &root,
            "tools/gen/Cargo.toml",
            "[package]\nname = \"gen\"\n[lib]\ncrate-type = [\"cdylib\"]\n",
        );
        write(
            &root,
            "vendor/v/Cargo.toml",
            "[package]\nname = \"vendored\"\n",
        );

        let g = discover_crates(&root).expect("workspace");
        let names: Vec<&str> = g.crates.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"store") && names.contains(&"gen") && names.contains(&"vendored"));
        assert!(!names.contains(&"skip"), "excluded: {names:?}");
        let idx = |n: &str| g.index_of(n).unwrap();
        let server = &g.crates[idx("server") as usize];
        assert_eq!(server.deps, vec![idx("store")]);
        assert_eq!(server.dev_deps, vec![idx("cli")]);
        assert!(server.has_bin && server.default_member);
        assert!(g.has_default_members);
        assert!(!g.crates[idx("store") as usize].default_member);
        assert!(g.crates[idx("gen") as usize].is_dylib);
        let cli = &g.crates[idx("cli") as usize];
        assert_eq!(cli.deps, vec![idx("vendored")]);
        assert!(cli.has_examples);
        let vendored = &g.crates[idx("vendored") as usize];
        assert!(!vendored.member, "a path dep, not a member");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_single_package_is_a_one_crate_graph_and_no_manifest_is_none() {
        let root = temp_root("single");
        assert!(discover_crates(&root).is_none());
        write(&root, "Cargo.toml", "[package]\nname = \"solo\"\n");
        let g = discover_crates(&root).unwrap();
        assert_eq!(g.crates.len(), 1);
        assert!(g.crates[0].member && g.crates[0].default_member);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*", "abc"));
        assert!(wildcard("lcw-*", "lcw-core"));
        assert!(!wildcard("lcw-*", "core"));
        assert!(wildcard("a?c", "abc"));
    }
}
