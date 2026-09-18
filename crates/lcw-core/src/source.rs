//! A source file handed to a [`crate::adapter::LanguageAdapter`].

use std::path::{Path, PathBuf};

/// An in-memory source file: its path plus full text. Adapters parse a whole
/// batch at once so cross-file call resolution is possible.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        SourceFile {
            path: path.into(),
            text: text.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
