//! On-disk per-file **fragment cache** backing incremental analysis.
//!
//! Each entry stores one file's [`FileFragment`] keyed by its path and tagged
//! with a content hash. On the next run the engine re-extracts only the files
//! whose hash changed and reuses the rest, then re-runs the (cheap) global
//! resolution over the union (plan: incremental analysis for giant repos).
//!
//! The cache is a plain JSON document. It is intentionally conservative: any
//! read error, schema-version bump or adapter mismatch simply yields an empty
//! cache (a cold, correct run) rather than a hard failure.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use lcw_core::FileFragment;
use serde::{Deserialize, Serialize};

/// Bump when the on-disk layout or the [`FileFragment`] schema changes so stale
/// caches are discarded instead of mis-deserialized.
const CACHE_VERSION: u32 = 1;

/// File name of the cache document inside the cache directory.
const CACHE_FILE: &str = "fragments.json";

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    hash: u64,
    fragment: FileFragment,
}

/// A content-addressed cache of per-file fragments for one repository + adapter.
#[derive(Debug, Serialize, Deserialize)]
pub struct FragmentCache {
    version: u32,
    /// Adapter that produced these fragments; a different adapter invalidates
    /// the cache (fragments are adapter-specific).
    adapter: String,
    entries: HashMap<PathBuf, Entry>,
    /// Whether anything changed since load (so we can skip pointless writes).
    #[serde(skip)]
    dirty: bool,
}

impl FragmentCache {
    /// An empty cache tagged for `adapter`.
    pub fn new(adapter: &str) -> Self {
        FragmentCache {
            version: CACHE_VERSION,
            adapter: adapter.to_string(),
            entries: HashMap::new(),
            dirty: false,
        }
    }

    /// Load the cache at `path`, falling back to an empty one on any problem
    /// (missing file, corrupt JSON, version or adapter mismatch).
    pub fn load(path: &Path, adapter: &str) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::new(adapter);
        };
        match serde_json::from_slice::<FragmentCache>(&bytes) {
            Ok(mut cache) if cache.version == CACHE_VERSION && cache.adapter == adapter => {
                cache.dirty = false;
                cache
            }
            _ => Self::new(adapter),
        }
    }

    /// The cached fragment for `path`, but only if its stored hash matches
    /// `hash` (i.e. the file's content is unchanged).
    pub fn get(&self, path: &Path, hash: u64) -> Option<&FileFragment> {
        self.entries
            .get(path)
            .filter(|e| e.hash == hash)
            .map(|e| &e.fragment)
    }

    /// Insert or replace the fragment for `path`.
    pub fn insert(&mut self, path: PathBuf, hash: u64, fragment: FileFragment) {
        self.entries.insert(path, Entry { hash, fragment });
        self.dirty = true;
    }

    /// Drop entries for files not in `keep` (deleted or now-excluded files).
    pub fn retain(&mut self, keep: &HashSet<PathBuf>) {
        let before = self.entries.len();
        self.entries.retain(|p, _| keep.contains(p));
        if self.entries.len() != before {
            self.dirty = true;
        }
    }

    /// Whether the cache changed since it was loaded.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persist the cache to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, bytes)
    }
}

/// Content hash of a file's text. Uses the standard library's fixed-seed
/// [`DefaultHasher`], which is deterministic across runs (unlike `RandomState`),
/// so a cache written by one invocation is valid for the next.
///
/// [`DefaultHasher`]: std::collections::hash_map::DefaultHasher
pub fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Mixing the length in guards against pathological prefix collisions.
    text.len().hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

/// Resolve the cache file for `root` given the `[cache]` config.
///
/// * An explicit `dir` is used verbatim (relative to `root` if not absolute).
/// * Otherwise a per-repository directory under the user's cache home is used,
///   so the cache never pollutes the analyzed tree.
///
/// Returns `None` only if no cache home can be determined and no explicit dir
/// was given (in practice always `Some`, since we fall back to the temp dir).
pub fn cache_file(root: &Path, dir: &str) -> Option<PathBuf> {
    if !dir.is_empty() {
        let base = PathBuf::from(dir);
        let base = if base.is_absolute() {
            base
        } else {
            root.join(base)
        };
        return Some(base.join(CACHE_FILE));
    }
    let home = cache_home()?;
    let key = hash_path(root);
    Some(
        home.join("livewalk")
            .join(format!("{key:016x}"))
            .join(CACHE_FILE),
    )
}

/// The user's cache home: `$XDG_CACHE_HOME`, then `$HOME/.cache`, then
/// `%LOCALAPPDATA%` (Windows), finally the OS temp dir.
fn cache_home() -> Option<PathBuf> {
    for var in ["XDG_CACHE_HOME", "HOME", "LOCALAPPDATA"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                let base = PathBuf::from(v);
                return Some(if var == "HOME" {
                    base.join(".cache")
                } else {
                    base
                });
            }
        }
    }
    Some(std::env::temp_dir())
}

/// Stable-ish hash of a repository root for use as a cache subdirectory name.
fn hash_path(p: &Path) -> u64 {
    let key = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(hash_text("fn a() {}"), hash_text("fn a() { b(); }"));
        assert_eq!(hash_text("same"), hash_text("same"));
    }

    #[test]
    fn get_is_hash_validated() {
        let mut c = FragmentCache::new("treesitter-rust");
        let p = PathBuf::from("src/a.rs");
        c.insert(p.clone(), 42, FileFragment::new(&p));
        assert!(c.get(&p, 42).is_some());
        assert!(c.get(&p, 43).is_none(), "stale hash must miss");
    }

    #[test]
    fn explicit_relative_dir_resolves_against_root() {
        let f = cache_file(Path::new("/repo"), ".livewalk-cache").unwrap();
        assert!(f.ends_with("fragments.json"));
        assert!(f.starts_with("/repo/.livewalk-cache"));
    }
}
