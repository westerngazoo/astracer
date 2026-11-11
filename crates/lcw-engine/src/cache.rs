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
///
/// 2: fragments carry spawn sites (`CallKind::Spawn`) and synthetic nodes for
/// spawned closures; a version-1 fragment would silently lack both, so a warm
/// cache would hide every thread root until the file happened to change.
///
/// 3: method calls carry an inferred receiver type (`RawCall::receiver`) and
/// calls inside macro arguments are extracted; a version-2 fragment would
/// load fine but drop those edges until the file changed.
pub(crate) const CACHE_VERSION: u32 = 3;

/// File name of the cache document inside the cache directory.
const CACHE_FILE: &str = "fragments.json";

/// Embedded graph database file name (see `lcw-graph-store`).
pub const GRAPH_DB_FILE: &str = "graph.sqlite";

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

/// Resolve the cache file for `root` given the `[cache]` config and language
/// variant (see [`repo_cache_dir`]).
///
/// `variant` isolates caches by language or adapter mode (e.g. `rust`, `cpp`,
/// `semantic`). See [`lcw_config::Config::cache_variant`].
pub fn repo_cache_dir(root: &Path, dir: &str, variant: &str) -> Option<PathBuf> {
    let dir = dir.trim();
    let variant = variant.trim();
    let variant = if variant.is_empty() { "rust" } else { variant };
    if !dir.is_empty() {
        let base = PathBuf::from(dir);
        let base = if base.is_absolute() {
            base
        } else {
            root.join(base)
        };
        return Some(base.join(variant));
    }
    let home = cache_home()?;
    let key = repo_cache_key(root);
    Some(
        home
            .join("livewalk")
            .join(format!("{key:016x}-{variant}")),
    )
}

pub fn cache_file(root: &Path, dir: &str, variant: &str) -> Option<PathBuf> {
    repo_cache_dir(root, dir, variant).map(|d| d.join(CACHE_FILE))
}

/// Path to the embedded graph SQLite file for `root` (same dir as [`cache_file`]).
pub fn graph_db_file(root: &Path, dir: &str, variant: &str) -> Option<PathBuf> {
    repo_cache_dir(root, dir, variant).map(|d| d.join(GRAPH_DB_FILE))
}

/// Create the per-repo cache directory if needed. Analysis does not require a
/// pre-existing cache; this is called before writes so `fragments.json` and
/// `graph.sqlite` can land in a fresh `livewalk/{hash}-{variant}/` folder.
pub fn ensure_repo_cache_dir(root: &Path, dir: &str, variant: &str) -> std::io::Result<()> {
    if let Some(p) = repo_cache_dir(root, dir, variant) {
        std::fs::create_dir_all(&p)?;
    }
    Ok(())
}

/// Environment variable that overrides the cache home (when `[cache].dir` is
/// empty). Useful for tests and for pinning all Livewalk caches to one folder.
pub const CACHE_HOME_ENV: &str = "LIVEWALK_CACHE_HOME";

/// The user's cache home: [`CACHE_HOME_ENV`], then `$XDG_CACHE_HOME`, then
/// `$HOME/.cache`, then `%LOCALAPPDATA%` (Windows), finally the OS temp dir.
pub fn cache_home() -> Option<PathBuf> {
    if let Ok(v) = std::env::var(CACHE_HOME_ENV) {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
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
pub fn repo_cache_key(p: &Path) -> u64 {
    let key = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// Root directory for all Livewalk caches (`~/.cache/livewalk` by default).
pub fn livewalk_cache_root() -> Option<PathBuf> {
    cache_home().map(|h| h.join("livewalk"))
}

/// One on-disk cache bucket under [`livewalk_cache_root`].
#[derive(Debug, Clone)]
pub struct CacheDirEntry {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Newest modification time among the directory tree (best-effort).
    pub modified: Option<std::time::SystemTime>,
    /// Hash-only dir superseded by a `{hash}-{variant}` sibling.
    pub legacy_superseded: bool,
}

/// Options for [`clean_cache_dirs`].
#[derive(Debug, Clone, Default)]
pub struct CacheCleanOptions {
    /// Delete matching entries (default is list-only).
    pub yes: bool,
    /// Only remove legacy `{16-hex}` buckets (no `-{variant}` suffix).
    pub legacy_only: bool,
    /// Only delete entries whose newest mtime is older than this many days.
    pub older_than_days: Option<u64>,
    /// Never delete buckets for this repository root.
    pub keep_repo: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct CacheCleanReport {
    pub listed: Vec<CacheDirEntry>,
    pub removed: Vec<PathBuf>,
    pub skipped_keep: Vec<PathBuf>,
    pub bytes_freed: u64,
}

/// List (and optionally remove) cache buckets under the Livewalk cache root.
pub fn clean_cache_dirs(opts: &CacheCleanOptions) -> std::io::Result<CacheCleanReport> {
    let mut report = CacheCleanReport::default();
    let Some(root) = livewalk_cache_root() else {
        return Ok(report);
    };
    if !root.is_dir() {
        return Ok(report);
    }

    let keep_key = opts.keep_repo.as_deref().map(repo_cache_key);
    let mut names: Vec<String> = std::fs::read_dir(&root)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    let superseded_legacy = legacy_superseded_names(&names);

    for name in &names {
        let path = root.join(name);
        let size_bytes = dir_size_bytes(&path).unwrap_or(0);
        let modified = dir_newest_mtime(&path);
        let legacy_superseded = superseded_legacy.contains(name.as_str());
        report.listed.push(CacheDirEntry {
            name: name.clone(),
            path: path.clone(),
            size_bytes,
            modified,
            legacy_superseded,
        });
    }

    let cutoff = opts.older_than_days.map(|days| {
        std::time::SystemTime::now()
            - std::time::Duration::from_secs(days.saturating_mul(86_400))
    });

    if !opts.yes {
        return Ok(report);
    }

    for entry in &report.listed {
        if should_keep(entry, keep_key) {
            report.skipped_keep.push(entry.path.clone());
            continue;
        }

        let remove = if opts.legacy_only {
            is_legacy_cache_dir_name(&entry.name)
        } else if let Some(cutoff) = cutoff {
            entry
                .modified
                .is_some_and(|m| m < cutoff)
                || (entry.modified.is_none() && opts.yes)
        } else {
            true
        };

        if remove {
            let size = entry.size_bytes;
            std::fs::remove_dir_all(&entry.path)?;
            report.removed.push(entry.path.clone());
            report.bytes_freed = report.bytes_freed.saturating_add(size);
        }
    }

    Ok(report)
}

fn should_keep(entry: &CacheDirEntry, keep_key: Option<u64>) -> bool {
    let Some(key) = keep_key else {
        return false;
    };
    let prefix = format!("{key:016x}");
    entry.name == prefix || entry.name.starts_with(&format!("{prefix}-"))
}

/// Legacy buckets are 16 lowercase hex digits with no `-{variant}` suffix.
pub fn is_legacy_cache_dir_name(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn legacy_superseded_names(names: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for name in names {
        if !is_legacy_cache_dir_name(name) {
            continue;
        }
        let prefix = format!("{name}-");
        if names.iter().any(|n| n.starts_with(&prefix)) {
            out.insert(name.clone());
        }
    }
    out
}

fn dir_size_bytes(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in std::fs::read_dir(&p)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

fn dir_newest_mtime(path: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(meta) = std::fs::metadata(&p) {
            newest = Some(match newest {
                Some(n) => n.max(meta.modified().unwrap_or(n)),
                None => meta.modified().ok()?,
            });
        }
        let Ok(read) = std::fs::read_dir(&p) else {
            continue;
        };
        for entry in read.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(entry.path());
            } else if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
                newest = Some(match newest {
                    Some(n) => n.max(m),
                    None => m,
                });
            }
        }
    }
    newest
}

#[cfg(test)]
pub(crate) static CACHE_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) struct CacheHomeGuard {
    old: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl CacheHomeGuard {
    pub(crate) fn set(path: &Path) -> Self {
        let old = std::env::var_os(CACHE_HOME_ENV);
        std::env::set_var(CACHE_HOME_ENV, path);
        CacheHomeGuard { old }
    }
}

#[cfg(test)]
impl Drop for CacheHomeGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => std::env::set_var(CACHE_HOME_ENV, v),
            None => std::env::remove_var(CACHE_HOME_ENV),
        }
    }
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
        let f = cache_file(Path::new("/repo"), ".livewalk-cache", "rust").unwrap();
        assert!(f.ends_with("fragments.json"));
        assert!(f.starts_with("/repo/.livewalk-cache/rust"));
    }

    #[test]
    fn default_dir_lives_under_cache_home_not_repo() {
        let repo = Path::new("/tmp/some-analyzed-project");
        let f = cache_file(repo, "", "rust").unwrap();
        let home = cache_home().expect("cache home");
        assert!(
            f.starts_with(home.join("livewalk")),
            "expected {:?} under {:?}/livewalk",
            f,
            home
        );
        assert!(
            f.to_string_lossy().contains("-rust/"),
            "cache dir should include language variant"
        );
        assert!(
            !f.starts_with(repo),
            "default cache must not live inside the analyzed tree"
        );
    }

    #[test]
    fn different_languages_use_distinct_cache_dirs() {
        let repo = Path::new("/tmp/multi-lang-repo");
        let rust = repo_cache_dir(repo, "", "rust").unwrap();
        let cpp = repo_cache_dir(repo, "", "cpp").unwrap();
        assert_ne!(rust, cpp);
        assert!(rust.to_string_lossy().ends_with("-rust"));
        assert!(cpp.to_string_lossy().ends_with("-cpp"));
    }

    #[test]
    fn whitespace_only_dir_uses_cache_home() {
        let repo = Path::new("/tmp/another-repo");
        let f = cache_file(repo, "  \t  ", "go").unwrap();
        let home = cache_home().expect("cache home");
        assert!(f.starts_with(home.join("livewalk")));
    }

    #[test]
    fn legacy_name_detected_and_superseded() {
        assert!(is_legacy_cache_dir_name("9a156570a2843030"));
        assert!(!is_legacy_cache_dir_name("9a156570a2843030-rust"));
        let names = vec![
            "9a156570a2843030".into(),
            "9a156570a2843030-rust".into(),
            "deadbeef00000000".into(),
        ];
        let sup = legacy_superseded_names(&names);
        assert!(sup.contains("9a156570a2843030"));
        assert!(!sup.contains("deadbeef00000000"));
    }

    #[test]
    fn clean_cache_dirs_requires_yes_to_remove() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let _lock = CACHE_HOME_TEST_LOCK.lock().unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "lcw-cache-clean-{}-{nanos}",
            std::process::id()
        ));
        let root = base.join("livewalk");
        let bucket = root.join("deadbeef00000000-rust");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join("fragments.json"), b"{}").unwrap();

        let _guard = CacheHomeGuard::set(&base);

        let list = clean_cache_dirs(&CacheCleanOptions::default()).unwrap();
        assert_eq!(list.listed.len(), 1);
        assert!(list.removed.is_empty());
        assert!(bucket.is_dir());

        let wiped = clean_cache_dirs(&CacheCleanOptions {
            yes: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(wiped.removed.len(), 1);
        assert!(!bucket.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_repo_cache_dir_creates_missing_directory() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "lcw-ensure-cache-{}-{nanos}",
            std::process::id()
        ));
        let repo = base.join("repo");
        let cache_dir = base.join("cache-root");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(!cache_dir.exists(), "precondition: cache dir must not exist yet");
        ensure_repo_cache_dir(&repo, &cache_dir.to_string_lossy(), "rust").expect("create cache dir");
        assert!(cache_dir.join("rust").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }
}
