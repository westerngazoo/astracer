//! A source file handed to a [`crate::adapter::LanguageAdapter`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

/// One line of a [`SourceSnippet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLine {
    pub number: u32,
    pub text: String,
}

/// A slice of source read from disk, with the node's span highlighted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnippet {
    pub file: String,
    /// First line returned (1-based).
    pub start_line: u32,
    /// Last line returned (1-based, inclusive).
    pub end_line: u32,
    /// The node's span inside the returned slice (1-based, inclusive).
    pub highlight_start: u32,
    pub highlight_end: u32,
    pub lines: Vec<SourceLine>,
}

/// Read lines from `path` around the node's `[start, end]` span (1-based,
/// inclusive). `context` pads before/after; the result is capped at `max_lines`
/// (centered on the highlight when clipped). Returns `None` when the file is
/// missing or the range is invalid.
pub fn read_snippet(
    path: &str,
    start: u32,
    end: u32,
    context: u32,
    max_lines: u32,
) -> Option<SourceSnippet> {
    if path.is_empty() || start == 0 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let file_lines: Vec<&str> = text.lines().collect();
    if file_lines.is_empty() {
        return None;
    }

    let hl_start = (start as usize).min(file_lines.len()).max(1);
    let hl_end = (end.max(start) as usize).min(file_lines.len()).max(hl_start);

    let mut lo = hl_start.saturating_sub(context as usize).max(1);
    let mut hi = (hl_end + context as usize).min(file_lines.len());
    let span = hi - lo + 1;
    if span > max_lines as usize {
        let mid = (hl_start + hl_end) / 2;
        let half = max_lines as usize / 2;
        lo = mid.saturating_sub(half).max(1);
        hi = (lo + max_lines as usize - 1).min(file_lines.len());
    }

    let lines = file_lines[lo - 1..hi]
        .iter()
        .enumerate()
        .map(|(i, &text)| SourceLine {
            number: lo as u32 + i as u32,
            text: text.to_string(),
        })
        .collect();

    Some(SourceSnippet {
        file: path.to_string(),
        start_line: lo as u32,
        end_line: hi as u32,
        highlight_start: hl_start as u32,
        highlight_end: hl_end as u32,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_snippet_returns_highlighted_range() {
        let dir = std::env::temp_dir().join("lcw_snippet_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "// header").unwrap();
            writeln!(f, "fn one() {{}}").unwrap();
            writeln!(f, "fn two() {{").unwrap();
            writeln!(f, "    println!(\"hi\");").unwrap();
            writeln!(f, "}}").unwrap();
            writeln!(f, "// footer").unwrap();
        }

        let snip = read_snippet(path.to_str().unwrap(), 3, 5, 1, 80).unwrap();
        assert_eq!(snip.highlight_start, 3);
        assert_eq!(snip.highlight_end, 5);
        assert!(snip.start_line <= 2);
        assert!(snip.end_line >= 6);
        assert_eq!(snip.lines[1].text, "fn two() {");
    }

    #[test]
    fn read_snippet_caps_long_files() {
        let dir = std::env::temp_dir().join("lcw_snippet_cap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 1..=200 {
                writeln!(f, "// line {i}").unwrap();
            }
        }

        let snip = read_snippet(path.to_str().unwrap(), 100, 150, 5, 20).unwrap();
        assert_eq!(snip.lines.len(), 20);
        assert_eq!(snip.highlight_start, 100);
        assert_eq!(snip.highlight_end, 150);
    }
}
