//! Source-file discovery: walk a repository, honoring `.gitignore` and the
//! config's extra excludes, and read the files an adapter can handle.

use std::path::{Path, PathBuf};

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use lcw_config::Config;
use lcw_core::SourceFile;

use crate::EngineError;

/// Directories we always skip, even if a repo doesn't `.gitignore` them.
const DEFAULT_EXCLUDES: &[&str] = &["target", "node_modules", "dist", ".git"];

/// Find all files under `root` whose extension is in `extensions`.
pub fn discover_files(
    root: &Path,
    config: &Config,
    extensions: &[&str],
) -> Result<Vec<PathBuf>, EngineError> {
    let mut overrides = OverrideBuilder::new(root);
    for pat in DEFAULT_EXCLUDES {
        // A leading `!` marks an *ignore* glob (exclude) without turning the
        // override set into a whitelist.
        overrides
            .add(&format!("!{pat}"))
            .map_err(|e| EngineError::Config(format!("bad default exclude '{pat}': {e}")))?;
        overrides
            .add(&format!("!**/{pat}/**"))
            .map_err(|e| EngineError::Config(format!("bad default exclude '{pat}': {e}")))?;
    }
    for pat in &config.adapter.exclude {
        overrides
            .add(&format!("!{pat}"))
            .map_err(|e| EngineError::Config(format!("bad exclude pattern '{pat}': {e}")))?;
    }
    let overrides = overrides
        .build()
        .map_err(|e| EngineError::Config(format!("building excludes: {e}")))?;

    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(config.adapter.follow_symlinks)
        .overrides(overrides);

    let mut files = Vec::new();
    for result in builder.build() {
        let entry = result?;
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(&ext) {
                files.push(path.to_path_buf());
            }
        }
    }
    // Deterministic order => reproducible graphs and golden tests.
    files.sort();
    Ok(files)
}

/// Read discovered files into memory, skipping (with a warning) any that are
/// not valid UTF-8 rather than aborting the whole run (best-effort parsing).
pub fn read_sources(files: &[PathBuf]) -> Vec<SourceFile> {
    let mut sources = Vec::with_capacity(files.len());
    for path in files {
        match std::fs::read_to_string(path) {
            Ok(text) => sources.push(SourceFile::new(path.clone(), text)),
            Err(e) => {
                lcw_telemetry::warn!(target: "lcw::engine", path = %path.display(), error = %e, "skipping unreadable file");
                lcw_telemetry::incr("engine.files_skipped");
            }
        }
    }
    sources
}
