//! Discover which fast-mode languages appear in a repository (by file extension).

use std::collections::HashSet;
use std::path::Path;

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use lcw_config::{Config, Language};

use crate::discover::DEFAULT_EXCLUDES;
use crate::EngineError;

/// Map a file extension to a supported fast-mode language, if any.
fn extension_to_language(ext: &str) -> Option<Language> {
    match ext {
        "rs" => Some(Language::Rust),
        "ts" | "tsx" | "mts" | "cts" => Some(Language::Typescript),
        "py" | "pyi" => Some(Language::Python),
        "go" => Some(Language::Go),
        "cpp" | "cc" | "cxx" | "c++" | "h" | "hpp" | "hh" | "hxx" => Some(Language::Cpp),
        _ => None,
    }
}

/// Scan `root` for source files and return the distinct languages found, in
/// [`Language::ALL`] order. Does not read `livewalk.toml`; the UI uses this to
/// offer a language picker without editing config.
pub fn discover_languages(root: &Path) -> Result<Vec<Language>, EngineError> {
    let config = Config::default();
    let mut overrides = OverrideBuilder::new(root);
    for pat in DEFAULT_EXCLUDES {
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

    let mut found = HashSet::new();
    for result in builder.build() {
        let entry = result?;
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
            if let Some(lang) = extension_to_language(ext) {
                found.insert(lang);
            }
        }
    }

    Ok(Language::ALL
        .into_iter()
        .filter(|l| found.contains(l))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_rust_and_cpp_in_same_folder() {
        let root = std::env::temp_dir().join(format!(
            "lcw-discover-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/native.cpp"), "int main() {}").unwrap();

        let langs = discover_languages(&root).unwrap();
        assert_eq!(langs, vec![Language::Rust, Language::Cpp]);

        let _ = fs::remove_dir_all(&root);
    }
}
